//! Toy 256-bit modular arithmetic, just enough to demonstrate `#[sbpf_test]`.
//!
//! `no_std`-clean so it builds for both host and SBPF.

#![no_std]

pub type Scalar = [u64; 4];

pub const SCALAR_A: Scalar = [
    0x0123_4567_89ab_cdef,
    0xfedc_ba98_7654_3210,
    0x1111_2222_3333_4444,
    0x5555_6666_7777_8888,
];

pub const SCALAR_B: Scalar = [
    0xdead_beef_cafe_babe,
    0x1234_5678_9abc_def0,
    0x0fed_cba9_8765_4321,
    0x2222_3333_4444_5555,
];

/// Order `n` of some hypothetical curve. Arbitrary value chosen so wrap is
/// observable.
pub const N: Scalar = [
    0xffff_ffff_ffff_fff5,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_ffff_ffff,
    0x7fff_ffff_ffff_ffff,
];

pub struct Curve;

impl Curve {
    /// `(a + b) mod n` with carry-propagated 256-bit add and a single
    /// conditional subtract.
    #[inline(never)]
    pub fn add_mod_n(a: &Scalar, b: &Scalar) -> Scalar {
        let mut sum = [0u64; 4];
        let mut carry: u128 = 0;
        for i in 0..4 {
            let s = a[i] as u128 + b[i] as u128 + carry;
            sum[i] = s as u64;
            carry = s >> 64;
        }

        let needs_sub = carry != 0 || cmp_ge(&sum, &N);
        if needs_sub {
            let mut borrow: i128 = 0;
            for i in 0..4 {
                let d = sum[i] as i128 - N[i] as i128 - borrow;
                sum[i] = d as u64;
                borrow = if d < 0 { 1 } else { 0 };
            }
        }
        sum
    }
}

fn cmp_ge(a: &Scalar, b: &Scalar) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}
