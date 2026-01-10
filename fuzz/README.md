# Fuzzing

This crate uses [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) to stress the MMU page-walker and the physical bus routing logic.

## Prereqs

```bash
cargo install cargo-fuzz
rustup toolchain install nightly
```

## Run

From the repository root:

```bash
cargo +nightly fuzz run fuzz_mmu_translate
cargo +nightly fuzz run fuzz_bus_rw
```

To run time-bounded:

```bash
cargo +nightly fuzz run fuzz_mmu_translate -- -max_total_time=10
```

