# svm-unit-test

Unit-test Solana SBPF programs from a regular `cargo test`. Annotate a
function with `#[svm_test]`, write what you want to measure, and the
framework compiles it to its own SBPF program, executes it through
[Mollusk](https://github.com/anza-xyz/mollusk), and reports the compute units
consumed.

```rust
use core::hint::black_box;
use my_lib::{Curve, SCALAR_A, SCALAR_B};
use svm_unit_test::svm_test;

#[svm_test]
fn add_mod_n() {
    let r = Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B));
    black_box(r);
}
```

```text
$ cargo test
test add_mod_n      ... ok
test add_mod_n_zero ... ok

# with --nocapture or stderr visible:
svm_test `add_mod_n`      => 76 CUs
svm_test `add_mod_n_zero` => 77 CUs
```

## Requirements

- The Solana CLI installed (`cargo build-sbf` must be on `$PATH`). Tested
  with `solana-cargo-build-sbf 3.1.7` / platform-tools v1.52.
- Rust 2024 edition.

## Install

```toml
[dev-dependencies]
svm-unit-test = "0.1"
```

No `build.rs`, no `[build-dependencies]`. The framework is test-only.

## Use

In any integration test (e.g. `tests/sbpf.rs`):

```rust
use core::hint::black_box;
use my_lib::{Curve, SCALAR_A, SCALAR_B};
use svm_unit_test::svm_test;

#[svm_test]
fn add_mod_n() {
    let r = Curve::add_mod_n(black_box(&SCALAR_A), black_box(&SCALAR_B));
    black_box(r);
}

#[svm_test]
fn add_mod_n_zero() {
    const ZERO: [u64; 4] = [0; 4];
    let r = Curve::add_mod_n(black_box(&SCALAR_A), black_box(&ZERO));
    black_box(r);
}

// Plain (non-#[svm_test]) helpers are kept available to the test bodies.
fn _double(x: u64) -> u64 { x.wrapping_mul(2) }
```

Then:

```sh
cargo test
```

Each `#[svm_test]` becomes a real `#[test]`. Other test attributes
(e.g. `#[ignore]`, `#[should_panic]`) work as usual.

## Parameterised benchmarks with `#[svm_harness]`

`#[svm_test]` runs a fixed body. When you want to feed the same SBPF
program different inputs without recompiling, use `#[svm_harness]`:

```rust
use core::hint::black_box;
use my_lib::Curve;
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
fn add_mod_n_zero() {
    add_mod_n_harness(&AddInputs { a: [0; 4], b: [0; 4] });
}

#[test]
fn add_mod_n_wrap() {
    add_mod_n_harness(&AddInputs { a: [u64::MAX; 4], b: [1, 0, 0, 0] });
}
```

The macro replaces the annotated fn with a host runner of the same
signature — call it from any `#[test]` (or any caller that has the
input) to push that input through the SBPF program and get a CU report.

### How the input gets in

Each `#[test]` serialises `&T` to bytes (one `from_raw_parts` over the
reference); Mollusk hands those bytes to the program as instruction
data; the SBPF entrypoint reinterprets the instruction-data pointer
back as `&T` directly — no copy, no decoder.

### Requirements on `T`

- `#[repr(C)]` — `T`'s in-memory layout *is* the wire format.
- `align_of::<T>() ≤ 8` — Solana's instruction-data buffer is 8-byte
  aligned, so the pointer reinterpret is sound for any reasonable type.
- One parameter only, of the form `&T` (not `T`, not `&mut T`).

### Non-`repr(C)` inputs: bring your own encoding

If your input doesn't fit those constraints — variable-length data, a
type from a crate you can't re-decorate `#[repr(C)]`, or a wire format
you control (borsh, bincode, manual layout) — wrap it in a `#[repr(C)]`
buffer and do the encode/decode at the boundaries yourself:

```rust
use svm_unit_test::svm_harness;

#[repr(C)]
struct Encoded {
    bytes: [u8; 256],
    len: u32,
}

impl Encoded {
    fn pack(v: &MyType) -> Self {
        let bytes = my_serialise(v); // wincode, borsh, bincode, manual, anything
        let mut buf = [0u8; 256];
        buf[..bytes.len()].copy_from_slice(&bytes);
        Self { bytes: buf, len: bytes.len() as u32 }
    }
}

#[svm_harness]
fn bench(input: &Encoded) {
    let value = my_deserialise(&input.bytes[..input.len as usize]);
    // ... use `value`
}

#[test]
fn t() {
    bench(&Encoded::pack(&my_value));
}
```

For simple variable-length cases, `&[u8; N]` works directly without a
wrapper struct (fixed-size arrays are already `#[repr(C)]`).

**A note on CUs:** the framework's zero-copy reinterpret still applies
to the wire buffer (`&Encoded` lands as a register-passed reference,
not a copy). But `my_deserialise` runs inside the SBPF program and its
cost is counted in the CU report — that's intentional. If your real
production code path includes deserialisation, you want to measure it;
if it doesn't, do the decoding host-side and pass the already-decoded
value as a `#[repr(C)]` struct so the harness body only measures the
work you care about.

## How it works

1. The `#[svm_test]` proc macro emits a `#[test]` that, on first run in
   the process, calls into the runtime suite-builder.
2. The suite-builder reads the test file (located by walking up from
   `CARGO_MANIFEST_DIR` until `<dir>/<file!()>` resolves), parses it with
   `syn` to find every `#[svm_test]` sibling plus the surrounding
   `use`s and helper items, and generates one tiny `#![no_std]` cdylib crate
   per test under `target/tmp/<test-bin>/suite-<hash>/build/<name>/`.
3. `cargo build-sbf` is invoked once per test crate; the resulting `.so` is
   placed in `…/suite-<hash>/so/<name>.so`.
4. The macro-generated `#[test]` loads its own `.so` via Mollusk's
   `add_program_with_loader_and_elf` (no temp files, no env-var racing),
   invokes the entrypoint with empty instruction data, and reports
   `compute_units_consumed`.

## Caveats

- The user's lib must build for `sbf-solana-solana` — i.e. `#![no_std]`-clean
  for the path the tests exercise.
- `use` statements pass through into the SBPF source verbatim and must
  resolve there. `svm_unit_test::*` resolves on both sides via a package
  rename in the generated `Cargo.toml`. `#[test]` fns and
  `extern crate` items are dropped on the way in (host-only by definition).
- All `fn`s with `#[svm_test]` / `#[svm_harness]` in a file share one
  suite directory under
  `target/tmp/suite-<hash-of-file-path>/`. Within a single test process the
  suite is built exactly once via a `OnceLock`.
- Every `cargo test` re-invokes `cargo build-sbf` and lets cargo's own
  fingerprinting decide whether to recompile. Editing the test file or any
  path-dep (your lib, transitive crates) is detected and produces a fresh
  `.so`; an unchanged tree finishes in well under a second.
- First run pulls in the Solana SDK and BPF toolchain, so it's slow. After
  that, `cargo build-sbf` is incremental.
- `mollusk-svm` is pinned at `0.12.1-agave-4.0`. Using a different SVM
  feature-set requires a fork.

## Workspace layout

```
crates/svm-unit-test/         # runtime: Mollusk runner, suite builder, host re-exports
crates/svm-unit-test-macros/  # proc macros #[svm_test] / #[svm_harness]
crates/svm-unit-test-types/   # internal no_std glue shared with the generated SBPF crates
examples/curve-bench/         # toy 256-bit add-mod-n bench
```

## License

MIT — see [LICENSE](LICENSE).
