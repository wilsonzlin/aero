# Virtio documentation (Windows 7 focus)

This directory contains virtio-related implementation references intended for **Windows 7 guest drivers** (KMDF/WDM).

## Index

- [`virtqueue-split-ring-win7.md`](./virtqueue-split-ring-win7.md) — Virtio 1.0 split-ring virtqueue implementation guide (descriptor management, ordering/barriers, EVENT_IDX, indirect descriptors).

Reference code in this repo:

- WDF-free split virtqueue engine (`VIRTQ_SPLIT` / `VirtqSplit*` API):
  - `drivers/windows/virtio/common/virtqueue_split.{c,h}`
  - Used by:
    - `drivers/windows/virtio-input/` (KMDF; Win7 target)
    - `drivers/windows7/virtio/blk/` (StorPort miniport; Win7 target)
    - `drivers/windows7/virtio/net/` (NDIS miniport; Win7 target)
    - `drivers/windows7/virtio-snd/` (WDM; Win7 target)
    - Host tests: `drivers/windows/virtio/common/tests/` (`CMakeLists.txt`, `Makefile`)

  In-tree include sites for `virtqueue_split.h`:
  - `drivers/windows7/virtio/blk/include/aerovblk.h`
  - `drivers/windows7/virtio/net/include/aerovnet.h`
  - `drivers/windows7/virtio-snd/include/virtiosnd_queue_split.h`
  - `drivers/windows/virtio-input/src/device.c`
  - `drivers/windows/virtio-input/src/virtio_statusq.c`
  - `drivers/windows/virtio/common/virtio_sg_pfn.h`
  - Unit tests: `drivers/windows/virtio/common/tests/virtqueue_split_{test,stress_test}.c` (include via `../virtqueue_split.h`)

- Win7 portable split-ring engine (`virtqueue_split_*` + `virtio_os_ops_t` API):
  - `drivers/windows7/virtio/common/src/virtqueue_split_legacy.c`
  - `drivers/windows7/virtio/common/include/virtqueue_split_legacy.h`
  - Used by:
    - Host tests: `drivers/windows7/virtio/common/tests/` (`CMakeLists.txt`)

  In-tree include sites for `virtqueue_split_legacy.h`:
  - Unit tests: `drivers/windows7/virtio/common/tests/{test_main.c,fake_pci_device.h,fake_pci_device_modern.h}`

Header policy: `drivers/windows/virtio/common/virtqueue_split.h` is the **only**
header named `virtqueue_split.h` in-tree. The Win7 portable header is named
`virtqueue_split_legacy.h` to avoid include-path ambiguity.

## Build wiring (where each engine is compiled)

These are the build entry points that pull in each implementation (kept here so
it’s obvious which driver binaries are expected to link which engine):

- Canonical engine (`drivers/windows/virtio/common/virtqueue_split.c`)
  - `drivers/windows7/virtio-snd/virtio-snd.vcxproj`
  - `drivers/windows7/virtio-snd/src/sources` (WDK 7.1 `build.exe`)
  - `drivers/windows/virtio-input/virtio-input.vcxproj`
  - `drivers/windows/virtio-input/sources` (WDK 7.1 `build.exe`)
  - Host tests:
    - `drivers/windows/virtio/common/tests/CMakeLists.txt`
    - `drivers/windows/virtio/common/tests/Makefile`

- Legacy portable engine (`drivers/windows7/virtio/common/src/virtqueue_split_legacy.c`)
  - `drivers/windows7/virtio/blk/aerovblk.vcxproj`
  - `drivers/windows7/virtio/blk/sources` (WDK 7.1 `build.exe`)
  - `drivers/windows7/virtio/net/aerovnet.vcxproj`
  - `drivers/windows7/virtio/net/sources` (WDK 7.1 `build.exe`)
  - Host tests: `drivers/windows7/virtio/common/tests/CMakeLists.txt`

CI enforces this wiring via:

- `scripts/ci/check-win7-virtqueue-split-headers.py` (header-name ambiguity)
- `scripts/ci/check-virtqueue-split-driver-builds.py` (per-driver build files)
- `scripts/ci/check-win7-virtio-header-collisions.py` (prevent include-order header collisions across shared include roots)

Related (outside this directory):

- [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`../virtio-windows-drivers.md`](../virtio-windows-drivers.md) — packaging/install notes for Windows 7 virtio drivers (virtio-win, driver ISO).
- [`../windows-drivers/virtio/virtqueue-dma-strategy.md`](../windows-drivers/virtio/virtqueue-dma-strategy.md) — Windows 7 KMDF virtqueue DMA/common-buffer strategy (rings + indirect tables).
- [`../windows/virtio-pci-modern-interrupts.md`](../windows/virtio-pci-modern-interrupts.md) — virtio-pci modern interrupts on Win7 (MSI-X vs INTx).
- [`../16-virtio-drivers-win7.md`](../16-virtio-drivers-win7.md) — shared Win7 virtio driver plumbing overview.
