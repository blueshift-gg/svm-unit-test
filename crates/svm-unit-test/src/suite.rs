//! Per-file suite discovery.
//!
//! The macro hands us `file!()` (a path that's typically relative to the
//! workspace root) plus the package's `CARGO_MANIFEST_DIR`. We walk up from
//! the manifest dir until we find the file, read it, parse it once per
//! process to discover every `#[svm_test]` sibling, then drive
//! [`crate::build_suite`] to compile one `.so` per test fn.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use quote::ToTokens;

/// Build (or return the cached path of) all SBPF programs for the file the
/// `#[svm_test]` macro was expanded in.
///
/// Thread-safe: a per-file `OnceLock` ensures the build runs exactly once
/// per process even when many sibling `#[test]`s race in parallel; later
/// callers block on the same `OnceLock` and reuse its result.
pub fn ensure_suite_built(
    file: &'static str,
    manifest_dir: &str,
    pkg_name: &str,
    target_tmpdir: Option<&str>,
) -> &'static Path {
    type SuiteCell = OnceLock<&'static Path>;
    static REGISTRY: OnceLock<Mutex<HashMap<&'static str, Arc<SuiteCell>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));

    let cell: Arc<SuiteCell> = {
        let mut guard = registry.lock().expect("suite registry poisoned");
        guard
            .entry(file)
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    };

    *cell.get_or_init(|| {
        let abs = resolve_source_path(manifest_dir, file);
        let source = fs::read_to_string(&abs)
            .unwrap_or_else(|e| panic!("svm_test: read {}: {e}", abs.display()));

        let (cleaned_source, names) = parse_suite(&source)
            .unwrap_or_else(|e| panic!("svm_test: parse {}: {e}", abs.display()));
        let names_refs: Vec<&str> = names.iter().map(String::as_str).collect();

        let path = crate::build_suite(
            &cleaned_source,
            &names_refs,
            target_tmpdir,
            manifest_dir,
            pkg_name,
        );
        Box::leak(path.into_boxed_path())
    })
}

/// `file!()` returns a path relative to whatever directory cargo invoked
/// rustc from — workspace root for workspace members, package root for
/// standalone packages, sometimes absolute. Walk up from `CARGO_MANIFEST_DIR`
/// until we find a directory where `<dir>/<file>` exists.
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

/// Parse the raw test source. Returns:
///   * the source string handed to `cargo build-sbf` — same items, with the
///     `#[svm_test]` attribute stripped from each test fn and any
///     `use svm_unit_test::*;` removed (the SBPF crate doesn't depend on us);
///   * the names of the discovered `#[svm_test]` fns.
fn parse_suite(source: &str) -> Result<(String, Vec<String>), syn::Error> {
    let file: syn::File = syn::parse_str(source)?;
    let mut out_items: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    for item in &file.items {
        match item {
            syn::Item::Use(u) => {
                if !is_crate_use(u) {
                    out_items.push(u.to_token_stream().to_string());
                }
            }
            syn::Item::Fn(f) => {
                let mut clone = f.clone();
                if let Some(idx) = clone.attrs.iter().position(is_svm_test_attr) {
                    names.push(f.sig.ident.to_string());
                    clone.attrs.remove(idx);
                }
                out_items.push(clone.to_token_stream().to_string());
            }
            other => out_items.push(other.to_token_stream().to_string()),
        }
    }

    Ok((out_items.join("\n"), names))
}

fn is_svm_test_attr(a: &syn::Attribute) -> bool {
    let p = a.path();
    if p.is_ident("svm_test") {
        return true;
    }
    p.segments
        .last()
        .map(|s| s.ident == "svm_test")
        .unwrap_or(false)
}

/// Filter `use svm_unit_test::...;` (and similar) out of the SBPF source —
/// the generated SBPF crate doesn't depend on `svm-unit-test`. Match by the
/// crate name (the first ident of the use tree), not the macro name.
fn is_crate_use(u: &syn::ItemUse) -> bool {
    fn first_ident(tree: &syn::UseTree) -> Option<String> {
        match tree {
            syn::UseTree::Path(p) => Some(p.ident.to_string()),
            syn::UseTree::Name(n) => Some(n.ident.to_string()),
            syn::UseTree::Rename(r) => Some(r.ident.to_string()),
            _ => None,
        }
    }
    matches!(first_ident(&u.tree).as_deref(), Some("svm_unit_test"))
}
