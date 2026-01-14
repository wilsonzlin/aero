# AeroGPU protocols in this repository

This repository contains multiple guest↔host GPU “protocols” that have accumulated during
bring-up. Only **one** of them is the real Windows 7 (WDDM-style) **versioned** AeroGPU ABI
that new work should target.

If you are doing new work and you are not explicitly working on a prototype, **target the
Win7/WDDM ABI** described below.

For the repo-wide “what’s implemented vs what’s missing” status checklist, see:

- [`docs/graphics/status.md`](./status.md)

Note on the canonical machine (`aero_machine::Machine`):

- The canonical full-system machine reserves `00:07.0` for the AeroGPU Windows driver contract
  (`PCI\\VEN_A3A0&DEV_0001`).
- When `MachineConfig::enable_aerogpu=true`, the machine exposes the AeroGPU PCI identity at
  `00:07.0` (`A3A0:0001`) with the canonical BAR layout (BAR0 regs + BAR1 VRAM aperture). In
  `aero_machine` today BAR1 is backed by a dedicated VRAM buffer for legacy VGA/VBE compatibility
  and implements permissive legacy VGA decode (VGA port I/O + VRAM-backed `0xA0000..0xBFFFF` window;
  see `docs/16-aerogpu-vga-vesa-compat.md`). Note: the in-tree Win7 AeroGPU driver treats the
  adapter as system-memory-backed (no dedicated WDDM VRAM segment); BAR1 is outside the WDDM memory
  model. BAR0 is implemented as a minimal MMIO surface:
  - ring/fence transport (submission decode/capture + fence-page/IRQ plumbing). Default bring-up
    behavior can complete fences without executing the command stream; browser/WASM runtimes can
    enable an out-of-process “submission bridge” (`Machine::aerogpu_drain_submissions` +
    `Machine::aerogpu_complete_fence`) so the GPU worker can execute submissions and report fence
    completion, and native builds can optionally install a feature-gated in-process headless wgpu
    backend (`Machine::aerogpu_set_backend_wgpu`), and
  - scanout0/vblank register storage so the host can present a guest-programmed scanout framebuffer
    and the Win7 stack can use vblank pacing primitives (see `drivers/aerogpu/protocol/vblank.md`).

  A shared device-side library (`crates/aero-devices-gpu`) contains the canonical register/ring
  definitions plus a ring executor (doorbell processing, fence tracking, vsync/vblank pacing) and
  a reusable PCI wrapper. Command execution is provided by host-side executors/backends (GPU worker
  execution via the submission bridge, or optional in-process backends such as the feature-gated
  wgpu backend). When no backend/bridge is installed, `aero-machine` completes fences without
  executing ACMD so guests can boot.
  - For an explicit breakdown of these executor modes, see [`docs/graphics/aerogpu-executor-modes.md`](./aerogpu-executor-modes.md).
  - For a broader “what’s implemented vs missing” checklist, see [`docs/graphics/status.md`](./status.md).
- Boot display in the canonical machine is provided by `aero_gpu_vga` (legacy VGA ports + Bochs VBE)
  when `MachineConfig::enable_vga=true`.
  - When `enable_pc_platform=false`, the VBE LFB MMIO aperture is mapped directly at the configured base.
  - When `enable_pc_platform=true`, the machine exposes a minimal Bochs/QEMU-compatible “Standard VGA”
    PCI function (currently `00:0c.0`) and routes the VBE LFB through PCI BAR0 inside the PCI MMIO
    window / BAR router (BAR base assigned by BIOS POST / the PCI allocator).

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

**Legacy/sandbox integration surface:** `crates/emulator`

- `crates/emulator/src/devices/pci/aerogpu.rs` — PCI device + MMIO register behavior.
- `crates/emulator/src/devices/aerogpu_ring.rs` — ring parsing utilities.
- `crates/emulator/src/gpu_worker/aerogpu_executor.rs` — execution/translation glue.
- `crates/aero-gpu/src/protocol.rs` — host-side parser for the versioned command stream (`aerogpu_cmd.h`).
- `emulator/protocol` — Rust/TypeScript mirror of the C headers (used by tooling/tests).

**Shared device-side library:** `crates/aero-devices-gpu`

- `crates/aero-devices-gpu/src/executor.rs` — ring executor (doorbell processing, fence/vblank pacing).
- `crates/aero-devices-gpu/src/pci.rs` — reusable PCI/BAR0/BAR1 wrapper built on the executor.

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

- **Implementing the Win7 graphics stack:** target `drivers/aerogpu/protocol/*` and the canonical stack (`crates/aero-machine` + `crates/aero-devices-gpu` + host-side executors). `crates/emulator` is a legacy/sandbox integration surface.
- **Working on early/prototype plumbing:** the toy protocols are fine, but keep them clearly
  labeled as prototypes.

Note: an older prototype Win7 driver tree existed during early bring-up; it used stale placeholder PCI IDs
(vendor 1AE0) and was not WOW64-complete on Win7 x64. It is archived under
`prototype/legacy-win7-aerogpu-1ae0/` to avoid accidental installs; use `drivers/aerogpu/packaging/win7/`
for the supported Win7 driver package.
