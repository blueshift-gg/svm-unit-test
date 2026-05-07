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
- `use svm_unit_test::svm_test;` is auto-stripped from the SBPF source.
  Other `use` statements pass through verbatim and must resolve in the SBPF
  crate.
- All `fn`s with `#[svm_test]` in a file share one suite directory under
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
crates/svm-unit-test/         # runtime: macro re-export, Mollusk runner, suite builder
crates/svm-unit-test-macros/  # proc macro #[svm_test]
examples/curve-bench/         # toy 256-bit add-mod-n bench
```

## License

MIT — see [LICENSE](LICENSE).
