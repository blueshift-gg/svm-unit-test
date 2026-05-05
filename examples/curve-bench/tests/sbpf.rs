use core::hint::black_box;
use curve_bench::{Curve, SCALAR_A, SCALAR_B};
use svm_unit_test::svm_test;

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
