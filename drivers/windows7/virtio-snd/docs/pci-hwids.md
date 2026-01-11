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
* Subsystem Device ID: `0x0019` (`VIRTIO_ID_SOUND`)

So, a fully-qualified expected HWID looks like:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

The Aero driver INF is intentionally **revision-gated** so it will not bind to devices that do not
claim to implement contract v1:

* `PCI\VEN_1AF4&DEV_1059&REV_01`

For additional safety you can also match the subsystem-qualified form, but that is currently
commented out in the INF:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00031AF4&REV_01`

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

### QEMU vs Aero contract v1 (REV_01)

The Aero INF matches `REV_01` (contract v1). Many QEMU device models report `REV_00` by default, so
Windows will not bind the Aero driver unless the device reports `REV_01`.

If you see `REV_00` in Device Manager → Hardware Ids, you have a few options:

* If your QEMU build supports overriding PCI identification fields, set the revision/subsystem to
  match the Aero contract v1 values. (Some builds expose `x-pci-*` properties; consult
  `qemu-system-x86_64 -device virtio-sound-pci,help`.)
* Alternatively, for bring-up on stock QEMU you can temporarily loosen the INF match to drop the
  `&REV_01` constraint (not recommended for Aero contract conformance).

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
  `REV_...`). The Aero INF matches the revision-gated form `PCI\VEN_1AF4&DEV_1059&REV_01`.
* The transitional ID `PCI\VEN_1AF4&DEV_1018` exists in the virtio spec, but **Aero contract v1 is
  modern-only** (use `disable-legacy=on` and validate that Windows sees `DEV_1059`).
