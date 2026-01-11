# AeroGPU protocols in this repository

This repository contains multiple guest↔host GPU “protocols” that have accumulated during
bring-up. Only **one** of them is the real Windows 7 (WDDM-style) AeroGPU ABI.

If you are doing new work and you are not explicitly working on a prototype, **target the
Win7/WDDM ABI** described below.

## Win7/WDDM target ABI (the real AeroGPU protocol)

**Source of truth (stable ABI headers):** `drivers/aerogpu/protocol/*`

- `aerogpu_pci.h` — PCI IDs, BAR0 layout, MMIO register map, feature bits.
- `aerogpu_ring.h` — ring header + submission descriptors + (optional) fence page.
- `aerogpu_cmd.h` — command stream packets (“AeroGPU IR”).
- `aerogpu_dbgctl_escape.h` — bring-up `DxgkDdiEscape` packets used by tooling.

**Host/emulator implementation:** `crates/emulator`

- `crates/emulator/src/devices/pci/aerogpu.rs` — PCI device + MMIO register behavior.
- `crates/emulator/src/devices/aerogpu_ring.rs` — ring parsing utilities.
- `crates/emulator/src/gpu_worker/aerogpu_executor.rs` — execution/translation glue.
- `emulator/protocol` — Rust/TypeScript mirror of the C headers (used by tooling/tests).

This is the ABI that the Windows 7 WDDM 1.1 driver stack (KMD + UMD) targets.

## Legacy prototype: toy CREATE_SURFACE/PRESENT protocol (removed)

This repository previously contained a minimal, self-contained paravirtual GPU used for early
bring-up and small smoke tests (ring mechanics, MMIO doorbell/IRQ plumbing, deterministic “draw
a triangle”). That implementation lived in `crates/aero-emulator`, but it has since been
removed in favor of the canonical Win7/WDDM path.

The protocol is still documented for reference in `docs/abi/gpu-command-protocol.md`:

- Commands are things like `CREATE_SURFACE`, `UPDATE_SURFACE`, `CLEAR_RGBA`, `PRESENT`.
- The code is intentionally simple and does not model WDDM concepts.

It is **not** compatible with the Win7/WDDM AeroGPU protocol.

## Prototype ABI: `aero-gpu-device` (AGRN/AGPC)

**Location:** `crates/aero-gpu-device/*`

This is another unrelated guest↔host GPU command ABI. It is identifiable by the FourCC
values used in its ring/record headers:

- `"AGRN"` — ring header magic
- `"AGPC"` — command record magic

It is used as a harness for backend experiments and for exercising the GPU trace
container/recorder (`crates/aero-gpu-trace`), but it is **not** the Win7/WDDM AeroGPU ABI.

## Summary / guidance

- **Implementing the Win7 graphics stack:** use `drivers/aerogpu/protocol/*` + `crates/emulator`.
- **Working on early/prototype plumbing:** the toy protocols are fine, but keep them clearly
  labeled as prototypes.
