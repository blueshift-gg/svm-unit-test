//! Lazy SBPF compilation triggered by `#[svm_test]`.
//!
//! On the first test in a suite, this generates one tiny `#![no_std]` cdylib
//! crate per test, shells out to `cargo build-sbf`, and copies the resulting
//! `.so` into a content-addressed directory. Subsequent process runs hit the
//! cache.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

/// Build all programs in the suite if not already cached. Returns the
/// directory containing `<name>.so` files.
///
/// `target_tmpdir` is `Some(env!("CARGO_TARGET_TMPDIR"))` for integration
/// tests; falls back to `<manifest_dir>/target/svm-unit-tests/` otherwise.
pub fn build_suite(
    source: &str,
    names: &[&str],
    target_tmpdir: Option<&str>,
    manifest_dir: &str,
    pkg_name: &str,
) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    for n in names {
        n.hash(&mut hasher);
    }
    pkg_name.hash(&mut hasher);
    let suite_hash = format!("{:016x}", hasher.finish());

    let workspace_root = match target_tmpdir {
        Some(t) => PathBuf::from(t),
        None => PathBuf::from(manifest_dir).join("target").join("svm-unit-tests"),
    };
    let work = workspace_root.join(format!("suite-{suite_hash}"));
    let so_dir = work.join("so");
    let done = work.join(".done");

    // The .done marker means "we successfully built every test in this suite
    // for this exact source/names tuple before". We still let `cargo build-sbf`
    // do its own incremental work below if the marker is missing — this short
    // circuit just avoids re-running cargo when nothing in the suite changed.
    if done.exists() && names.iter().all(|n| so_dir.join(format!("{n}.so")).exists()) {
        return so_dir;
    }

    fs::create_dir_all(&so_dir).expect("create so dir");
    let build_dir = work.join("build");
    let pkg_path = PathBuf::from(manifest_dir);

    for &name in names {
        build_one(&build_dir, &so_dir, name, source, &pkg_path, pkg_name);
    }

    // Mark the suite ready. Best-effort across processes — we don't take a
    // file lock; cargo's own target locking serializes the actual builds.
    fs::write(&done, "").ok();
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
    fs::write(crate_dir.join("Cargo.toml"), cargo_toml).expect("write Cargo.toml");

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
    fs::write(crate_dir.join("src").join("lib.rs"), lib_rs).expect("write lib.rs");

    let manifest = crate_dir.join("Cargo.toml");
    let target_dir = crate_dir.join("target");

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
    // Idempotent: a parallel cargo-test process may have already renamed it.
    let produced = so_dir.join(format!("{crate_name}.so"));
    let wanted = so_dir.join(format!("{name}.so"));
    if produced != wanted && produced.exists() {
        fs::rename(&produced, &wanted).unwrap_or_else(|e| {
            if wanted.exists() {
                // Lost a race; either side has a valid .so.
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
