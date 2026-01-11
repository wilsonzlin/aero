# Virtio documentation (Windows 7 focus)

This directory contains virtio-related implementation references intended for **Windows 7 guest drivers** (KMDF/WDM).

## Index

- [`virtqueue-split-ring-win7.md`](./virtqueue-split-ring-win7.md) — Virtio 1.0 split-ring virtqueue implementation guide (descriptor management, ordering/barriers, EVENT_IDX, indirect descriptors).

Reference code in this repo:

- WDF-free split virtqueue engine (`VIRTQ_SPLIT` / `VirtqSplit*` API):
  - `drivers/windows/virtio/common/virtqueue_split.{c,h}`
  - Used by:
    - `drivers/windows/virtio-input/` (KMDF; Win7 target)
    - `drivers/windows7/virtio-snd/` (WDM; Win7 target)
    - Host tests: `drivers/windows/virtio/common/tests/` (`CMakeLists.txt`, `Makefile`)

  In-tree `#include "virtqueue_split.h"` sites:
  - `drivers/windows7/virtio-snd/include/virtiosnd_queue_split.h`
  - `drivers/windows/virtio-input/src/device.c`
  - `drivers/windows/virtio-input/src/virtio_statusq.c`
  - `drivers/windows/virtio/common/virtio_sg_pfn.h`

- Win7 portable split-ring engine (`virtqueue_split_*` + `virtio_os_ops_t` API):
  - `drivers/windows7/virtio/common/src/virtqueue_split_legacy.c`
  - `drivers/windows7/virtio/common/include/virtqueue_split_legacy.h`
  - Used by:
    - `drivers/windows7/virtio/blk/` (StorPort miniport)
    - `drivers/windows7/virtio/net/` (NDIS miniport)
    - Host tests: `drivers/windows7/virtio/common/tests/` (`CMakeLists.txt`)

  In-tree `#include "virtqueue_split_legacy.h"` sites:
  - `drivers/windows7/virtio/blk/include/aerovblk.h`
  - `drivers/windows7/virtio/net/include/aerovnet.h`
  - `drivers/windows7/virtio-snd/include/virtiosnd_sg_core.h` (SG entry shape)

Header policy: `drivers/windows/virtio/common/virtqueue_split.h` is the **only**
header named `virtqueue_split.h` in-tree. The Win7 portable header is named
`virtqueue_split_legacy.h` to avoid include-path ambiguity.

Related (outside this directory):

- [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`../virtio-windows-drivers.md`](../virtio-windows-drivers.md) — packaging/install notes for Windows 7 virtio drivers (virtio-win, driver ISO).
- [`../windows-drivers/virtio/virtqueue-dma-strategy.md`](../windows-drivers/virtio/virtqueue-dma-strategy.md) — Windows 7 KMDF virtqueue DMA/common-buffer strategy (rings + indirect tables).
- [`../windows/virtio-pci-modern-interrupts.md`](../windows/virtio-pci-modern-interrupts.md) — virtio-pci modern interrupts on Win7 (MSI-X vs INTx).
- [`../16-virtio-drivers-win7.md`](../16-virtio-drivers-win7.md) — shared Win7 virtio driver plumbing overview.
