# AeroGPU PCI identity (VID/DID) and ABI generations

AeroGPU is a paravirtual PCI display controller used inside the Aero emulator.
For AeroGPU, the PCI **Vendor ID / Device ID pair is part of the ABI contract**:
a Windows driver should only bind to a device model that implements the matching
MMIO + ring protocol.

For the repo-wide “what’s implemented vs what’s missing (Win7 UX)” checklist, see:
[`docs/graphics/status.md`](../graphics/status.md).

## Current status in `aero_machine::Machine`

The canonical full-system machine (`crates/aero-machine`, `aero_machine::Machine`) reserves
`00:07.0` for the **AeroGPU** PCI identity (`VID:DID = A3A0:0001`).

The canonical machine supports **two mutually-exclusive** display configurations:

- `MachineConfig::enable_aerogpu=true`: expose the canonical AeroGPU PCI identity at `00:07.0`
  (`A3A0:0001`) with the canonical BAR layout (BAR0 regs + BAR1 VRAM aperture).

  Note: in `aero_machine`, `enable_aerogpu` requires `enable_pc_platform=true` (the PCI bus must be
  present) and is mutually exclusive with `enable_vga`.

  In `aero_machine` today this provides:

  - **BAR1 VRAM aperture:** backed by a dedicated VRAM buffer for legacy VGA/VBE boot display
    compatibility, with permissive legacy VGA decode (VGA port I/O + VRAM-backed
    `0xA0000..0xBFFFF` window; see `docs/16-aerogpu-vga-vesa-compat.md`).
    - Note: in `wasm32` builds, the host-side BAR1 backing allocation is capped at 32MiB to fit
      browser heap constraints. The guest-visible PCI BAR still reports the full VRAM aperture size;
      reads return zero and writes are ignored beyond the backing allocation.
  - **WDDM memory model:** the in-tree Win7 AeroGPU driver treats the adapter as
    system-memory-backed (no dedicated VRAM segment). BAR1 exists for VGA/VBE compatibility and is
    outside the WDDM ABI (see `docs/graphics/win7-wddm11-aerogpu-driver.md`).
  - **BAR0 regs:** a minimal implementation of the versioned AeroGPU MMIO surface:
    - ring/fence transport (submission decode + fence-page/IRQ plumbing). Default bring-up behavior
      can complete fences without executing the command stream; browser/WASM runtimes can enable an
      out-of-process “submission bridge” (`Machine::aerogpu_drain_submissions` +
      `Machine::aerogpu_complete_fence`) so the GPU worker can execute submissions and report fence
      completion (see [`docs/graphics/aerogpu-executor-modes.md`](../graphics/aerogpu-executor-modes.md)
      for the executor/fence completion modes), and
    - scanout0 register storage + vblank timing/IRQ semantics (per `drivers/aerogpu/protocol/vblank.md`;
      vblank time is a monotonic “nanoseconds since boot” value) so `Machine::display_present` can
      present the WDDM scanout framebuffer by reading its guest physical address from guest memory.

  The shared device-side library `crates/aero-devices-gpu` contains a reusable PCI wrapper + ring
  executor and can be paired with host-side backends for real **command execution** (feature-gated).
  A legacy sandbox integration surface also exists in `crates/emulator`. The reusable
  `aero-devices-gpu` PCI wrapper/executor is not yet the canonical `aero_machine::Machine`
  integration (see: [`21-emulator-crate-migration.md`](../21-emulator-crate-migration.md)); the
  canonical browser runtime instead uses the machine’s submission bridge + the JS/WASM GPU worker
  executor. Native/test builds can also install an in-process backend. See:
  [`docs/graphics/status.md`](../graphics/status.md).

  When the AeroGPU-owned VGA/VBE boot display path is active, firmware derives the VBE linear
  framebuffer base from AeroGPU BAR1: `PhysBasePtr = BAR1_BASE + 0x40000`
  (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; see `crates/aero-machine/src/lib.rs::VBE_LFB_OFFSET`).
- `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`): provide boot display via the
  standalone `aero_gpu_vga` VGA/VBE implementation.
  - When `MachineConfig::enable_pc_platform=false`, the machine maps the LFB MMIO aperture directly
    at `MachineConfig::vga_lfb_base` (defaulting to `0xE000_0000` / `aero_gpu_vga::SVGA_LFB_BASE`).
  - When `MachineConfig::enable_pc_platform=true`, the canonical machine maps the LFB MMIO aperture
    through a transitional Bochs/QEMU-compatible “Standard VGA” PCI function (`1234:1111` at `00:0c.0`;
    see `aero_devices::pci::profile::VGA_TRANSITIONAL_STUB`). The VBE LFB is exposed via BAR0 inside
    the PCI MMIO window / BAR router; the BAR base is assigned by BIOS POST / the PCI resource
    allocator (and may vary when other PCI devices are present) unless pinned via
    `MachineConfig::{vga_lfb_base,vga_vram_bar_base}`. The machine mirrors the assigned BAR base into
    the BIOS VBE `PhysBasePtr` and the VGA device model so mode info and MMIO routing remain
    coherent.
  - Note: This transitional stub is not installed when AeroGPU is enabled, to avoid exposing two
    VGA-class PCI devices.

See also:

- [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md) (desired VGA/VBE-compat
  boot display behavior of AeroGPU)
- [`docs/pci-device-compatibility.md`](../pci-device-compatibility.md) (canonical BDF/ID table)

This repo currently has **two AeroGPU ABI generations**:

- the **versioned ABI** (canonical / current), and
- a **legacy bring-up ABI** (retired; retained for optional compatibility/regression testing).

## Canonical PCI IDs (source of truth)

| ABI generation | PCI IDs | Header (source of truth) | Host device model |
|---|---:|---|---|
| New, versioned ABI | `VID=0xA3A0, DID=0x0001` (`PCI\VEN_A3A0&DEV_0001`) | `drivers/aerogpu/protocol/aerogpu_pci.h` (+ `aerogpu_ring.h`, `aerogpu_cmd.h`) | `crates/aero-machine/src/aerogpu.rs` (canonical machine MVP); `crates/aero-devices-gpu/src/pci.rs` (shared); `crates/emulator/src/devices/pci/aerogpu.rs` (legacy integration surface) |
| Legacy bring-up ABI (deprecated) | `VID=0x1AED, DID=0x0001` (`PCI\VEN_1AED&DEV_0001`) | `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h` | `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (feature `emulator/aerogpu-legacy`) |

## PCI class identity (base class / subclass / prog-if)

Both AeroGPU ABIs identify as a VGA-compatible display controller so that Windows 7 will bind the WDDM stack:

| Field | Value | Protocol constant (versioned ABI) |
|---|---:|---|
| Base class | `0x03` | `AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER` |
| Subclass | `0x00` | `AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE` |
| Prog-IF | `0x00` | `AEROGPU_PCI_PROG_IF` |

Source of truth for these values is `drivers/aerogpu/protocol/aerogpu_pci.h` (mirrored in the canonical `aero-protocol` crate).

Notes:

* These PCI IDs are **project-local** and are **not assigned by PCI‑SIG**. They
  are only intended for use inside the Aero emulator.
* The device ID remains `0x0001` for both generations; the vendor ID is what
  distinguishes the ABI.

## PCI interrupt wiring (legacy INTx)

The canonical `aero_machine::Machine` exposes AeroGPU's interrupt delivery via **PCI INTx**:

- `Interrupt Pin` (0x3D) = `1` (**INTA#**)
- `Interrupt Line` (0x3C) = the routed platform interrupt line per Aero's canonical PCI INTx router
  (swizzle + PIRQ→GSI mapping).

For the canonical AeroGPU BDF (`00:07.0`) and the default PC-compatible routing table
(`PIRQ[A-D] → GSI[10-13]`), this evaluates to **GSI/IRQ 13**.

This contract is enforced by integration tests under `crates/aero-machine/tests/` (for example
`aerogpu_pci_enumeration.rs`).

## Why two ABIs exist

`drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h` is the original, minimal ABI used to bring up the Windows 7
WDDM stack.

It is retained for optional compatibility/bring-up and regression testing:

- The canonical Win7 AeroGPU INFs (`drivers/aerogpu/packaging/win7/{aerogpu.inf,aerogpu_dx11.inf}`) bind only to the
  versioned ABI identity (`PCI\VEN_A3A0&DEV_0001`).
  - For the legacy bring-up identity (`PCI\VEN_1AED&DEV_0001`), use the legacy INFs under
    `drivers/aerogpu/packaging/win7/legacy/`.
    - CI packages / Guest Tools stage legacy binding INFs under `legacy/` (sourced from `drivers/aerogpu/legacy/`):
      - `legacy/aerogpu.inf` (D3D9-only)
      - `legacy/aerogpu_dx11.inf` (DX11-capable; optional/opt-in)
- The emulator's legacy device model is feature-gated behind `emulator/aerogpu-legacy`.

`drivers/aerogpu/protocol/aerogpu_pci.h` is the newer, versioned ABI intended
to become the long-term stable contract. New development should target this ABI
and its PCI IDs.

## Windows driver packaging

The supported Windows 7 driver package lives under:

* `drivers/aerogpu/packaging/win7/` (see its `README.md`)

The in-tree Win7 AeroGPU INFs (`aerogpu.inf`, `aerogpu_dx11.inf`) bind to the canonical `A3A0:0001` (`PCI\VEN_A3A0&DEV_0001`)
HWID only. The Win7 KMD still has a compatibility path for the deprecated legacy bring-up ABI (`1AED:0001` / `PCI\VEN_1AED&DEV_0001`),
but the canonical INFs intentionally do not match it.

If you need the legacy device model, use the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/` (see its
`README.md`) and enable the legacy emulator device model feature (`emulator/aerogpu-legacy`).

An older AeroGPU driver stack existed during early bring-up; it is **not** the supported driver
package and was not WOW64-complete on Win7 x64. It is archived under
`prototype/legacy-win7-aerogpu-1ae0/` for reference only.

## Related docs

* `docs/windows-device-contract.md` – normative Windows driver/device binding contract for all paravirtual devices (including AeroGPU).

## Deprecated / stale IDs

The repo also contains **non-canonical prototypes** that use different PCI IDs. These are not
part of the Windows 7 WDDM AeroGPU ABI contract and must not be used for current device models,
driver packages, or documentation:

* A retired experimental prototype ABI existed for deterministic host-side tests and gpu-trace plumbing.
  The implementation has been removed; see `docs/legacy/experimental-gpu-command-abi.md` for a brief note.

Older prototypes in this repository used a different PCI vendor ID (1AE0). These identifiers are
**stale** and must not be used for current device models, driver packages, or documentation.
