# virtio-input PCI hardware IDs (HWIDs)

This driver targets **virtio-input over PCI** (e.g. QEMU’s `virtio-keyboard-pci`,
`virtio-mouse-pci`, and `virtio-tablet-pci`).

The Windows INF needs to match the correct PCI vendor/device IDs so that Windows 7
will bind the driver automatically when a virtio-input device is present.

## Sources (clean-room)

* **Virtio Specification** → *PCI bus binding* → “PCI Device IDs” table for
  vendor **`0x1AF4`** (Red Hat) and **virtio device type `VIRTIO_ID_INPUT`**.
* **Aero Windows virtio contract (definitive)** → `docs/windows7-virtio-driver-contract.md`
  (PCI identity rules; subsystem IDs for keyboard vs mouse; Revision ID policy).
* **Aero Windows device contract (tooling manifest)** → `docs/windows-device-contract.md` and
  `docs/windows-device-contract.json` (stable PCI IDs + strict HWID patterns for automation).
* **QEMU** (runtime verification) → QEMU monitor command `info pci` shows the
  currently-emitted `vendor:device` IDs for each `-device ...` option.

## Confirmed IDs

Vendor ID: **`VEN_1AF4`**

| Variant | PCI Device ID | Windows HWID prefix | Notes |
| --- | --- | --- | --- |
| Modern / non-transitional | **`DEV_1052`** | `PCI\VEN_1AF4&DEV_1052` | Matches virtio device type **18 / `0x12`** (`VIRTIO_ID_INPUT`). |
| Transitional (legacy+modern) | **`DEV_1011`** | `PCI\VEN_1AF4&DEV_1011` | Virtio “transitional” PCI ID for virtio-input (per virtio spec table). |

### Relationship to virtio-input type ID

Virtio uses device type **18 (`0x12`)** for input. The corresponding PCI device
ID used by modern/non-transitional virtio-input is:

* `0x1052 = 0x1040 + 0x12`

The virtio spec also defines a **transitional** (legacy+modern) PCI device ID
for virtio-input:

* `0x1011 = 0x1000 + 0x11` (legacy virtio device ID `0x11`)

## QEMU mapping

QEMU provides multiple PCI device frontends that all represent the same underlying
virtio-input device type:

* `-device virtio-keyboard-pci`
* `-device virtio-mouse-pci`
* `-device virtio-tablet-pci`

### QEMU 8.2.x behavior (observed)

These devices currently enumerate as **modern/non-transitional** virtio-input:

* `PCI\VEN_1AF4&DEV_1052` (and a `SUBSYS_11001AF4...` variant)
* Changing `disable-legacy=` / `disable-modern=` does **not** change the PCI ID;
  QEMU’s virtio-input PCI devices are effectively modern-only today.

To verify without a guest OS, run:

```bash
printf 'info pci\nquit\n' | \
  qemu-system-x86_64 -nodefaults -machine q35 -m 128 -nographic -monitor stdio \
    -device virtio-keyboard-pci
```

Expected `info pci` line (device ID may be shown in lowercase):

```
Keyboard: PCI device 1af4:1052
```

## Windows 7 caveats

* Windows 7 will show the device as an unknown PCI device until a matching driver
  is installed.
  * The “Hardware Ids” list in Device Manager includes more-specific forms (with
  `SUBSYS_...` and `REV_...`). The in-tree Aero INFs intentionally match only
  **Aero contract v1** hardware IDs (revision-gated `REV_01`):
  - `inf/aero_virtio_input.inf` (keyboard/mouse; canonical):
    - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01` (keyboard; **Aero VirtIO Keyboard**)
    - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01` (mouse; **Aero VirtIO Mouse**)
    - Note: canonical INF is intentionally **SUBSYS-only** (no strict generic fallback).
  - `inf/aero_virtio_tablet.inf` (tablet / absolute pointer; **Aero VirtIO Tablet Device**):
    - `PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01`
  - Optional legacy filename alias `inf/virtio-input.inf.disabled` (disabled by default; rename to `virtio-input.inf` to enable):
    - Compatibility filename for workflows/tools that still reference `virtio-input.inf`.
    - Adds the strict revision-gated generic fallback HWID (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`
      (**Aero VirtIO Input Device**).
    - Alias drift policy: allowed to diverge from `inf/aero_virtio_input.inf` only in the models sections (`[Aero.NTx86]` /
      `[Aero.NTamd64]`) where it adds the fallback entry. Outside those sections, from the first section header (`[Version]`)
      onward, it is expected to remain byte-for-byte identical (banner/comments may differ; see `../scripts/check-inf-alias.py`).
    - Enabling it does **change** HWID matching behavior (it enables fallback binding when Aero `SUBSYS_` IDs are not exposed/recognized).
    - Do **not** ship/install it alongside `aero_virtio_input.inf` (install only one of the two filenames at a time).
  Tablet devices bind via `inf/aero_virtio_tablet.inf` when that INF matches: its HWID is more specific (`SUBSYS_0012...`),
  so it wins over the generic fallback when both driver packages are installed (i.e. when the opt-in fallback alias is enabled).
* Aero’s Win7 virtio contract encodes the contract major version in the PCI Revision
  ID (contract v1 = `REV_01`). Some QEMU virtio devices report `REV_00` by default;
  for contract testing, use `x-pci-revision=0x01` on the QEMU `-device ...` args.

## Aero contract v1 expectations (keyboard vs mouse subsystem + revision)

`AERO-W7-VIRTIO` v1 exposes **two** virtio-input PCI functions (keyboard + mouse)
as a **single multi-function PCI device** (same slot, functions 0 and 1). It uses
the PCI Subsystem Device ID to distinguish keyboard vs mouse:

* Vendor/Device: `PCI\VEN_1AF4&DEV_1052`
* Revision ID: `REV_01`
* Subsystem vendor: `0x1AF4`
* Subsystem device:
  * keyboard: `0x0010` → `SUBSYS_00101AF4`
  * mouse: `0x0011` → `SUBSYS_00111AF4`
  * (optional) tablet / absolute pointer: `0x0012` → `SUBSYS_00121AF4`

The in-tree Win7 virtio-input INFs use these subsystem-qualified HWIDs to assign **distinct Device Manager names**:

- `SUBSYS_00101AF4` → **Aero VirtIO Keyboard** (`inf/aero_virtio_input.inf`)
- `SUBSYS_00111AF4` → **Aero VirtIO Mouse** (`inf/aero_virtio_input.inf`)
- `SUBSYS_00121AF4` → **Aero VirtIO Tablet Device** (`inf/aero_virtio_tablet.inf`)

The canonical keyboard/mouse INF (`inf/aero_virtio_input.inf`) is intentionally **SUBSYS-only** (no strict generic fallback model line).

Strict generic fallback binding (no `SUBSYS`) is **opt-in** via the legacy filename alias INF (`inf/virtio-input.inf.disabled` → rename
to `virtio-input.inf` to enable):

- `PCI\VEN_1AF4&DEV_1052&REV_01` → **Aero VirtIO Input Device** (when binding via the fallback entry)

Tablet devices bind via `inf/aero_virtio_tablet.inf` when that INF matches. The tablet HWID is more specific (`SUBSYS_0012...`),
so it wins over the generic fallback when both driver packages are installed. If the tablet INF is not installed (or the device does not
expose the tablet subsystem ID), the generic fallback entry (when enabled via the alias INF) can also bind to tablet devices (but will
use the generic device name).

For compatibility with tooling that still expects `virtio-input.inf`, the repo also carries a legacy filename alias INF
(`inf/virtio-input.inf.disabled`, rename to `virtio-input.inf` to enable). It is allowed to diverge from the canonical INF only in the
models sections where it adds the opt-in fallback entry; outside those sections, from the first section header (`[Version]`) onward it is
expected to remain byte-for-byte identical (banner/comments may differ; see `../scripts/check-inf-alias.py`). Enabling it does change HWID
matching behavior.
Do not ship/install it alongside `aero_virtio_input.inf` (install only one of the two basenames at a time).

Topology notes:

* The keyboard must be **function 0** and must set the PCI multi-function bit
  (`header_type = 0x80`) so guests enumerate function 1.
* The mouse is **function 1** (`header_type = 0x00`).

`docs/windows-device-contract.json` carries both strict and convenience HWID
patterns; automation should prefer the strict `...&SUBSYS_...&REV_01` patterns
to avoid false positives against non-Aero virtio-input devices.
