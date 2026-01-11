# Windows 7 virtio drivers

This directory contains Windows 7 SP1 virtio driver packages.

Some drivers here still use the **legacy/transitional** virtio PCI ID space
(`DEV_100x`). Others have been migrated to Aero’s **virtio contract v1**, which
requires the virtio-pci **modern** transport and the modern PCI Device ID space
(`DEV_104x`/`DEV_105x`).

- Contract: `docs/windows7-virtio-driver-contract.md`
- Contract-v1 drivers: `drivers/win7/virtio-blk/`, `drivers/win7/virtio-net/`, …
- Modern transport core library: `drivers/win7/virtio/virtio-core/`

## Why this matters (INF hardware ID conflicts)

Windows picks drivers by matching PCI hardware IDs against installed INFs. Aero’s
contract v1 uses modern-only IDs like:

- `PCI\\VEN_1AF4&DEV_1041` (virtio-net)
- `PCI\\VEN_1AF4&DEV_1042` (virtio-blk)

To avoid Windows seeing **multiple** INFs that match the **same** modern device,
make sure your driver set does **not** contain duplicate INFs that bind the
same modern IDs (for example, both `drivers/windows7/virtio/blk/aerovblk.inf`
and `drivers/win7/virtio-blk/aerovblk.inf`).

## Contents

- `common/` – shared virtio helpers (legacy transport + split-virtqueue library).
- `blk/` – `aerovblk` StorPort miniport (Aero contract v1; binds to `DEV_1042`).
- `net/` – `aerovnet` NDIS 6.20 miniport (legacy/transitional; binds to `DEV_1000`).

If you are implementing or packaging Aero’s contract-v1 drivers, do **not** use
this directory; use the contract-v1 driver directories instead.
