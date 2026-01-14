<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# virtio-snd PCI hardware IDs (HWIDs)

This driver targets **Aero Windows 7 virtio contract v1** (`AERO-W7-VIRTIO`) and binds only to the
**modern** virtio-snd PCI function (`DEV_1059`) using the virtio-pci **modern** transport (PCI
vendor-specific capabilities + BAR0 MMIO).

Authoritative references:

- `docs/windows7-virtio-driver-contract.md` (definitive virtio transport + device contract)
- `drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf` (actual INF match strings)
- `docs/windows-device-contract.{md,json}` (consolidated IDs for Guest Tools; must remain consistent with `AERO-W7-VIRTIO`)

## Contract v1 PCI identity

- Vendor ID: `VEN_1AF4`
- Device ID: `DEV_1059` (virtio device id 25 / `VIRTIO_ID_SND`, modern virtio-pci ID space: `0x1040 + 25`)
- Revision ID: `REV_01` (contract major version encoding)
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0019` (virtio-snd / `VIRTIO_ID_SND`)

Windows will typically enumerate multiple hardware IDs, including more-specific forms such as:

- `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`
- `PCI\VEN_1AF4&DEV_1059&REV_01`
- `PCI\VEN_1AF4&DEV_1059`

## INF binding (Aero)

The shipped INF (`inf/aero_virtio_snd.inf`) is intentionally strict and matches only:

- `PCI\VEN_1AF4&DEV_1059&REV_01`

Optional (commented out in the INF): further restrict binding to Aero’s subsystem ID:

- `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01`

## QEMU notes

To validate contract-v1 driver binding under QEMU, your QEMU build must support:

- Modern-only virtio-pci mode:
  - `-device virtio-sound-pci,disable-legacy=on`
- Contract revision ID:
  - `x-pci-revision=0x01`

The Aero Windows device/driver contract uses stable subsystem IDs derived from the virtio device type and encodes the
contract major version in the PCI **Revision ID**. Automation (Guest Tools, CI) should prefer revision-gated forms
(`...&REV_01`) as described in `docs/windows-device-contract.md`, even though `docs/windows-device-contract.json` also
includes non-revision-gated patterns for tooling convenience.

CI packaging stages only `inf/aero_virtio_snd.inf` (see `ci-package.json`) to avoid shipping multiple INFs that match
the same HWIDs.

If your QEMU build cannot set both `disable-legacy=on` and `x-pci-revision=0x01`, it will not be able to exercise the
strict Aero INF match (`...&REV_01`) reliably. In that case, use the opt-in QEMU compatibility package instead:

- `inf/aero-virtio-snd-legacy.inf` + `virtiosnd_legacy.sys` (binds transitional virtio-snd: `PCI\VEN_1AF4&DEV_1018`)

If you need a legacy **I/O-port** transport driver (older bring-up), the tree also contains:

- `inf/aero-virtio-snd-ioport.inf` + `virtiosnd_ioport.sys` (matches `PCI\VEN_1AF4&DEV_1018&REV_00`)

The repository also contains an optional **legacy filename alias** INF (`inf/virtio-snd.inf.disabled`). If you rename
it back to `virtio-snd.inf`, it installs the same driver/service and matches the same contract-v1 HWIDs as
`aero_virtio_snd.inf`, but provides the legacy filename for compatibility with older tooling/workflows.

## QEMU mapping

QEMU may expose the virtio-snd PCI frontend under one of these device names:

- `-device virtio-sound-pci` (common upstream name)
- `-device virtio-snd-pci` (alias on some builds)

Example (contract v1 identity under QEMU):

```bash
-device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
```

### Verify the emitted PCI ID (no guest required)

```bash
printf 'info pci\nquit\n' | \
  qemu-system-x86_64 -nodefaults -machine q35 -m 128 -nographic -monitor stdio \
    -device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01
```

Expected `info pci` line (device ID may be shown in lowercase):

```
Audio: PCI device 1af4:1059
```

## Contract v1 summary (virtio-snd behavior)

- The “Hardware Ids” list in Device Manager includes more-specific forms (with `SUBSYS_...` and `REV_...`). The Aero
  contract-v1 INF is strict and requires `PCI\VEN_1AF4&DEV_1059&REV_01`.
- Transport: virtio-pci modern-only (vendor-specific caps + BAR0 MMIO)
- Required features: `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` only
- Queues: control/event/rx=64, tx=256
- Streams: stream0 render (stereo) and stream1 capture (mono); baseline S16 48k (device may advertise additional formats/rates via `PCM_INFO`)
