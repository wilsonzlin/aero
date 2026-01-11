<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd PCI hardware IDs (HWIDs)

This driver targets **virtio-snd over PCI** (for example QEMU’s `virtio-sound-pci` device).

The Windows INF must match the correct PCI vendor/device IDs so that Windows 7 will bind the driver.

## Sources (clean-room)

* **Virtio Specification** → *PCI bus binding* → “PCI Device IDs” table for vendor **`0x1AF4`**
  (virtio) and virtio device type **`VIRTIO_ID_SOUND`**.
* **Aero Windows device contract** → `docs/windows-device-contract.md` and
  `docs/windows-device-contract.json` (stable PCI IDs + INF naming for Aero’s Windows drivers).
* **QEMU** (runtime verification) → QEMU monitor command `info pci` shows the currently-emitted
  `vendor:device` ID for each `-device ...` option.

## Confirmed IDs

Vendor ID: **`VEN_1AF4`**

| Variant | PCI Device ID | Windows HWID prefix | Notes |
| --- | --- | --- | --- |
| Modern / non-transitional (**contract v1**) | **`DEV_1059`** | `PCI\VEN_1AF4&DEV_1059` | `0x1059 = 0x1040 + 0x19` (virtio device id 25 / `VIRTIO_ID_SOUND`). |
| Transitional (legacy+modern) | **`DEV_1018`** | `PCI\VEN_1AF4&DEV_1018` | `0x1018 = 0x1000 + 0x18` (legacy virtio device id `0x18`). |

## Aero contract v1 expectations (subsystem + revision)

The Aero Windows device/driver contract uses stable subsystem IDs derived from the virtio device
type, and encodes the contract major version in the PCI **Revision ID**:

* Revision ID: `REV_01` (Aero virtio contract v1)
* Subsystem Vendor ID: `0x1AF4`
* Subsystem Device ID: `0x0019` (`VIRTIO_ID_SOUND` / 25)

So, a fully-qualified expected HWID looks like:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

The Aero driver INF (`inf/aero-virtio-snd.inf`) includes both:

* **Revision-gated** forms (e.g. `PCI\VEN_1AF4&DEV_1059&REV_01`)
* **Non-revision-gated** forms (e.g. `PCI\VEN_1AF4&DEV_1059`)

And it matches both the modern (`DEV_1059`) and transitional (`DEV_1018`) virtio-snd PCI IDs.

This keeps development installs simple. For strict Aero contract-v1 binding you can tighten the
INF to only keep revision- and subsystem-qualified entries, for example:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

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

Many QEMU device models report `REV_00` by default. The Aero INF also includes short forms without
`REV_01`, so it can bind to stock QEMU for bring-up.

If you see `REV_00` in Device Manager → Hardware Ids, you have a few options:

* If your QEMU build supports overriding PCI identification fields, set the revision/subsystem to
  match the Aero contract v1 values. For Revision ID specifically, many QEMU builds expose
  `x-pci-revision`:
  ```bash
  -device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
  ```
  (You can confirm supported properties with `qemu-system-x86_64 -device virtio-sound-pci,help`.)
* If you want strict Aero contract v1 conformance, configure QEMU to report `REV_01` (for example
  via `x-pci-revision=0x01`) and tighten the INF to require the revision-gated match.

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
  `REV_...`). The Aero INF includes both revision-gated and non-revision-gated forms; if you want
  strict contract-v1 binding, keep only the `REV_01` entries.
* The transitional ID `PCI\VEN_1AF4&DEV_1018` exists in the virtio spec. The Aero INF matches both
  the modern and transitional ID spaces for compatibility. If you want to validate the modern ID
  space specifically, use `disable-legacy=on` and confirm Windows enumerates `DEV_1059` (and
  optionally `REV_01`).
