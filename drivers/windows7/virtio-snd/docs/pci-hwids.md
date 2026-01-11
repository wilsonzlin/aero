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

The shipped INF (`aero-virtio-snd.inf`; in-repo source: `inf/aero-virtio-snd.inf`) intentionally
matches only the **AERO-W7-VIRTIO v1** modern virtio-snd PCI function:

- **Required (as shipped):** `PCI\VEN_1AF4&DEV_1059&REV_01`
- **Optional (commented out in the INF):** `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

It does **not** match:

- Transitional virtio-snd (`PCI\VEN_1AF4&DEV_1018`)
- Any non-revision-gated “short forms” (for example `PCI\VEN_1AF4&DEV_1059`)

## Related virtio-snd PCI device IDs (reference)

| Variant | PCI Device ID | Windows HWID prefix | Notes |
| --- | --- | --- | --- |
| Modern / non-transitional (**contract v1**) | **`DEV_1059`** | `PCI\VEN_1AF4&DEV_1059` | `0x1059 = 0x1040 + 0x19` (virtio device id 25 / `VIRTIO_ID_SOUND`). |
| Transitional (legacy+modern) | **`DEV_1018`** | `PCI\VEN_1AF4&DEV_1018` | Transitional virtio-pci IDs use `0x1000 + (virtio_device_id - 1)` → `0x1018 = 0x1000 + (0x19 - 1)`. |

## Aero contract v1 expectations (subsystem + revision)

The Aero Windows device/driver contract uses stable subsystem IDs derived from the virtio device
type, and encodes the contract major version in the PCI **Revision ID**:

* Revision ID: `REV_01` (Aero virtio contract v1)
* Subsystem Vendor ID: `0x1AF4`
* Subsystem Device ID: `0x0019` (`VIRTIO_ID_SOUND` / 25)

So, a fully-qualified expected HWID looks like:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

Note: `docs/windows-device-contract.json` lists **both** revision-gated and
non-revision-gated patterns for tooling convenience. Automation (Guest Tools,
CI) should prefer the revision-gated forms (`...&REV_01`) as described in
`docs/windows-device-contract.md`.

The virtio-snd INF is intentionally strict and requires `REV_01`. CI packaging
stages only `inf/aero-virtio-snd.inf` (see `ci-package.json`) to avoid shipping
multiple INFs that match the same contract-v1 device IDs.

For QEMU bring-up (which is typically transitional by default), use the opt-in legacy package:

* `inf/aero-virtio-snd-legacy.inf` → `PCI\VEN_1AF4&DEV_1018` (no `REV_01` requirement) + `virtiosnd_legacy.sys`

The repository also contains an optional **legacy filename alias** INF (`inf/virtio-snd.inf.disabled`).
If you rename it back to `virtio-snd.inf` (and regenerate/sign `virtio-snd.cat`), it installs the same
driver/service as `aero-virtio-snd.inf` and matches the same contract-v1 HWIDs, but provides the legacy
filename for compatibility with older tooling/workflows.

## QEMU mapping

QEMU may expose the virtio-snd PCI frontend under one of these device names:

* `-device virtio-sound-pci` (common upstream name)
* `-device virtio-snd-pci` (alias on some builds)

### Modern-only vs transitional in QEMU

Many QEMU virtio PCI devices enumerate as **transitional** by default. For virtio-snd this typically
means Windows will see `DEV_1018` unless you explicitly disable legacy mode. The
shipped Aero virtio-snd contract INF requires the modern ID space (`DEV_1059`). To get `DEV_1059`
under QEMU, use:

```bash
-device virtio-sound-pci,disable-legacy=on
```

Note: the strict Aero contract v1 INF is also revision-gated (`REV_01`); see the next section for
`x-pci-revision=0x01` when validating the contract-v1 identity under QEMU.

If you just want to run with stock QEMU defaults (transitional `DEV_1018`), install the legacy package
(`inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`) instead.

### QEMU vs Aero contract v1 (REV_01)

Many QEMU device models report `REV_00` by default. The shipped Aero INF is revision-gated
(`REV_01`) and the driver validates the Revision ID at runtime, so it will not bind to stock QEMU
unless you override the revision.

When testing under QEMU, prefer:

```bash
-device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
```

If you see `REV_00` in Device Manager → Hardware Ids, you have a few options:

* If your QEMU build supports overriding PCI identification fields, set the revision/subsystem to
  match the Aero contract v1 values. For Revision ID specifically, many QEMU builds expose
  `x-pci-revision`:
  ```bash
  -device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
  ```
  (You can confirm supported properties with `qemu-system-x86_64 -device virtio-sound-pci,help`.)
* If your QEMU build does **not** support overriding the PCI Revision ID (or you just want stock QEMU
  defaults), install the legacy package (`inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys`).

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
  `REV_...`). The Aero contract-v1 INF is strict and requires `PCI\VEN_1AF4&DEV_1059&REV_01`.
* For transitional QEMU devices (`DEV_1018`, typically `REV_00`), use the legacy INF/package.
