# Virtio documentation (Windows 7 focus)

This directory contains virtio-related implementation references intended for **Windows 7 guest drivers** (KMDF/WDM).

## Index

- [`virtqueue-split-ring-win7.md`](./virtqueue-split-ring-win7.md) — Virtio 1.0 split-ring virtqueue implementation guide (descriptor management, ordering/barriers, EVENT_IDX, indirect descriptors).

Reference code in this repo:

- `drivers/windows/virtio/common/` — Windows-friendly virtio split-ring virtqueue implementation (`virtqueue_split.*`) + `make test` user-mode harness.

Related (outside this directory):

- [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`../virtio-windows-drivers.md`](../virtio-windows-drivers.md) — packaging/install notes for Windows 7 virtio drivers (virtio-win, driver ISO).
- [`../windows-drivers/virtio/virtqueue-dma-strategy.md`](../windows-drivers/virtio/virtqueue-dma-strategy.md) — Windows 7 KMDF virtqueue DMA/common-buffer strategy (rings + indirect tables).
- [`../windows/virtio-pci-modern-interrupts.md`](../windows/virtio-pci-modern-interrupts.md) — virtio-pci modern interrupts on Win7 (MSI-X vs INTx).
- [`../16-virtio-drivers-win7.md`](../16-virtio-drivers-win7.md) — shared Win7 virtio driver plumbing overview.
