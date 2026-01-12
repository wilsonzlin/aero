# Virtio PCI Modern Transport (AERO-W7-VIRTIO v1)

This directory contains a **WDF-free**, reusable C transport module implementing the **virtio-pci modern** (Virtio 1.0+) transport for Aero Windows 7 guest drivers.

It is designed to be linked into **WDM** (non-KMDF) drivers such as:

- `virtio-snd` (WDM/PortCls)

Note: Aero's Windows 7 **miniport** drivers (`virtio-blk`, `virtio-net`) use the
miniport-friendly shim under `drivers/windows7/virtio/common/` instead of this
module.

## What this module provides

`virtio_pci_modern_transport.{c,h}` implements:

- PCI config discovery and parsing of virtio vendor-specific capabilities (COMMON/NOTIFY/ISR/DEVICE)
- BAR0 MMIO mapping
- Pointers to the mapped regions:
  - `CommonCfg` (`struct virtio_pci_common_cfg`)
  - `NotifyBase`
  - `IsrStatus`
  - `DeviceCfg`
- Virtio 1.0 status helpers (reset/status byte)
- 64-bit feature negotiation (requires `VIRTIO_F_VERSION_1`)
- Split-virtqueue programming helpers (desc/avail/used addresses, `queue_enable`)
- Queue notification helper (doorbell write)
- MSI-X vector programming helpers (`msix_config`, `queue_msix_vector`) with readback validation
- Device-specific config read/write with `config_generation` retry logic

## AERO-W7-VIRTIO v1 contract enforcement (STRICT mode)

`VirtioPciModernTransportInit(..., STRICT, ...)` rejects devices that do not match the Aero Windows 7 virtio contract:

- PCI Vendor ID **MUST** be `0x1AF4`
- PCI Device ID **MUST** be in the modern-only virtio-pci ID space (`>= 0x1040`)
- PCI Revision ID **MUST** be `0x01`
- PCI Subsystem Vendor ID **MUST** be `0x1AF4`
- PCI Interrupt Pin **MUST** be `1` (INTA#)
- BAR0 **MUST** be a 64-bit memory BAR (MMIO), not I/O space
- BAR0 base address in PCI config **MUST** match the BAR0 physical address passed by the driver
- PCI capability list **MUST** be present (Status bit 4 set)
- Capability list pointers **MUST** be 4-byte aligned and acyclic
- Required virtio vendor caps **MUST** exist and reference BAR0:
  - COMMON @ `0x0000` (len >= `0x0100`)
  - NOTIFY @ `0x1000` (len >= `0x0100`)
  - ISR    @ `0x2000` (len >= `0x0020`)
  - DEVICE @ `0x3000` (len >= `0x0100`)
- `notify_off_multiplier` **MUST** be `4`

Feature negotiation additionally enforces:

- `VIRTIO_F_VERSION_1` is required
- STRICT mode requires the device to offer `VIRTIO_F_RING_INDIRECT_DESC` (bit 28)
- `VIRTIO_F_RING_EVENT_IDX` is **never** negotiated
- `VIRTIO_F_RING_PACKED` is **never** negotiated

`COMPAT` mode keeps the safety checks but relaxes the fixed-offset requirement to ease QEMU/transitional experimentation.

## Caller responsibilities

This module is **transport-only**. Drivers integrating it must provide:

- Implementations of the `VIRTIO_PCI_MODERN_OS_INTERFACE` callbacks:
  - PCI config reads (8/16/32)
  - MMIO map/unmap for BAR0
  - microsecond stall for reset polling
  - spinlock primitives for CommonCfg selector serialization
- BAR0 physical address + length (from the driver’s resource discovery path)
- Per-driver device ID filtering (e.g. `0x1059` for virtio-snd) if required by the driver package
- DMA allocations for virtqueues and request buffers (device sees guest physical addresses)
- Interrupt wiring (INTx + ISR polling/ack)

If a driver uses MSI-X routing, vector selectors may be disabled by programming
`VIRTIO_PCI_MSI_NO_VECTOR` into `msix_config` / `queue_msix_vector`.

## Integration sketch

Typical init flow inside a driver:

1. Discover BAR0 physical base/length via the driver model’s resource list.
2. Initialize the transport:
   - `VirtioPciModernTransportInit(&t, &os, STRICT, bar0_pa, bar0_len)`
3. Negotiate features:
   - `VirtioPciModernTransportNegotiateFeatures(&t, Required, Wanted, &Negotiated)`
4. Allocate and program queues:
   - `VirtioPciModernTransportSetupQueue(&t, q, desc_pa, avail_pa, used_pa)`
5. Set `DRIVER_OK` once the device is ready.

Interrupt handling (INTx):

- On interrupt, call `VirtioPciModernTransportReadIsrStatus(&t)` to read-to-ack and determine cause.
