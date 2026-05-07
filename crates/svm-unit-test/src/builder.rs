//! Lazy SBPF compilation triggered by `#[svm_test]`.
//!
//! On the first test in a process, this generates one tiny `#![no_std]`
//! cdylib crate per test, shells out to `cargo build-sbf`, and copies the
//! resulting `.so` into a stable per-suite directory. We do **not** content-
//! cache: every fresh `cargo test` re-invokes `cargo build-sbf`, which uses
//! its own incremental fingerprinting to detect changes in the test file or
//! any path dependency (including the user's lib). That way edits to the
//! underlying code always show up in the next test run instead of getting
//! masked by a stale `.so`.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

/// Build all programs in the suite and return the directory holding
/// `<name>.so`.
///
/// `file` (the test file's `file!()` value) keys the suite directory so two
/// integration test files in the same package don't trample each other's
/// build artifacts. The directory itself is stable across `cargo test`
/// invocations, so cargo's incremental cache inside it stays warm.
///
/// `target_tmpdir` is `Some(env!("CARGO_TARGET_TMPDIR"))` for integration
/// tests; falls back to `<manifest_dir>/target/svm-unit-tests/` otherwise.
pub fn build_suite(
    source: &str,
    names: &[&str],
    file: &str,
    target_tmpdir: Option<&str>,
    manifest_dir: &str,
    pkg_name: &str,
) -> PathBuf {
    // Hash the *file path*, not the source — keeps the dir stable across
    // edits so cargo can do an incremental rebuild rather than starting
    // from scratch each time.
    let mut hasher = DefaultHasher::new();
    file.hash(&mut hasher);
    let suite_id = format!("{:016x}", hasher.finish());

    let workspace_root = match target_tmpdir {
        Some(t) => PathBuf::from(t),
        None => PathBuf::from(manifest_dir).join("target").join("svm-unit-tests"),
    };
    let work = workspace_root.join(format!("suite-{suite_id}"));
    let so_dir = work.join("so");
    let build_dir = work.join("build");
    fs::create_dir_all(&so_dir).expect("create so dir");

    let pkg_path = PathBuf::from(manifest_dir);
    for &name in names {
        build_one(&build_dir, &so_dir, name, source, &pkg_path, pkg_name);
    }

    so_dir
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

    // Write the manifest only if it would change — otherwise we touch its
    // mtime on every test run and force cargo to re-evaluate fingerprints.
    let cargo_toml = format!(
        r#"[package]
name = "{crate_name}"
version = "0.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "lib"]

[dependencies]
{pkg_name} = {{ path = "{rel_pkg}" }}

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

#[panic_handler]
fn _svm_test_panic(_: &core::panic::PanicInfo) -> ! {{ loop {{}} }}

{source}

#[unsafe(no_mangle)]
pub extern "C" fn entrypoint(_input: *mut u8) -> u64 {{
    {name}();
    0
}}
"#
    );
    write_if_changed(&crate_dir.join("src").join("lib.rs"), &lib_rs);

    let manifest = crate_dir.join("Cargo.toml");
    let target_dir = crate_dir.join("target");

    // Always invoke build-sbf — cargo's own fingerprinting decides whether
    // to actually recompile. For an unchanged test, this is a sub-second
    // no-op; for a changed lib path-dep, it rebuilds.
    let status = Command::new("cargo")
        .arg("build-sbf")
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
    let produced = so_dir.join(format!("{crate_name}.so"));
    let wanted = so_dir.join(format!("{name}.so"));
    if produced != wanted && produced.exists() {
        // fs::rename replaces the destination on Unix, which is what we
        // want — the new build supersedes any stale prior artifact.
        fs::rename(&produced, &wanted).unwrap_or_else(|e| {
            if wanted.exists() {
                // Lost a race with a parallel cargo-test process; either
                // side's .so is valid. Drop the duplicate.
                let _ = fs::remove_file(&produced);
            } else {
                panic!(
                    "rename {} -> {}: {e}",
                    produced.display(),
                    wanted.display(),
                );
            }
        });
    }
    assert!(
        wanted.exists(),
        "expected `{}` to exist after build-sbf",
        wanted.display(),
    );
}

/// Write `contents` to `path` only if the file is missing or its current
/// contents differ. Avoids bumping mtime on no-op runs, which would defeat
/// `cargo build-sbf`'s fingerprinting.
fn write_if_changed(path: &Path, contents: &str) {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == contents {
            return;
        }
    }
    fs::write(path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
