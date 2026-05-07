//! `#[svm_test]` proc macro.
//!
//! Annotate any free fn with `#[svm_test]`. On `cargo test` it becomes a
//! regular `#[test]` that, on first invocation, calls
//! [`svm_unit_test::ensure_test_built`] — which lazily compiles *just this
//! test's* SBPF program and returns the path to its `.so`. Sibling tests
//! that aren't selected (e.g. `cargo test add_mod_n` only runs `add_mod_n`)
//! are never compiled.
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

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

#[proc_macro_attribute]
pub fn svm_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let name = &func.sig.ident;
    let name_str = name.to_string();
    let block = &func.block;
    let attrs = &func.attrs;
    let body_fn = format_ident!("_svm_test_body_{}", name);

    quote! {
        #[allow(dead_code)]
        fn #body_fn() {
            #block
        }

        #(#attrs)*
        #[test]
        fn #name() {
            let so_path = ::svm_unit_test::ensure_test_built(
                ::std::file!(),
                #name_str,
                ::std::env!("CARGO_MANIFEST_DIR"),
                ::std::env!("CARGO_PKG_NAME"),
                ::std::option_env!("CARGO_TARGET_TMPDIR"),
            );
            let elf = ::std::fs::read(so_path)
                .unwrap_or_else(|e| ::std::panic!("read {}: {}", so_path.display(), e));
            ::svm_unit_test::run(#name_str, &elf);
        }
    }
    .into()
}
