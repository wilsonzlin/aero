# AeroGPU PCI identity (VID/DID) and ABI generations

AeroGPU is a paravirtual PCI display controller used inside the Aero emulator.
For AeroGPU, the PCI **Vendor ID / Device ID pair is part of the ABI contract**:
a Windows driver should only bind to a device model that implements the matching
MMIO + ring protocol.

## Current status in `aero_machine::Machine`

The canonical full-system machine (`crates/aero-machine`, `aero_machine::Machine`) reserves
`00:07.0` for the **AeroGPU** PCI identity (`VID:DID = A3A0:0001`).

Today, `aero_machine::Machine` does **not** yet wire up the full AeroGPU WDDM device model. Boot
display is provided by the separate `aero_gpu_vga` VGA/VBE implementation, plus a minimal
Bochs/QEMU “Standard VGA”-like PCI stub at `00:0c.0` (`1234:1111`) used only to route the fixed VBE
linear framebuffer through the PCI MMIO window.

See also:

- [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md) (future desired
  VGA/VBE-compat behavior of AeroGPU itself)
- [`docs/pci-device-compatibility.md`](../pci-device-compatibility.md) (canonical BDF/ID table,
  including the transitional VGA stub)

This repo currently has **two AeroGPU ABI generations**:

- the **versioned ABI** (canonical / current), and
- a **legacy bring-up ABI** (retired; retained for optional compatibility/regression testing).

## Canonical PCI IDs (source of truth)

| ABI generation | PCI IDs | Header (source of truth) | Host device model |
|---|---:|---|---|
| New, versioned ABI | `VID=0xA3A0, DID=0x0001` (`PCI\VEN_A3A0&DEV_0001`) | `drivers/aerogpu/protocol/aerogpu_pci.h` (+ `aerogpu_ring.h`, `aerogpu_cmd.h`) | `crates/emulator/src/devices/pci/aerogpu.rs` |
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
