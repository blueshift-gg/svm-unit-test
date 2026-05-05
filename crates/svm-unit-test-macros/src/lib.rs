//! `#[svm_test]` proc macro.
//!
//! Annotate any free fn with `#[svm_test]`. On `cargo test` it becomes a
//! regular `#[test]` that, on first run, calls
//! [`svm_unit_test::ensure_suite_built`] — which reads the test file, parses
//! it to find every `#[svm_test]` sibling plus their surrounding
//! `use`s/helpers, and drives one `cargo build-sbf` per fn. The test then
//! executes its own `.so` in Mollusk and reports CUs.
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
            let dir = ::svm_unit_test::ensure_suite_built(
                ::std::file!(),
                ::std::env!("CARGO_MANIFEST_DIR"),
                ::std::env!("CARGO_PKG_NAME"),
                ::std::option_env!("CARGO_TARGET_TMPDIR"),
            );
            let path = dir.join(::std::concat!(#name_str, ".so"));
            let elf = ::std::fs::read(&path)
                .unwrap_or_else(|e| ::std::panic!("read {}: {}", path.display(), e));
            ::svm_unit_test::run(#name_str, &elf);
        }
    }
    .into()
}
