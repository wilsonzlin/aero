# `aero-emulator` (legacy/minimal emulator crate)

This crate contains a small Rust-only emulator core used by unit tests and early bring-up.

It is **not** the primary browser-facing emulator implementation and it does **not** contain
the canonical Win7/WDDM AeroGPU device model or ABI.

For the real AeroGPU contract, see:

- `drivers/aerogpu/protocol/*` (C headers, source of truth)
- `emulator/protocol` (Rust/TypeScript mirror)
- `crates/emulator/src/devices/pci/aerogpu.rs` (emulator device model)
- `docs/graphics/aerogpu-protocols.md` (overview of in-tree GPU ABIs)

