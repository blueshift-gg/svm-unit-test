use core::hint::black_box;
use curve_bench::Curve;
use svm_unit_test::svm_harness;

#[repr(C)]
struct AddInputs {
    a: [u64; 4],
    b: [u64; 4],
}

#[svm_harness]
fn add_mod_n_harness(input: &AddInputs) {
    Curve::add_mod_n(black_box(&input.a), black_box(&input.b));
}

#[test]
fn add_mod_n_zero_zero() {
    add_mod_n_harness(&AddInputs {
        a: [0; 4],
        b: [0; 4],
    });
}

