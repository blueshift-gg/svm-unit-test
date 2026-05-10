//! Internal `no_std` glue crate for the `svm-unit-test` framework.
//!
//! **End users do not depend on this crate directly.** They add
//! `svm-unit-test = "..."` like any other dep and write
//! `#[svm_unit_test::svm_test]` / `#[svm_unit_test::svm_harness]`. The
//! host crate (`svm-unit-test`) re-exports everything defined here.
//!
//! Why this crate exists: so we don't make users depend on unnecessary
//! crates that the framework needs internally, and avoid having to play
//! with feature flags.

#![no_std]

// Re-exported so the SBPF aliased dep — where this crate IS
// `svm_unit_test` via Cargo's `package` rename — exposes the same names
// (`svm_test`, `svm_harness`) the user already wrote in their source.
// `use svm_unit_test::svm_harness;` then resolves on both sides without
// the SBPF side needing its own dep on `svm-unit-test-macros`.
pub use svm_unit_test_macros::{svm_harness, svm_test};

/// Absolute path to this crate's source dir, captured at compile time.
/// The host-side builder forwards it into the generated SBPF crate's
/// `Cargo.toml` as the `path = "..."` of the package-renamed dep.
pub const TYPES_CRATE_DIR: &str = env!("CARGO_MANIFEST_DIR");
