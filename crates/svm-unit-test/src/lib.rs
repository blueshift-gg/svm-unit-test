//! Runtime side of the `svm-unit-test` framework.
//!
//! Re-exports [`svm_test`] / [`svm_harness`] and provides:
//!  * [`ensure_test_built`] — called by the macro on first invocation
//!    of each `#[svm_test]` / `#[svm_harness]`; lazily compiles **just
//!    that one test's** SBPF program and returns the path to its `.so`.
//!    Sibling tests aren't touched unless they're also run.
//!  * [`run`] — loads a compiled ELF into Mollusk and reports CU usage.
//!  * [`run_harness`] — same, but pushes user-provided bytes as
//!    instruction data. 
//!
//! ```ignore
//! use svm_unit_test::{svm_test, svm_harness};
//! use core::hint::black_box;
//! use my_lib::{Curve, SCALAR_A, SCALAR_B};
//!
//! #[svm_test]
//! fn add_mod_n() {
//!     black_box(Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B)));
//! }
//!
//! #[repr(C)] struct AddInputs { a: [u64; 4], b: [u64; 4] }
//!
//! #[svm_harness]
//! fn add_mod_n_harness(input: &AddInputs) {
//!     black_box(Curve::add_mod_n(black_box(&input.a), black_box(&input.b)));
//! }
//! ```

// Re-export so we can use as svm_unit_test::*;
pub use svm_unit_test_types::{TYPES_CRATE_DIR, svm_harness, svm_test};

pub(crate) mod builder;
mod suite;
pub use suite::ensure_test_built;

use mollusk_svm::{Mollusk, program::loader_keys::LOADER_V3};
use solana_instruction::Instruction;
use solana_address::Address;

#[derive(Debug, Clone)]
pub struct RunReport {
    pub name: String,
    pub compute_units: u64,
    pub execution_time_us: u64,
}

/// Load `elf` into a fresh Mollusk instance, invoke the entrypoint with empty
/// instruction data, and report compute units consumed. Panics on program
/// failure.
pub fn run(name: &str, elf: &[u8]) -> RunReport {
    run_with_ix_data(name, elf, &[])
}

/// Same as [`run`], but feeds `ix_data`. Used by
/// `#[svm_harness]` to push the test's input bytes into `r2` at program entry.
pub fn run_harness(name: &str, elf: &[u8], ix_data: &[u8]) -> RunReport {
    run_with_ix_data(name, elf, ix_data)
}

fn run_with_ix_data(name: &str, elf: &[u8], ix_data: &[u8]) -> RunReport {
    let program_id = Address::new_unique();

    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(&program_id, &LOADER_V3, elf);

    let instruction = Instruction::new_with_bytes(program_id, ix_data, vec![]);
    let result = mollusk.process_instruction(&instruction, &[]);

    if !result.program_result.is_ok() {
        panic!(
            "svm_test `{name}` program failed: {:?} (CUs consumed before failure: {})",
            result.program_result, result.compute_units_consumed,
        );
    }

    let report = RunReport {
        name: name.to_string(),
        compute_units: result.compute_units_consumed,
        execution_time_us: result.execution_time,
    };

    eprintln!(
        "svm_test `{}` => {} CUs ({}us)",
        report.name, report.compute_units, report.execution_time_us,
    );

    report
}
