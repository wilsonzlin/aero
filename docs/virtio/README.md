# Virtio documentation (Windows 7 focus)

This directory contains virtio-related implementation references intended for **Windows 7 guest drivers** (KMDF/WDM).

## Index

- [`virtqueue-split-ring-win7.md`](./virtqueue-split-ring-win7.md) — Virtio 1.0 split-ring virtqueue implementation guide (descriptor management, ordering/barriers, EVENT_IDX, indirect descriptors).

Related (outside this directory):

- [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`../virtio-windows-drivers.md`](../virtio-windows-drivers.md) — packaging/install notes for Windows 7 virtio drivers (virtio-win, driver ISO).

