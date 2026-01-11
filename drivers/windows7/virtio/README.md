# Windows 7 virtio drivers (legacy/transitional)

This directory contains **legacy/transitional** Windows 7 SP1 virtio driver
packages that bind to the **transitional** virtio PCI Device ID space
(`DEV_100x`).

These drivers are **not** compliant with Aero’s **virtio contract v1**, which
requires the virtio-pci **modern** transport and the modern PCI Device ID space
(`DEV_104x`/`DEV_105x`):

- Contract: `docs/windows7-virtio-driver-contract.md`
- Contract-v1 drivers: `drivers/win7/virtio-blk/`, `drivers/win7/virtio-net/`, …
- Modern transport core library: `drivers/win7/virtio/virtio-core/`

## Why this matters (INF hardware ID conflicts)

Windows picks drivers by matching PCI hardware IDs against installed INFs. Aero’s
contract v1 uses modern-only IDs like:

- `PCI\\VEN_1AF4&DEV_1041` (virtio-net)
- `PCI\\VEN_1AF4&DEV_1042` (virtio-blk)

To avoid Windows seeing **multiple** INFs that match the **same** modern device,
the INFs under `drivers/windows7/virtio/` must only match the legacy/transitional
ID space (`DEV_100x`), even if a virtio device type can also enumerate with a
modern ID.

## Contents

- `common/` – shared virtio-pci **legacy** transport + split-virtqueue library.
- `blk/` – `aerovblk` StorPort miniport (binds only to `DEV_1001`).
- `net/` – `aerovnet` NDIS 6.20 miniport (binds only to `DEV_1000`).

If you are implementing or packaging Aero’s contract-v1 drivers, do **not** use
this directory; use the contract-v1 driver directories instead.
