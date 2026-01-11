# Windows 7 virtio drivers

This directory contains Windows 7 SP1 virtio driver packages that implement Aero’s
**AERO-W7-VIRTIO contract v1** (virtio-pci **modern** transport + modern PCI Device ID
space `DEV_104x`/`DEV_105x`).

- Contract: `docs/windows7-virtio-driver-contract.md`
- Host/portable virtio helpers (cap parser tests, etc.): `drivers/win7/virtio/`

## Why this matters (INF hardware ID conflicts)

Windows picks drivers by matching PCI hardware IDs against installed INFs. Aero’s
contract v1 uses modern-only IDs like:

- `PCI\VEN_1AF4&DEV_1041&REV_01` (virtio-net)
- `PCI\VEN_1AF4&DEV_1042&REV_01` (virtio-blk)

To avoid Windows seeing **multiple** INFs that match the **same** modern device,
make sure your driver set does **not** contain duplicate INFs that bind the
same modern IDs.

If you have historical/experimental driver copies elsewhere in the repo (for example
under `drivers/win7/`), they must not ship an additional `.inf` that matches the
same contract-v1 HWIDs.

## Contents

- `common/` – shared Windows 7 virtio helpers (INTx helper, contract checks, legacy I/O-port transport, host-test helpers).
  - Note: the canonical virtio-pci modern transport and canonical split-ring virtqueue engine used by
    `blk/` + `net/` live under `drivers/windows/virtio/` (`pci-modern/`, `common/`).
- `blk/` – `aerovblk` StorPort miniport (Aero contract v1; binds to `PCI\VEN_1AF4&DEV_1042&REV_01`).
- `net/` – `aerovnet` NDIS 6.20 miniport (Aero contract v1; binds to `PCI\VEN_1AF4&DEV_1041&REV_01`).

If you are packaging Aero’s contract-v1 drivers, avoid staging multiple INFs that
bind the same modern HWIDs (or share the same basename) unless you explicitly
disambiguate by relative path during provisioning.
