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
//!
//! By default the test passes only if the program executes successfully. Three
//! attribute forms invert that:
//!
//! ```ignore
//! use svm_unit_test::{svm_test, ProgramError};
//!
//! // Passes if the program returns *any* error (fails if it succeeds).
//! // `fail` and a bare `error` are equivalent.
//! #[svm_test(fail)]
//! fn rejects_garbage() { /* … */ }
//!
//! // Passes only if the program fails with exactly this `ProgramError`.
//! // The body returns a `u64` exit code; small codes map to
//! // `ProgramError::Custom(n)`.
//! #[svm_test(error = ProgramError::Custom(1))]
//! fn rejects_with_code() -> u64 { 1 }
//! ```
//!
//! A body must return `()` (success) or a `u64` exit code. The code is run
//! through Solana's `u64 → ProgramError` mapping, so `error = Custom(n)` is
//! the practically matchable form; builtin variants encode to `n << 32` and
//! need their encoded `u64` returned explicitly.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Expr, ItemFn, Token,
    parse::{Parse, ParseStream},
    parse_macro_input,
};

/// Parsed form of the `#[svm_test(...)]` attribute arguments.
enum Expectation {
    /// Plain `#[svm_test]` — the program must succeed.
    Success,
    /// `#[svm_test(fail)]` or bare `#[svm_test(error)]` — the program must
    /// return some error.
    Failure,
    /// `#[svm_test(error = <expr>)]` — the program must fail with this error.
    Error(Expr),
}

impl Parse for Expectation {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.is_empty() {
            return Ok(Expectation::Success);
        }
        let ident: syn::Ident = input.parse()?;
        match ident.to_string().as_str() {
            "fail" => Ok(Expectation::Failure),
            // `error` is overloaded: bare `error` accepts any failure, while
            // `error = <expr>` pins the exact `ProgramError`.
            "error" => {
                if input.parse::<Token![=]>().is_ok() {
                    Ok(Expectation::Error(input.parse()?))
                } else {
                    Ok(Expectation::Failure)
                }
            }
            other => Err(syn::Error::new(
                ident.span(),
                format!(
                    "unknown `svm_test` argument `{other}`; expected nothing, \
                     `fail`, `error`, or `error = <ProgramError expression>`"
                ),
            )),
        }
    }
}

#[proc_macro_attribute]
pub fn svm_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let expectation = parse_macro_input!(attr as Expectation);
    let func = parse_macro_input!(item as ItemFn);
    let name = &func.sig.ident;
    let name_str = name.to_string();
    let block = &func.block;
    let attrs = &func.attrs;
    let output = &func.sig.output;
    let body_fn = format_ident!("_svm_test_body_{}", name);

    let expect = match expectation {
        Expectation::Success => quote! { ::svm_unit_test::Expect::Success },
        Expectation::Failure => quote! { ::svm_unit_test::Expect::Failure },
        Expectation::Error(err) => quote! { ::svm_unit_test::Expect::Error(#err) },
    };

    quote! {
        #[allow(dead_code)]
        fn #body_fn() #output {
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
            ::svm_unit_test::run_expecting(#name_str, &elf, #expect);
        }
    }
    .into()
}
