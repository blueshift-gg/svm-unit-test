//! `#[svm_test]` and `#[svm_harness]` proc macros.
//!
//! `#[svm_test]` annotates any free fn and turns it into a regular
//! `#[test]` that, on first invocation, lazily compiles **just that
//! one test's** SBPF program (via [`svm_unit_test::ensure_test_built`])
//! and runs it through Mollusk. Sibling tests that aren't selected
//! (e.g. `cargo test add_mod_n` only runs `add_mod_n`) are never
//! compiled.
//!
//! `#[svm_harness]` is the input-taking variant: the wrapped fn has
//! signature `fn name(input: &T)` and is invoked from a regular
//! `#[test]` (or any caller) as `name(&value)`. The macro generates a
//! host runner with the same signature; the body runs inside the SBPF
//! program, where the entrypoint reinterprets the instruction-data
//! pointer as `&T` — zero copy.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, ItemFn, parse_macro_input};

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


/// Annotate a free fn `fn name(input: &T)` to turn it into a benchmark
/// harness. The macro replaces the fn at source level with a host-side
/// runner of the same signature; the original body is compiled into the
/// per-test SBPF cdylib, where the entrypoint reinterprets the
/// instruction-data pointer as `&T` directly — zero copy 
///
/// Requirements on `T`:
///   * `#[repr(C)]` (so its in-memory layout is what the host writes
///     into instruction data);
///   * `align_of::<T>() ≤ 8` (Solana's instruction-data buffer is
///     8-byte aligned, so anything reasonable fits);
///   * `Sized`.
///
/// The host runner serializes the input by transmuting `&T` to `&[u8]`
/// of length `size_of::<T>()`. The SBPF entrypoint mirror-casts those
/// bytes back to `&T` via a pointer reinterpret.
///
/// ```ignore
/// #[repr(C)]
/// struct AddInputs { a: u64, b: u64 }
///
/// #[svm_unit_test::svm_harness]
/// fn add(input: &AddInputs) {
///     core::hint::black_box(input.a.wrapping_add(input.b));
/// }
///
/// #[test]
/// fn add_small() { add(&AddInputs { a: 1, b: 2 }); }
/// ```
#[proc_macro_attribute]
pub fn svm_harness(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = parse_macro_input!(item as ItemFn);
    let name = &func.sig.ident;
    let name_str = name.to_string();
    let attrs = &func.attrs;
    let vis = &func.vis;

    // We expect exactly one parameter, of the form `&T`.
    let inputs = &func.sig.inputs;
    if inputs.len() != 1 {
        return syn::Error::new_spanned(
            &func.sig,
            "`#[svm_harness]` requires exactly one parameter of the form `&T` (the input value, passed via instruction data)",
        )
        .to_compile_error()
        .into();
    }
    let arg = match inputs.first().unwrap() {
        FnArg::Typed(t) => t,
        FnArg::Receiver(_) => {
            return syn::Error::new_spanned(
                inputs,
                "`#[svm_harness]` cannot be applied to methods (receiver `self` not allowed)",
            )
            .to_compile_error()
            .into();
        }
    };

    // The parameter must be a shared reference. A value-typed param would
    // force a by-value copy at the entrypoint and a redundant stack
    // copy into the user fn, both of which we want to avoid  
    let arg_ref = match &*arg.ty {
        syn::Type::Reference(r) if r.mutability.is_none() => r,
        _ => {
            return syn::Error::new_spanned(
                &arg.ty,
                "`#[svm_harness]` parameter must be a shared reference `&T` — by-value or `&mut` are not supported (the SBPF entrypoint reinterprets the instruction-data pointer as `&T` directly, with no copy)",
            )
            .to_compile_error()
            .into();
        }
    };
    let arg_pat = &arg.pat;
    let arg_ty = &arg.ty;       // `&T`
    let inner_ty = &arg_ref.elem; // `T`
    let block = &func.block;
    let body_fn = format_ident!("_svm_harness_body_{}", name);

    quote! {
        // Host-side ghost copy of the body — never called, but kept so
        // imports referenced only inside the body don't trigger
        // "unused import" warnings on the host build (the body itself
        // only runs inside the SBPF cdylib). Also gives the user
        // host-side type-checking of their benchmark body.
        #[allow(dead_code)]
        fn #body_fn(#arg_pat: #arg_ty) #block

        #(#attrs)*
        #vis fn #name(#arg_pat: #arg_ty) {
            // Re-interpret `&T` as `&[u8]` of length `size_of::<T>()`.
            // The SBPF entrypoint reverses this with a pointer cast, so
            // `T`'s in-memory representation IS the wire format.
            let __svm_bytes: &[u8] = unsafe {
                ::core::slice::from_raw_parts(
                    ::core::ptr::from_ref::<#inner_ty>(#arg_pat).cast::<u8>(),
                    ::core::mem::size_of::<#inner_ty>(),
                )
            };

            let so_path = ::svm_unit_test::ensure_test_built(
                ::std::file!(),
                #name_str,
                ::std::env!("CARGO_MANIFEST_DIR"),
                ::std::env!("CARGO_PKG_NAME"),
                ::std::option_env!("CARGO_TARGET_TMPDIR"),
            );
            let elf = ::std::fs::read(so_path)
                .unwrap_or_else(|e| ::std::panic!("read {}: {}", so_path.display(), e));
            ::svm_unit_test::run_harness(#name_str, &elf, __svm_bytes);
        }
    }
    .into()
}

