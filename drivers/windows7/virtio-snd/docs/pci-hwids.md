<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd PCI hardware IDs (HWIDs)

This driver package targets **virtio-snd over PCI** and is versioned/validated by
the definitive Aero Windows 7 virtio device contract:

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md#34-virtio-snd-audio) (`AERO-W7-VIRTIO` v1)

For overall driver status and supported HWID binding rules, see:
[`docs/README.md`](README.md).

The Windows INF must match the correct PCI vendor/device IDs so that Windows 7 will bind the driver.

## Sources (clean-room)

* **Virtio Specification** → *PCI bus binding* → “PCI Device IDs” table for vendor **`0x1AF4`**
  (virtio) and virtio device type **`VIRTIO_ID_SOUND`**.
* **Aero Windows virtio contract (definitive)** → `docs/windows7-virtio-driver-contract.md`
  (queue sizes, feature bits, PCI identity rules).
* **Aero Windows device contract (tooling manifest)** → `docs/windows-device-contract.md` and
  `docs/windows-device-contract.json` (stable PCI IDs + INF naming for Aero’s Windows drivers).
* **QEMU** (runtime verification) → QEMU monitor command `info pci` shows the currently-emitted
  `vendor:device` ID for each `-device ...` option.

## HWIDs matched by the shipped INF

Vendor ID: **`VEN_1AF4`**

The shipped INF (`inf/aero-virtio-snd.inf`) intentionally matches only the
**AERO-W7-VIRTIO v1** modern virtio-snd PCI function:

- **Required (as-shipped):** `PCI\VEN_1AF4&DEV_1059&REV_01`
- **Optional (commented out in the INF):** `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

It does **not** match:

- Transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`)
- Any non-revision-gated “short forms” (for example `PCI\VEN_1AF4&DEV_1059`)

## Aero contract v1 expectations (subsystem + revision)

The Aero Windows device/driver contract uses stable subsystem IDs derived from the virtio device
type, and encodes the contract major version in the PCI **Revision ID**:

* Revision ID: `REV_01` (Aero virtio contract v1)
* Subsystem Vendor ID: `0x1AF4`
* Subsystem Device ID: `0x0019` (`VIRTIO_ID_SOUND` / 25)

So, a fully-qualified expected HWID looks like:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

The Aero driver INFs (`inf/aero-virtio-snd.inf` and the `inf/virtio-snd.inf` alias)
are intentionally **strict** for `AERO-W7-VIRTIO` v1 and match only:

* `PCI\VEN_1AF4&DEV_1059&REV_01`

Optional (commented out) tighter match:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

Note: `docs/windows-device-contract.json` currently lists non-revision-gated
patterns (`PCI\VEN_1AF4&DEV_1059` and `...&SUBSYS_00191AF4`) for tooling
convenience. The virtio-snd INF is intentionally stricter and requires `REV_01`.

They do **not** match transitional `DEV_1018` IDs and do not include non-`REV_01` short forms.

## QEMU mapping

QEMU may expose the virtio-snd PCI frontend under one of these device names:

* `-device virtio-sound-pci` (common upstream name)
* `-device virtio-snd-pci` (alias on some builds)

### Modern-only vs transitional in QEMU

Many QEMU virtio PCI devices enumerate as **transitional** by default. For virtio-snd this typically
means Windows will see `DEV_1018` unless you explicitly disable legacy mode. The
shipped Aero virtio-snd INF requires the modern ID space (`DEV_1059`), so you
must use:

```bash
-device virtio-sound-pci,disable-legacy=on
```

### QEMU vs Aero contract v1 (REV_01)

Many QEMU device models report `REV_00` by default. The Aero INFs require `REV_01`, so the driver
will not bind to stock QEMU unless you override the revision.

If you see `REV_00` in Device Manager → Hardware Ids, you have a few options:

* If your QEMU build supports overriding PCI identification fields, set the revision/subsystem to
  match the Aero contract v1 values. For Revision ID specifically, many QEMU builds expose
  `x-pci-revision`:
  ```bash
  -device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
  ```
  (You can confirm supported properties with `qemu-system-x86_64 -device virtio-sound-pci,help`.)
* If your QEMU build does **not** support overriding the PCI Revision ID, the stock Aero INF will
  not bind. Use a contract-v1-capable device model/hypervisor (or patch QEMU) for testing.

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
  `REV_...`). The Aero INF requires a `REV_01` match; if your device reports `REV_00`, the driver will
  not bind.
* The transitional ID `PCI\VEN_1AF4&DEV_1018` exists in the virtio spec. The Aero Win7 contract v1
  INF does **not** match it; if Windows shows `DEV_1018`, configure the hypervisor to expose a
  modern-only device (e.g. QEMU `disable-legacy=on`) so Windows enumerates `DEV_1059`.
