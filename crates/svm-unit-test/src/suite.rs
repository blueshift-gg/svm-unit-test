//! Per-(file, test) discovery + lazy build dispatch.
//!
//! The macro hands us `file!()` (typically workspace-root-relative) plus the
//! specific test `name` and the package's cargo env. We:
//!   1. find the file by walking up from `CARGO_MANIFEST_DIR` until
//!      `<dir>/<file>` resolves;
//!   2. parse it once per process to extract every `#[svm_test]` /
//!      `#[svm_harness]` sibling, their input types (if any), and the
//!      surrounding `use`s/helpers (memoized per file);
//!   3. trigger [`crate::build_one_test`] for *just this test* (memoized per
//!      `(file, name)` pair so parallel sibling tests in the same process
//!      don't double-build).
//!
//! This means `cargo test add_mod_n` only ever compiles `add_mod_n.so`; its
//! sibling tests in the same file are never built unless they're also run.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use quote::ToTokens;

struct ParsedFile {
    cleaned_source: String,
    /// Map from fn name to its first parameter's type, rendered as token
    /// text (e.g. `"AddInputs"`, `"crate::foo::Bar"`). `None` means the fn
    /// takes no parameters — `#[svm_test]` shape. `Some(_)` means
    /// `#[svm_harness]` shape and the SBPF entrypoint must deserialize that type
    fn_inputs: HashMap<String, Option<String>>,
}

/// Build (or return the cached path of) the SBPF program for the
/// `#[svm_test]` named `name` defined in `file`. Returns the absolute path
/// to its `.so`.
///
/// Thread-safe: a per-`(file, name)` `OnceLock` ensures the build happens
/// exactly once per process even when many sibling `#[test]`s race in
/// parallel.
pub fn ensure_test_built(
    file: &'static str,
    name: &'static str,
    manifest_dir: &str,
    pkg_name: &str,
    target_tmpdir: Option<&str>,
) -> &'static Path {
    type Cell = OnceLock<&'static Path>;
    type Reg = HashMap<(&'static str, &'static str), Arc<Cell>>;
    static REGISTRY: OnceLock<Mutex<Reg>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));

    let cell: Arc<Cell> = {
        let mut guard = registry.lock().expect("svm_test registry poisoned");
        guard
            .entry((file, name))
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    };

    *cell.get_or_init(|| {
        let parsed = parse_file_cached(file, manifest_dir);
        let input_type = parsed
            .fn_inputs
            .get(name)
            .and_then(|t| t.as_deref());
        let path = crate::builder::build_one_test(
            &parsed.cleaned_source,
            name,
            file,
            target_tmpdir,
            manifest_dir,
            pkg_name,
            input_type,
        );
        Box::leak(path.into_boxed_path())
    })
}

/// Parse a test file once per process. Multiple `#[svm_test]` siblings in
/// the same file all share the result.
fn parse_file_cached(file: &'static str, manifest_dir: &str) -> &'static ParsedFile {
    type Cell = OnceLock<&'static ParsedFile>;
    type Reg = HashMap<&'static str, Arc<Cell>>;
    static CACHE: OnceLock<Mutex<Reg>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    let cell: Arc<Cell> = {
        let mut guard = cache.lock().expect("svm_test parse cache poisoned");
        guard
            .entry(file)
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    };

    *cell.get_or_init(|| {
        let abs = resolve_source_path(manifest_dir, file);
        let source = fs::read_to_string(&abs)
            .unwrap_or_else(|e| panic!("svm_test: read {}: {e}", abs.display()));
        let (cleaned_source, fn_inputs) = parse_suite(&source)
            .unwrap_or_else(|e| panic!("svm_test: parse {}: {e}", abs.display()));
        Box::leak(Box::new(ParsedFile { cleaned_source, fn_inputs }))
    })
}

/// Walk up from `CARGO_MANIFEST_DIR` until `<dir>/<file>` exists.
fn resolve_source_path(start_dir: &str, file: &str) -> PathBuf {
    let file_path = Path::new(file);
    if file_path.is_absolute() {
        if file_path.exists() {
            return file_path.to_path_buf();
        }
        panic!("svm_test: absolute file path `{file}` does not exist");
    }

    let mut dir = PathBuf::from(start_dir);
    loop {
        let candidate = dir.join(file);
        if candidate.exists() {
            return candidate;
        }
        if !dir.pop() {
            panic!(
                "svm_test: could not locate source file `{file}` starting from `{start_dir}` (walked up to filesystem root)"
            );
        }
    }
}

/// Build the source string handed to `cargo build-sbf` and collect each
/// fn's first-param type for the entrypoint codegen.
///
/// `#[svm_test]` / `#[svm_harness]` attributes are stripped (the SBPF
/// crate doesn't depend on the proc-macro crate, but the user's source
/// is shared between host and SBPF compilation units).
fn parse_suite(
    source: &str,
) -> Result<(String, HashMap<String, Option<String>>), syn::Error> {
    let file: syn::File = syn::parse_str(source)?;
    let mut out_items: Vec<String> = Vec::new();
    let mut fn_inputs: HashMap<String, Option<String>> = HashMap::new();

    for item in &file.items {
        match item {
            syn::Item::Use(u) => {
                out_items.push(u.to_token_stream().to_string());
            }
            syn::Item::Fn(f) => {
                let mut clone = f.clone();
                clone.attrs.retain(|a| !is_svm_attr(a));

                fn_inputs.insert(
                    f.sig.ident.to_string(),
                    first_param_type(&f.sig),
                );

                out_items.push(clone.to_token_stream().to_string());
            }
            other => out_items.push(other.to_token_stream().to_string()),
        }
    }

    Ok((out_items.join("\n"), fn_inputs))
}

fn is_svm_attr(a: &syn::Attribute) -> bool {
    a.path()
        .segments
        .last()
        .map(|s| s.ident == "svm_test" || s.ident == "svm_harness")
        .unwrap_or(false)
}

/// Render the first parameter's type as token text, or `None` if the fn
/// takes no params (edge case if the first arg is a `self` receiver — shouldn't
/// happen for free fns but treat conservatively).
fn first_param_type(sig: &syn::Signature) -> Option<String> {
    match sig.inputs.first()? {
        syn::FnArg::Typed(pat) => Some(pat.ty.to_token_stream().to_string()),
        syn::FnArg::Receiver(_) => None,
    }
}

