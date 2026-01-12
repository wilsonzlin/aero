# AeroGPU protocols in this repository

This repository contains multiple guest↔host GPU “protocols” that have accumulated during
bring-up. Only **one** of them is the real Windows 7 (WDDM-style) **versioned** AeroGPU ABI
that new work should target.

If you are doing new work and you are not explicitly working on a prototype, **target the
Win7/WDDM ABI** described below.

Note on the canonical machine (`aero_machine::Machine`):

- The canonical full-system machine reserves `00:07.0` for the AeroGPU Windows driver contract
  (`PCI\\VEN_A3A0&DEV_0001`), but does **not** yet expose the full AeroGPU PCI device model.
- Today, boot display in the canonical machine is provided by `aero_gpu_vga` (legacy VGA ports +
  Bochs VBE) plus a minimal “Standard VGA”-like PCI stub at `00:0c.0` (`1234:1111`) used only for VBE
  LFB MMIO routing.

See:

- [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- [`docs/pci-device-compatibility.md`](../pci-device-compatibility.md)

## Win7/WDDM target ABI (the real AeroGPU protocol)

**Source of truth (versioned ABI headers):** `drivers/aerogpu/protocol/*`

- `aerogpu_pci.h` — PCI IDs, BAR0 layout, MMIO register map, feature bits.
- `aerogpu_ring.h` — ring header + submission descriptors + (optional) fence page.
- `aerogpu_cmd.h` — command stream packets (“AeroGPU IR”).
- `aerogpu_escape.h` — stable `DxgkDdiEscape` packet header + base ops.
- `aerogpu_dbgctl_escape.h` — bring-up/tooling `DxgkDdiEscape` packets (layered on `aerogpu_escape.h`).

**Host/emulator implementation:** `crates/emulator`

- `crates/emulator/src/devices/pci/aerogpu.rs` — PCI device + MMIO register behavior.
- `crates/emulator/src/devices/aerogpu_ring.rs` — ring parsing utilities.
- `crates/emulator/src/gpu_worker/aerogpu_executor.rs` — execution/translation glue.
- `crates/aero-gpu/src/protocol.rs` — host-side parser for the versioned command stream (`aerogpu_cmd.h`).
- `emulator/protocol` — Rust/TypeScript mirror of the C headers (used by tooling/tests).

This is the ABI that the Windows 7 WDDM 1.1 driver stack (KMD + UMD) targets.
Current status: UMDs in this repo emit the versioned command stream (`aerogpu_cmd.h`). The
Win7 KMD supports both the versioned and legacy submission transports and auto-detects the
active ABI via BAR0 MMIO magic (see `drivers/aerogpu/protocol/README.md` and
`drivers/aerogpu/kmd/README.md`).

### Legacy bring-up ABI (retired; not the long-term target)

There is also a **legacy bring-up** PCI/MMIO ABI:

- Header: `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`
- Host device model: `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (feature `emulator/aerogpu-legacy`)

It is retained for historical reference and optional regression testing; it is not supported by the
shipping Win7 driver package and should generally not be the target for new features.
For a concise mapping of PCI IDs ↔ ABI ↔ device model, see `docs/abi/aerogpu-pci-identity.md`.

## Legacy prototype: toy CREATE_SURFACE/PRESENT protocol (removed)

This repository previously contained a minimal, self-contained paravirtual GPU used for early
bring-up and small smoke tests (ring mechanics, MMIO doorbell/IRQ plumbing, deterministic “draw
a triangle”). That implementation originally lived in `crates/aero-emulator` and is now archived
under `crates/legacy/aero-emulator` (excluded from the default Cargo workspace) in favor of the
canonical Win7/WDDM path.

The protocol is still documented for reference, but it is archived:

- `docs/legacy/aerogpu-prototype-command-protocol.md` (full spec)
- `docs/abi/gpu-command-protocol.md` (deprecated stub / original location)

- Commands are things like `CREATE_SURFACE`, `UPDATE_SURFACE`, `CLEAR_RGBA`, `PRESENT`.
- The code is intentionally simple and does not model WDDM concepts.
- It used stale placeholder PCI IDs (deprecated vendor 1AE0) and must not be used as a
  driver contract (see `docs/abi/aerogpu-pci-identity.md`).

It is **not** compatible with the Win7/WDDM AeroGPU protocol.

## Retired prototype ABI: experimental cmd/completion ring (removed)

This repository previously contained an experimental ring/opcode ABI used for early backend
experiments and for exercising gpu-trace plumbing. It has been removed so the repo only
supports the canonical A3A0 AeroGPU protocol.

The archived note is kept under:

- `docs/legacy/experimental-gpu-command-abi.md`

## Summary / guidance

- **Implementing the Win7 graphics stack:** use `drivers/aerogpu/protocol/*` + `crates/emulator`.
- **Working on early/prototype plumbing:** the toy protocols are fine, but keep them clearly
  labeled as prototypes.

Note: an older prototype Win7 driver tree existed during early bring-up; it used stale placeholder PCI IDs
(vendor 1AE0) and was not WOW64-complete on Win7 x64. It is archived under
`prototype/legacy-win7-aerogpu-1ae0/` to avoid accidental installs; use `drivers/aerogpu/packaging/win7/`
for the supported Win7 driver package.
