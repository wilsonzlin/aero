# `aero-gpu-device` (experimental/prototype ABI)

This crate implements a **standalone** virtual GPU device model and guest↔host ring/opcode ABI
used for:

- deterministic host-side tests (`crates/aero-gpu-device/tests/*`), and
- validating gpu-trace capture/replay plumbing.

It is identifiable by the FourCC values used in its ring/record headers (`"AGRN"`/`"AGPC"`) and
uses its own PCI IDs (notably vendor ID `0xA0E0`).

It is **not** the canonical Windows 7 WDDM AeroGPU guest↔emulator protocol.

## Canonical AeroGPU WDDM protocol

For the real AeroGPU PCI/MMIO/ring/command ABI used by the Win7 drivers, see:

- [`drivers/aerogpu/protocol/README.md`](../../drivers/aerogpu/protocol/README.md)
  (`aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
- [`emulator/protocol`](../../emulator/protocol) (Rust/TypeScript mirror)
- `crates/emulator/src/devices/pci/aerogpu.rs` (emulator implementation)

See [`docs/graphics/aerogpu-protocols.md`](../../docs/graphics/aerogpu-protocols.md) for an
overview of similarly named in-tree protocols.
