# virtio-input PCI hardware IDs (HWIDs)

This driver targets **virtio-input over PCI** (e.g. QEMU’s `virtio-keyboard-pci`,
`virtio-mouse-pci`, and `virtio-tablet-pci`).

The Windows INF needs to match the correct PCI vendor/device IDs so that Windows 7
will bind the driver automatically when a virtio-input device is present.

## Sources (clean-room)

* **Virtio Specification** → *PCI bus binding* → “PCI Device IDs” table for
  vendor **`0x1AF4`** (Red Hat) and **virtio device type `VIRTIO_ID_INPUT`**.
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
  `SUBSYS_...` and `REV_...`). The INF should match at least the short form
  `PCI\VEN_1AF4&DEV_1052` (and optionally `...DEV_1011` for transitional
  implementations).
* Aero’s Win7 virtio contract encodes the contract major version in the PCI Revision
  ID (contract v1 = `REV_01`). Some QEMU virtio devices report `REV_00` by default;
  for contract testing, use `x-pci-revision=0x01` on the QEMU `-device ...` args.
