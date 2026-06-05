use core::hint::black_box;
use curve_bench::{Curve, SCALAR_A, SCALAR_B};
use svm_unit_test::{ProgramError, svm_test};

// Passes as long as the program does *not* succeed — here the body panics,
// which aborts the SBPF program. A bare `error` is equivalent to `fail`.
#[svm_test(error)]
fn add_mod_n_panics() {
    let xs = black_box([1u64, 2, 3]);
    black_box(xs[black_box(7)]);
}

// Passes only if the program fails with exactly this error. A body returning
// a `u64` makes the SBPF program exit with that code; `6` maps to
// `ProgramError::Custom(6)`.
#[svm_test(error = ProgramError::Custom(6))]
fn add_mod_n_custom_error() -> u64 {
    Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B));
    6
}

#[svm_test]
fn add_mod_n() {
    Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B));
}

#[svm_test]
fn add_mod_n_zero() {
    const ZERO: [u64; 4] = [5; 4];
    Curve::add_mod_n(black_box(&SCALAR_A), black_box(&ZERO));
}

// Plain (non-#[svm_test]) helpers stay available to test bodies but don't
// run as tests themselves.
#[allow(dead_code)]
fn _double(x: u64) -> u64 {
    x.wrapping_mul(2)
}
