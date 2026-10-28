//! Lazy SBPF compilation triggered by `#[svm_test]`.
//!
//! Each test gets its own `#![no_std]` cdylib crate generated under
//! `target/tmp/suite-<hash-of-file-path>/build/<test-name>/`. Before
//! shelling out to `cargo build-sbf` we stat-compare the existing `.so`
//! against the generated lib.rs/Cargo.toml plus the user's package source
//! tree — if nothing is newer than the `.so`, the spawn is skipped entirely.
//! This keeps the no-op path on the order of milliseconds rather than
//! hundreds.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

/// Build (if needed) and return the path to `<name>.so` for the given
/// `#[svm_test]` fn. Idempotent and concurrency-safe; the caller (suite.rs)
/// further memoizes per `(file, name)` so this is invoked at most once per
/// test per process.
pub fn build_one_test(
    source: &str,
    name: &str,
    file: &str,
    target_tmpdir: Option<&str>,
    manifest_dir: &str,
    pkg_name: &str,
) -> PathBuf {
    // Hash the *file path*, not the source — the suite directory needs to
    // be stable across cargo runs so cargo's incremental fingerprinting
    // inside it stays warm.
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    let suite_id = format!("{:016x}", hasher.finish());

    let workspace_root = match target_tmpdir {
        Some(t) => PathBuf::from(t),
        None => PathBuf::from(manifest_dir)
            .join("target")
            .join("svm-unit-tests"),
    };
    let work = workspace_root.join(format!("suite-{suite_id}"));
    let so_dir = work.join("so");
    let build_dir = work.join("build");
    fs::create_dir_all(&so_dir).expect("create so dir");

    let pkg_path = PathBuf::from(manifest_dir);
    build_one(&build_dir, &so_dir, name, source, &pkg_path, pkg_name);

    so_dir.join(format!("{name}.so"))
}

fn build_one(
    build_dir: &Path,
    so_dir: &Path,
    name: &str,
    source: &str,
    pkg_path: &Path,
    pkg_name: &str,
) {
    let crate_name = format!("svm_test_{name}");
    let crate_dir = build_dir.join(name);
    fs::create_dir_all(crate_dir.join("src")).expect("create per-test crate dir");

    let rel_pkg = pathdiff::diff_paths(pkg_path, &crate_dir)
        .expect("relative path to user package")
        .display()
        .to_string()
        .replace('\\', "/");

    let cargo_toml = format!(
        r#"[package]
name = "{crate_name}"
version = "0.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "lib"]

[dependencies]
{pkg_name} = {{ path = "{rel_pkg}" }}
solana-program-error = {{ version = "3", default-features = false }}

[workspace]

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
overflow-checks = false
panic = "abort"
strip = "symbols"
"#
    );
    write_if_changed(&crate_dir.join("Cargo.toml"), &cargo_toml);

    let lib_rs = format!(
        r#"#![no_std]
#![allow(unused_imports, dead_code)]

const HEAP_START_ADDRESS: usize = 0x3000_0000;
const HEAP_LENGTH: usize = 32 * 1024;

struct SvmTestBumpAlloc {{
    start: usize,
    len: usize,
}}

impl SvmTestBumpAlloc {{
    const unsafe fn with_fixed_address_range(start: usize, len: usize) -> Self {{
        Self {{ start, len }}
    }}
}}

unsafe impl core::alloc::GlobalAlloc for SvmTestBumpAlloc {{
    #[inline]
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {{
        let pos_ptr = self.start as *mut usize;
        let mut pos = unsafe {{ *pos_ptr }};
        if pos == 0 {{
            pos = self.start + self.len;
        }}
        pos = pos.saturating_sub(layout.size());
        pos &= !(layout.align().wrapping_sub(1));
        if pos < self.start + core::mem::size_of::<usize>() {{
            return core::ptr::null_mut();
        }}
        unsafe {{
            *pos_ptr = pos;
        }}
        pos as *mut u8
    }}

    #[inline]
    unsafe fn dealloc(&self, _: *mut u8, _: core::alloc::Layout) {{}}
}}

#[global_allocator]
static SVM_TEST_HEAP: SvmTestBumpAlloc = unsafe {{
    SvmTestBumpAlloc::with_fixed_address_range(HEAP_START_ADDRESS, HEAP_LENGTH)
}};

#[panic_handler]
fn _svm_test_panic(_: &core::panic::PanicInfo) -> ! {{ loop {{}} }}

// Lets a #[svm_test] body return `()` (→ success, code 0), a `u64` program
// return code, or a `Result<T, E: Into<ProgramError>>` — so an
// `#[svm_test(error = ...)]` body can just be the fallible op itself (`Ok` →
// success, `Err(e)` → its `ProgramError` code, which `error = <ProgramError>`
// matches). The `Into<ProgramError>` bound means any idiomatic Solana error
// type works with no extra impls.
trait _SvmTestReturnCode {{ fn _svm_test_return_code(self) -> u64; }}
impl _SvmTestReturnCode for () {{ fn _svm_test_return_code(self) -> u64 {{ 0 }} }}
impl _SvmTestReturnCode for u64 {{ fn _svm_test_return_code(self) -> u64 {{ self }} }}
impl<T, E: Into<solana_program_error::ProgramError>> _SvmTestReturnCode for Result<T, E> {{
    fn _svm_test_return_code(self) -> u64 {{
        match self {{
            Ok(_) => 0,
            Err(e) => {{
                let p: solana_program_error::ProgramError = e.into();
                p.into()
            }}
        }}
    }}
}}

{source}

#[unsafe(no_mangle)]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {{
    _SvmTestReturnCode::_svm_test_return_code({name}())
}}
"#
    );
    write_if_changed(&crate_dir.join("src").join("lib.rs"), &lib_rs);

    let so_path = so_dir.join(format!("{name}.so"));
    let target_dir = crate_dir.join("target");
    if !needs_rebuild(&so_path, &crate_dir, pkg_path, &target_dir, &crate_name) {
        return;
    }

    let manifest = crate_dir.join("Cargo.toml");

    let status = Command::new("cargo")
        .arg("build-sbf")
        // Emit an SBPF v3 program. `build-sbf` defaults to `--arch v0`; v3 is
        // the current on-chain target and is what Mollusk (>= 0.15.1) executes
        // under LOADER_V3.
        .arg("--arch")
        .arg("v3")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--sbf-out-dir")
        .arg(so_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("spawn `cargo build-sbf`");

    assert!(
        status.success(),
        "`cargo build-sbf` failed for svm_test `{name}` (manifest: {})",
        manifest.display(),
    );

    // build-sbf emits `<crate_name>.so`; the macro expects `<name>.so`.
    // fs::rename replaces the destination on Unix, which is what we want
    // — the new build supersedes any stale prior artifact.
    let produced = so_dir.join(format!("{crate_name}.so"));
    if produced != so_path && produced.exists() {
        fs::rename(&produced, &so_path).unwrap_or_else(|e| {
            if so_path.exists() {
                // Lost a race with a parallel cargo-test process; drop the
                // duplicate and use the existing one.
                let _ = fs::remove_file(&produced);
            } else {
                panic!(
                    "rename {} -> {}: {e}",
                    produced.display(),
                    so_path.display(),
                );
            }
        });
    }
    assert!(
        so_path.exists(),
        "expected `{}` to exist after build-sbf",
        so_path.display(),
    );
}

/// Decide whether to invoke `cargo build-sbf` at all. We approximate cargo's
/// own fingerprint check with mtimes:
///   * the `.so` must exist;
///   * the generated Cargo.toml + lib.rs must not be newer than it;
///   * the user's package Cargo.toml must not be newer than it;
///   * no file the last build read may be newer than it, or gone. Those are
///     the ones cargo lists in the dep-info (`<crate>.d`) beside the build: the
///     user's sources and every path dependency's (the lib under test, sibling
///     workspace crates), and files pulled in by `include_bytes!` and friends.
///
/// Still misses a dependency *version* change (a Cargo.lock bump in another
/// workspace member); for that, touch the test file or `cargo clean`. The
/// no-op path stays a few hundred stat() calls instead of a multi-hundred-ms
/// `cargo build-sbf` spawn.
fn needs_rebuild(
    so_path: &Path,
    crate_dir: &Path,
    pkg_path: &Path,
    target_dir: &Path,
    crate_name: &str,
) -> bool {
    let Some(so_mtime) = mtime(so_path) else {
        return true;
    };

    for f in [
        crate_dir.join("Cargo.toml"),
        crate_dir.join("src").join("lib.rs"),
        pkg_path.join("Cargo.toml"),
    ] {
        if mtime(&f).is_some_and(|m| m > so_mtime) {
            return true;
        }
    }

    // A build from before dep-info was read (or one that never finished):
    // rebuild once rather than guess.
    let Some(inputs) = build_inputs(target_dir, crate_name) else {
        return true;
    };
    inputs.iter().any(|f| mtime(f).is_none_or(|m| m > so_mtime))
}

/// The files the last build of `crate_name` read, from the dep-info cargo
/// writes at `<target>/<triple>/release/<crate_name>.d`.
fn build_inputs(target_dir: &Path, crate_name: &str) -> Option<Vec<PathBuf>> {
    for triple in fs::read_dir(target_dir).ok()?.flatten() {
        let d = triple
            .path()
            .join("release")
            .join(format!("{crate_name}.d"));
        if let Ok(text) = fs::read_to_string(&d) {
            return Some(parse_dep_info(&text));
        }
    }
    None
}

/// The prerequisites of a Makefile-style dep-info file's first rule:
/// `out: a b\ c` gives `a` and `b c` (a space in a path is `\ `).
fn parse_dep_info(text: &str) -> Vec<PathBuf> {
    let line = text.lines().next().unwrap_or("");
    // The target is a path too and may contain `:` (`C:\…`); the separator
    // is the first `: `.
    let Some((_, deps)) = line.split_once(": ") else {
        return Vec::new();
    };
    let mut files = Vec::new();
    let mut cur = String::new();
    let mut chars = deps.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            ' ' => {
                if !cur.is_empty() {
                    files.push(PathBuf::from(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        files.push(PathBuf::from(cur));
    }
    files
}

fn mtime(p: &Path) -> Option<SystemTime> {
    p.metadata().ok()?.modified().ok()
}

/// Write `contents` to `path` only if it would change. Avoids bumping
/// mtime on no-op runs (which would defeat our `needs_rebuild` check).
fn write_if_changed(path: &Path, contents: &str) {
    if let Ok(existing) = fs::read_to_string(path)
        && existing == contents
    {
        return;
    }
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_info_lists_the_first_rules_prerequisites() {
        let text = "/t/x.so: /a/src/lib.rs /b/my\\ dir/f.bin /c/lib.rs\n\n/a/src/lib.rs:\n";
        assert_eq!(
            parse_dep_info(text),
            [
                PathBuf::from("/a/src/lib.rs"),
                PathBuf::from("/b/my dir/f.bin"),
                PathBuf::from("/c/lib.rs"),
            ]
        );
        assert!(parse_dep_info("").is_empty());
    }

    #[test]
    fn a_changed_path_dependency_rebuilds() {
        let dir = std::env::temp_dir().join(format!("svm-unit-test-dep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let crate_dir = dir.join("build/t");
        let release = crate_dir.join("target/sbpfv3-solana-solana/release");
        let pkg = dir.join("pkg");
        let dep = dir.join("dep/src/lib.rs");
        for d in [
            crate_dir.join("src"),
            release.clone(),
            pkg.clone(),
            dep.parent().unwrap().into(),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        for f in [
            crate_dir.join("Cargo.toml"),
            crate_dir.join("src/lib.rs"),
            pkg.join("Cargo.toml"),
        ] {
            fs::write(f, "").unwrap();
        }
        fs::write(&dep, "").unwrap();
        let so = dir.join("t.so");
        // What a build does: write the dep-info, then the .so.
        let build = || {
            fs::write(
                release.join("svm_test_t.d"),
                format!("{}: {}\n", so.display(), dep.display()),
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(&so, "").unwrap();
        };
        let stale = || {
            needs_rebuild(
                &so,
                &crate_dir,
                &pkg,
                &crate_dir.join("target"),
                "svm_test_t",
            )
        };

        assert!(stale(), "no .so yet");
        build();
        assert!(!stale(), "nothing changed");
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&dep, "changed").unwrap();
        assert!(stale(), "a path dependency changed");
        build();
        fs::remove_file(&dep).unwrap();
        assert!(stale(), "a file the build read is gone");
        fs::remove_dir_all(&dir).unwrap();
    }
}
