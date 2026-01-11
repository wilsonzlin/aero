# `aero-gpu-device` (experimental)

This crate implements a **standalone** virtual GPU device model and ring/opcode ABI used for:

- deterministic host-side tests (`crates/aero-gpu-device/tests/*`), and
- validating gpu-trace capture/replay plumbing.

It is **not** the canonical Windows 7 WDDM AeroGPU guest↔emulator protocol.

## Canonical AeroGPU WDDM protocol

For the real AeroGPU PCI/MMIO/ring/command ABI used by the Win7 drivers, see:

- [`drivers/aerogpu/protocol/README.md`](../../drivers/aerogpu/protocol/README.md)
  (`aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
- [`emulator/protocol`](../../emulator/protocol) (Rust/TypeScript mirror)

The experimental device in this crate uses its own PCI IDs (notably vendor ID `0xA0E0`) and
should not be used as the Windows driver contract.
