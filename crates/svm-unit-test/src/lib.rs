//! Runtime side of the `svm-unit-test` framework.
//!
//! Re-exports [`svm_test`] and provides:
//!  * [`ensure_suite_built`] — called by the macro on first test run; parses
//!    the test source to find every `#[svm_test]` sibling and triggers a
//!    one-shot build of the whole file's worth of SBPF programs.
//!  * [`build_suite`] — the lower-level builder used by `ensure_suite_built`.
//!  * [`run`] — loads a compiled ELF into Mollusk and reports CU usage.
//!
//! ```ignore
//! use svm_unit_test::svm_test;
//! use core::hint::black_box;
//! use my_lib::{Curve, SCALAR_A, SCALAR_B};
//!
//! #[svm_test]
//! fn add_mod_n() {
//!     black_box(Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B)));
//! }
//! ```

pub use svm_unit_test_macros::svm_test;

mod builder;
mod suite;
pub use builder::build_suite;
pub use suite::ensure_suite_built;

use mollusk_svm::{Mollusk, program::loader_keys::LOADER_V3};
use solana_instruction::Instruction;
use solana_pubkey::Pubkey;

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
    let program_id = Pubkey::new_unique();

    let mut mollusk = Mollusk::default();
    mollusk.add_program_with_loader_and_elf(&program_id, &LOADER_V3, elf);

    let instruction = Instruction::new_with_bytes(program_id, &[], vec![]);
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
