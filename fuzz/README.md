# Fuzzing

This crate uses [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) to stress a few "guest input parsing" paths, including:

- MMU page walking / translation
- Physical bus routing logic
- Storage controller emulation (AHCI + IDE + ATAPI + PIIX3 PCI wrapper)
- HTTP `Range` header parsing (`aero-http-range`)
- AeroSparse disk image parsing/open (`aero-storage`)
- AeroGPU command stream + alloc-table parsing (`aero-gpu` / `aero-protocol`)

## Prereqs

```bash
cargo install --locked cargo-fuzz
nightly="$(node -p "require('./scripts/toolchains.json').rust.nightlyWasm")"
rustup toolchain install "$nightly"
```

## Run

From the repository root:

```bash
cargo +"$nightly" fuzz run fuzz_mmu_translate
cargo +"$nightly" fuzz run fuzz_bus_rw
cargo +"$nightly" fuzz run fuzz_ahci
cargo +"$nightly" fuzz run fuzz_ahci_command
cargo +"$nightly" fuzz run fuzz_ide
cargo +"$nightly" fuzz run fuzz_piix3_ide_pci
cargo +"$nightly" fuzz run fuzz_ide_busmaster
cargo +"$nightly" fuzz run fuzz_http_range
cargo +"$nightly" fuzz run fuzz_atapi
cargo +"$nightly" fuzz run fuzz_aerosparse_open
cargo +"$nightly" fuzz run fuzz_aero_storage_sparse_open
cargo +"$nightly" fuzz run fuzz_aerogpu_parse
```

To run time-bounded:

```bash
cargo +"$nightly" fuzz run fuzz_mmu_translate -- -max_total_time=10
```

## Smoke runs

Build all targets:

```bash
cd fuzz && cargo +"$nightly" fuzz build
```

The `fuzz/` directory includes its own `rust-toolchain.toml` (nightly), so you can also run these
from inside `fuzz/` without specifying a `+toolchain`:

```bash
cd fuzz && cargo fuzz build
```

Note: an explicit `RUSTUP_TOOLCHAIN=...` environment variable overrides `rust-toolchain.toml`.

Run a bounded number of iterations:

```bash
cd fuzz && cargo +"$nightly" fuzz run fuzz_ahci -- -runs=10000
cd fuzz && cargo fuzz run fuzz_ahci -- -runs=10000

# IDE (PIIX3-style, includes Bus Master IDE DMA)
cd fuzz && cargo fuzz run fuzz_ide -- -runs=10000

# IDE via PCI wrapper (PIIX3-style config gating + BAR4 relocation + BMIDE DMA)
cd fuzz && cargo fuzz run fuzz_piix3_ide_pci -- -runs=10000

# HTTP Range parsing/resolution (hostile headers near caps)
cd fuzz && cargo fuzz run fuzz_http_range -- -runs=10000

# ATAPI packet parsing (SCSI CDBs)
cd fuzz && cargo fuzz run fuzz_atapi -- -runs=10000

# AeroSparse image parsing/open + bounded IO against corrupt images
cd fuzz && cargo fuzz run fuzz_aerosparse_open -- -runs=10000
cd fuzz && cargo fuzz run fuzz_aero_storage_sparse_open -- -runs=10000

# AeroGPU command stream + alloc-table parsing
cd fuzz && cargo fuzz run fuzz_aerogpu_parse -- -runs=10000
```
