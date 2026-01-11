# AeroGPU PCI identity (VID/DID) and ABI generations

AeroGPU is a paravirtual PCI display controller used inside the Aero emulator.
For AeroGPU, the PCI **Vendor ID / Device ID pair is part of the ABI contract**:
a Windows driver should only bind to a device model that implements the matching
MMIO + ring protocol.

This repo currently has **two canonical AeroGPU ABIs**, each with its own PCI
identity.

## Canonical PCI IDs (source of truth)

| ABI generation | PCI IDs | Header (source of truth) | Host device model |
|---|---:|---|---|
| New, versioned ABI | `VID=0xA3A0, DID=0x0001` (`PCI\VEN_A3A0&DEV_0001`) | `drivers/aerogpu/protocol/aerogpu_pci.h` (+ `aerogpu_ring.h`) | `crates/emulator/src/devices/pci/aerogpu.rs` |
| Legacy bring-up ABI | `VID=0x1AED, DID=0x0001` (`PCI\VEN_1AED&DEV_0001`) | `drivers/aerogpu/protocol/aerogpu_protocol.h` | `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (feature `emulator/aerogpu-legacy`) |

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

`drivers/aerogpu/protocol/aerogpu_protocol.h` is the original, minimal ABI used
to bring up the Windows 7 WDDM stack. It still exists because there is
guest-side code that speaks it and the emulator still has a compatible device
model (`aerogpu_legacy.rs`, behind the `emulator/aerogpu-legacy` feature).

`drivers/aerogpu/protocol/aerogpu_pci.h` is the newer, versioned ABI intended
to become the long-term stable contract. New development should target this ABI
and its PCI IDs.

## Windows driver packaging

The supported Windows 7 driver package lives under:

* `drivers/aerogpu/packaging/win7/` (see its `README.md`)

The in-tree Win7 AeroGPU INFs (`aerogpu.inf`, `aerogpu_dx11.inf`) match both the canonical
`A3A0:0001` HWID and the legacy bring-up `1AED:0001` HWID to ease migration/bring-up; `A3A0:0001`
remains the canonical ABI identity.

An older AeroGPU driver stack existed during early bring-up; it is **not** the supported driver
package and was not WOW64-complete on Win7 x64. It is archived at
`prototype/legacy-win7-aerogpu-1ae0/guest/windows/` for reference only.

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
