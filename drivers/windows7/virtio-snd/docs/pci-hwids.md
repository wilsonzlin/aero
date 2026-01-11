# virtio-snd PCI hardware IDs (HWIDs)

This driver targets **virtio-snd over PCI** (for example QEMU’s `virtio-sound-pci` device).

The Windows INF must match the correct PCI vendor/device IDs so that Windows 7 will bind the driver.

## Sources (clean-room)

* **Virtio Specification** → *PCI bus binding* → “PCI Device IDs” table for vendor **`0x1AF4`**
  (virtio) and virtio device type **`VIRTIO_ID_SOUND`**.
* **Aero virtio contract** → `docs/windows7-virtio-driver-contract.md` (contract v1 is
  modern-only).
* **QEMU** (runtime verification) → QEMU monitor command `info pci` shows the currently-emitted
  `vendor:device` ID for each `-device ...` option.

## Confirmed IDs

Vendor ID: **`VEN_1AF4`**

| Variant | PCI Device ID | Windows HWID prefix | Notes |
| --- | --- | --- | --- |
| Modern / non-transitional (**contract v1**) | **`DEV_1059`** | `PCI\VEN_1AF4&DEV_1059` | `0x1059 = 0x1040 + 0x19` (virtio device id 25 / `VIRTIO_ID_SOUND`). |
| Transitional (legacy+modern) | **`DEV_1018`** | `PCI\VEN_1AF4&DEV_1018` | `0x1018 = 0x1000 + 0x18` (legacy virtio device id `0x18`). |

## Aero contract v1 expectations (subsystem + revision)

The Aero Windows 7 virtio contract v1 encodes the contract major version in the PCI **Revision ID**
and assigns subsystem IDs per device instance:

* Revision ID: `REV_01` (contract v1)
* Subsystem Vendor ID: `0x1AF4`
* Subsystem Device ID: `0x0003` (virtio-snd instance)

So, a fully-qualified expected HWID looks like:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00031AF4&REV_01`

The INF should still match the **short** `PCI\VEN_1AF4&DEV_1059` form so that it can bind even if a
different hypervisor uses different subsystem IDs/revision IDs.

## QEMU mapping

QEMU may expose the virtio-snd PCI frontend under one of these device names:

* `-device virtio-sound-pci` (common upstream name)
* `-device virtio-snd-pci` (alias on some builds)

### Modern-only vs transitional in QEMU

Many QEMU virtio PCI devices enumerate as **transitional** by default. For virtio-snd this typically
means Windows will see `DEV_1018` unless you explicitly disable legacy mode:

```bash
-device virtio-sound-pci,disable-legacy=on
```

### Verify the emitted PCI ID (no guest required)

```bash
printf 'info pci\nquit\n' | \
  qemu-system-x86_64 -nodefaults -machine q35 -m 128 -nographic -monitor stdio \
    -device virtio-sound-pci,disable-legacy=on
```

Expected `info pci` line (device ID may be shown in lowercase):

```
Audio: PCI device 1af4:1059
```

## Windows 7 caveats

* The “Hardware Ids” list in Device Manager includes more-specific forms (with `SUBSYS_...` and
  `REV_...`). The INF should match at least the short form `PCI\VEN_1AF4&DEV_1059`.
* The transitional ID `PCI\VEN_1AF4&DEV_1018` may be kept in the INF to ease bring-up on QEMU
  defaults, but **Aero contract v1 is modern-only** (use `disable-legacy=on` and validate `DEV_1059`
  when testing contract conformance).
