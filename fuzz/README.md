# Fuzzing

This crate uses [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz) (libFuzzer) to stress a few "guest input parsing" paths, including:

- MMU page walking / translation
- Physical bus routing logic
- x86 linear-memory wrapped helpers (`aero-cpu-core::linear_mem::*_wrapped`)
- Storage controller emulation (AHCI + IDE + ATAPI + PIIX3 PCI wrapper)
- L2 tunnel protocol codec (`aero-l2-protocol`)
- User-space network stack Ethernet ingress (`aero-net-stack`)
- NIC device models (E1000 + virtio-net)
- HTTP `Range` header parsing (`aero-http-range`)
- Auth token verification (`aero-auth-tokens`)
- AeroSparse disk image parsing/open (`aero-storage`)
- AeroGPU command stream + alloc-table parsing (`aero-gpu` / `aero-protocol`)
- DXBC container + shader bytecode parsing (`aero-dxbc`)
- D3D9 SM2/SM3 shader decoding + IR/WGSL lowering (`aero-d3d9::sm3`)
- D3D9 shader token stream parsing + disassembly formatting (`aero-d3d9-shader`)
- Intel HDA controller emulation (MMIO + CORB/RIRB parsing) (`aero-audio`)
- virtio-snd queue parsing + playback/capture request handling (`aero-virtio`)
- BIOS HLE interrupt dispatch robustness (INT 10h/13h/15h/16h/1Ah) (`firmware`)
- i8042 PS/2 controller emulation (`aero-devices-input`)
- UHCI (USB 1.1) controller register file + schedule walker (`aero-usb`)
- HID report descriptor parsing (`aero-usb`)

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
cargo +"$nightly" fuzz run fuzz_auth_tokens
cargo +"$nightly" fuzz run fuzz_atapi
cargo +"$nightly" fuzz run fuzz_aerosparse_open
cargo +"$nightly" fuzz run fuzz_aero_storage_sparse_open
cargo +"$nightly" fuzz run fuzz_disk_image_open_auto
cargo +"$nightly" fuzz run fuzz_aerogpu_parse
cargo +"$nightly" fuzz run fuzz_i8042
cargo +"$nightly" fuzz run fuzz_uhci
cargo +"$nightly" fuzz run fuzz_hid_report_descriptor
cargo +"$nightly" fuzz run fuzz_aerogpu_bc_decompress
cargo +"$nightly" fuzz run fuzz_bios_interrupts
cargo +"$nightly" fuzz run fuzz_tier0_step
cargo +"$nightly" fuzz run fuzz_linear_mem_wrapped

# DXBC / shaders
  cargo +"$nightly" fuzz run fuzz_dxbc_sm4_parse
  cargo +"$nightly" fuzz run fuzz_dxbc_parse
  cargo +"$nightly" fuzz run fuzz_d3d11_sm4_translate
  cargo +"$nightly" fuzz run fuzz_d3d9_sm3_decode
  cargo +"$nightly" fuzz run fuzz_d3d9_sm3_wgsl
  cargo +"$nightly" fuzz run fuzz_d3d9_shader_parse
  
  # Audio
  cargo +"$nightly" fuzz run fuzz_hda_mmio
  cargo +"$nightly" fuzz run fuzz_hda_corb_verbs
  cargo +"$nightly" fuzz run fuzz_virtio_snd_queues

# Networking
  cargo +"$nightly" fuzz run fuzz_l2_protocol_decode
  cargo +"$nightly" fuzz run fuzz_net_stack_outbound_ethernet
  cargo +"$nightly" fuzz run fuzz_e1000_mmio_poll
  cargo +"$nightly" fuzz run fuzz_virtio_net_queue
```

To run time-bounded:

```bash
cargo +"$nightly" fuzz run fuzz_mmu_translate -- -max_total_time=10
```

## Resource limits / AddressSanitizer note

`cargo-fuzz` enables AddressSanitizer by default. ASan reserves a very large *virtual* address
space region for shadow memory. If you run fuzzers under a strict `RLIMIT_AS` (virtual address
space) limit (for example via `scripts/safe-run.sh`), the fuzz target may fail to start with an
ASan error like:

```
ReserveShadowMemoryRange failed while trying to map ...
```

Workarounds:

- Use an unlimited/high virtual address space limit when running fuzzers:
  - `AERO_MEM_LIMIT=unlimited bash ./scripts/safe-run.sh cargo +"$nightly" fuzz run <target> -- -max_total_time=10`
- Or disable sanitizers for that run (less bug-finding, but avoids the VA reservation):
  - `cargo +"$nightly" fuzz run -s none <target> -- -max_total_time=10`

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

If you see an error like:

```
error: the option `Z` is only accepted on the nightly compiler
```

make sure you're actually using a nightly toolchain when invoking `cargo fuzz` (e.g. `cargo +"$nightly" fuzz ...`,
or `unset RUSTUP_TOOLCHAIN` if something in your environment is forcing stable).

Run a bounded number of iterations:

```bash
cd fuzz && cargo +"$nightly" fuzz run fuzz_ahci -- -runs=10000
cd fuzz && cargo fuzz run fuzz_ahci -- -runs=10000

# Targeted AHCI command list / PRDT parsing
cd fuzz && cargo fuzz run fuzz_ahci_command -- -runs=10000

# IDE (PIIX3-style, includes Bus Master IDE DMA)
cd fuzz && cargo fuzz run fuzz_ide -- -runs=10000

# Targeted Bus Master IDE PRD parsing / DMA engine
cd fuzz && cargo fuzz run fuzz_ide_busmaster -- -runs=10000

# IDE via PCI wrapper (PIIX3-style config gating + BAR4 relocation + BMIDE DMA)
cd fuzz && cargo fuzz run fuzz_piix3_ide_pci -- -runs=10000

# HTTP Range parsing/resolution (hostile headers near caps)
cd fuzz && cargo fuzz run fuzz_http_range -- -runs=10000

# Auth tokens (session cookie + HS256 JWT)
cd fuzz && cargo fuzz run fuzz_auth_tokens -- -runs=10000

# ATAPI packet parsing (SCSI CDBs)
cd fuzz && cargo fuzz run fuzz_atapi -- -runs=10000

# AeroSparse image parsing/open + bounded IO against corrupt images
cd fuzz && cargo fuzz run fuzz_aerosparse_open -- -runs=10000
cd fuzz && cargo fuzz run fuzz_aero_storage_sparse_open -- -runs=10000

# Allow larger generated inputs (the target itself caps at 1MiB)
cd fuzz && cargo fuzz run fuzz_aero_storage_sparse_open -- -runs=10000 -max_len=1048576

# Optional: use the bundled dictionary to help libFuzzer find valid headers faster
cd fuzz && cargo fuzz run fuzz_aero_storage_sparse_open -- -runs=10000 -max_len=1048576 -dict=fuzz_targets/fuzz_aero_storage_sparse_open.dict
cd fuzz && cargo fuzz run fuzz_aerosparse_open -- -runs=10000 -max_len=1048576 -dict=fuzz_targets/fuzz_aerosparse_open.dict
cd fuzz && cargo fuzz run fuzz_disk_image_open_auto -- -runs=10000 -max_len=1048576 -dict=fuzz_targets/fuzz_disk_image_open_auto.dict

# Auto-detect + open (raw/aerosparse/qcow2/vhd) + bounded IO
cd fuzz && cargo fuzz run fuzz_disk_image_open_auto -- -runs=10000

# AeroGPU command stream + alloc-table parsing
cd fuzz && cargo fuzz run fuzz_aerogpu_parse -- -runs=10000

# AeroGPU CPU BCn decompression (BC1/BC2/BC3/BC7) + hostile dims/truncated inputs
cd fuzz && cargo fuzz run fuzz_aerogpu_bc_decompress -- -runs=10000

# DXBC parsing
cd fuzz && cargo fuzz run fuzz_dxbc_parse -- -runs=10000
cd fuzz && cargo fuzz run fuzz_dxbc_parse -- -runs=10000 -dict=fuzz_targets/fuzz_dxbc_sm4_parse.dict
  
# DXBC container + signature + SM4/SM5 token parsing
cd fuzz && cargo fuzz run fuzz_dxbc_sm4_parse -- -runs=10000

# Optional: use the bundled dictionary to help libFuzzer find DXBC/signature chunk IDs faster
cd fuzz && cargo fuzz run fuzz_dxbc_sm4_parse -- -runs=10000 -dict=fuzz_targets/fuzz_dxbc_sm4_parse.dict
  
# D3D11 SM4/SM5 decode + WGSL translation
cd fuzz && cargo fuzz run fuzz_d3d11_sm4_translate -- -runs=10000

# Optional: use the bundled dictionary to help libFuzzer find DXBC/signature + SM token patterns faster
cd fuzz && cargo fuzz run fuzz_d3d11_sm4_translate -- -runs=10000 -dict=fuzz_targets/fuzz_d3d11_sm4_translate.dict
 
# D3D9 SM2/SM3 bytecode decode + IR build
cd fuzz && cargo fuzz run fuzz_d3d9_sm3_decode -- -runs=10000

# Optional: use the bundled dictionary to help libFuzzer find version/opcode tokens faster
cd fuzz && cargo fuzz run fuzz_d3d9_sm3_decode -- -runs=10000 -dict=fuzz_targets/fuzz_d3d9_sm3.dict

# D3D9 SM2/SM3 IR -> WGSL generation
cd fuzz && cargo fuzz run fuzz_d3d9_sm3_wgsl -- -runs=10000

# Optional: same dictionary for WGSL generation fuzzer
cd fuzz && cargo fuzz run fuzz_d3d9_sm3_wgsl -- -runs=10000 -dict=fuzz_targets/fuzz_d3d9_sm3.dict
 
# Networking (quick sanity)
cd fuzz && cargo fuzz run fuzz_l2_protocol_decode -- -runs=1000
cd fuzz && cargo fuzz run fuzz_net_stack_outbound_ethernet -- -runs=1000
cd fuzz && cargo fuzz run fuzz_e1000_mmio_poll -- -runs=1000
cd fuzz && cargo fuzz run fuzz_virtio_net_queue -- -runs=1000

# i8042 PS/2 controller port I/O + keyboard injection + snapshot roundtrips
cd fuzz && cargo fuzz run fuzz_i8042 -- -runs=10000 -dict=fuzz_targets/fuzz_i8042.dict

# UHCI register I/O + schedule walker tick + snapshot roundtrips
cd fuzz && cargo fuzz run fuzz_uhci -- -runs=10000 -dict=fuzz_targets/fuzz_uhci.dict

# HID report descriptor parser (bounded to 4KiB per input)
cd fuzz && cargo fuzz run fuzz_hid_report_descriptor -- -runs=10000 -dict=fuzz_targets/fuzz_hid_report_descriptor.dict
```
