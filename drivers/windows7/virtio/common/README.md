# Aero Windows 7 virtio common library

Permissive license: **MIT OR Apache-2.0** (see `LICENSE-MIT` and `LICENSE-APACHE`).

This directory contains a small, reusable C library intended to be shared by all
Windows 7 SP1 x86/x64 Aero guest drivers that speak virtio.

It currently contains two transport implementations:

- **Legacy/transitional:** `virtio_pci_legacy.*` (I/O port register set)
- **Modern-only (WDM):** `virtio_pci_modern_wdm.*` (virtio 1.0+ PCI vendor caps + MMIO)

## What is implemented

- **Transport:** `virtio_pci_legacy.*`
  - virtio-pci legacy I/O register block (as exposed by QEMU transitional
    devices).
  - device reset + status bits
  - feature read/write (legacy: low 32 bits)
  - queue select/size/PFN programming
  - queue notify
  - ISR read (acknowledges interrupt)

- **Virtqueue:** `virtqueue_split.*`
  - split ring layout (`vring_desc`, `vring_avail`, `vring_used`)
  - descriptor allocation/free with in-flight cookie tracking
  - optional **indirect descriptors** (`VIRTIO_RING_F_INDIRECT_DESC`)
  - optional **event index** notification suppression (`VIRTIO_RING_F_EVENT_IDX`)
  - explicit memory barriers via the OS shim (SMP/DMA ordering)

- **OS abstraction:** `include/virtio_os.h`
  - core code has **no StorPort/NDIS/KMDF header dependencies**
  - drivers provide a `virtio_os_ops_t` implementation
  - reference kernel-mode shims live in `os_shim/`

## virtio-pci legacy register offsets

The legacy register set is a byte-addressed I/O port (or MMIO) block. Offsets
below are from the base of the virtio BAR:

| Offset | Size | Name | Notes |
|-------:|-----:|------|-------|
| 0x00 | u32 | `HOST_FEATURES` | device feature bits (low 32) |
| 0x04 | u32 | `GUEST_FEATURES` | driver accepted feature bits (low 32) |
| 0x08 | u32 | `QUEUE_PFN` | `queue_paddr >> 12` (page frame number) |
| 0x0c | u16 | `QUEUE_NUM` | queue size for selected queue |
| 0x0e | u16 | `QUEUE_SEL` | select queue index |
| 0x10 | u16 | `QUEUE_NOTIFY` | notify queue index |
| 0x12 | u8 | `STATUS` | device status bits |
| 0x13 | u8 | `ISR` | interrupt status (read-to-ack) |
| 0x14 | u16 | `CONFIG_VECTOR` | MSI-X only (otherwise device config starts here) |
| 0x16 | u16 | `QUEUE_VECTOR` | MSI-X only |
| 0x14 / 0x18 | ... | `DEVICE_CONFIG` | device-specific config space (no MSI-X / MSI-X) |

This matches the layout used by QEMU virtio-pci transitional devices and the
Linux `virtio_pci_legacy` driver.

## Queue alignment + PFN programming

For legacy split virtqueues the ring layout uses a fixed alignment:

- The queue ring memory **must be physically contiguous** and **aligned** to
  **4096 bytes** (`VIRTIO_PCI_VRING_ALIGN`).
- The driver programs the queue base by writing:

```
QUEUE_PFN = (queue_physical_address >> 12)
```

The PFN register is 32-bit, so the maximum representable queue physical address
is `0xFFFFFFFF << 12` (16 TiB - 4 KiB).

## Emulator/device-model contract (Aero + QEMU compatibility)

To interoperate with this library (and typical virtio drivers), the device-model
must:

1. Expose a PCI function with a virtio-pci transitional compatible BAR that
   implements the register block above.
2. Implement the **split ring** vring layout:
   - descriptor table + avail ring + used ring in a single physically contiguous
     region starting at `QUEUE_PFN << 12`
   - used ring aligned to **4096 bytes** (`VIRTIO_PCI_VRING_ALIGN`)
3. Implement queue notifications via `QUEUE_NOTIFY`.
4. Raise an interrupt and set the ISR bit(s) when:
   - used ring is updated (bit 0: queue interrupt)
   - device config changes (bit 1: config interrupt, optional)
   Reading `ISR` must acknowledge/clear the pending interrupt bits.
5. If the negotiated feature set includes:
   - `VIRTIO_RING_F_INDIRECT_DESC`: device must support `VRING_DESC_F_INDIRECT`.
   - `VIRTIO_RING_F_EVENT_IDX`: device must honor `used_event`/`avail_event`
     (notification suppression). If not implemented, do not advertise it.

## Host-side unit tests

`drivers/windows7/virtio/common/tests/` contains a small user-mode test program
that builds the core ring logic with a fake I/O backend and validates:

- descriptor chain allocation/free under u16 index wraparound
- avail/used index handling
- indirect descriptor table building
- randomized fuzz sequences (invariants/no corruption)

Build with CMake:

```sh
cmake -S . -B build
cmake --build build
ctest --test-dir build --output-on-failure
```

## Modern virtio-pci (WDM-only) transport

`include/virtio_pci_modern_wdm.h` + `src/virtio_pci_modern_wdm.c` provide a
**WDM-only** virtio-pci **modern** transport implementation tailored for the
[`AERO-W7-VIRTIO`](../../../../docs/windows7-virtio-driver-contract.md) contract:

- Discover PCI vendor capabilities (COMMON/NOTIFY/ISR/DEVICE) using the portable
  capability parser from `drivers/win7/virtio/virtio-core/portable/`.
- Map BAR0 MMIO with `MmMapIoSpace(MmNonCached)`.
- Serialize `common_cfg` selector registers (`*_feature_select`, `queue_select`)
  with a per-device `KSPIN_LOCK`.
- Negotiate `VIRTIO_F_VERSION_1` and caller-supplied feature bits.
- Program split virtqueues via `common_cfg` and notify via the notify region.

IRQL notes (see SAL annotations in the header):

- `VirtioPciModernWdmInit` / `VirtioPciModernWdmMapBars` / `VirtioPciModernWdmUnmapBars` / `VirtioPciModernWdmUninit`:
  **PASSIVE_LEVEL**
- Queue/config/notify helpers: **<= DISPATCH_LEVEL**
