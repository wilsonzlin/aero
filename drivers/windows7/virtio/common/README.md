# Aero Windows 7 virtio common helpers

Permissive license: **MIT OR Apache-2.0** (see `LICENSE-MIT` and `LICENSE-APACHE`).

This directory contains small reusable helpers shared by Aero's Windows 7 SP1
x86/x64 virtio drivers.

## Virtio-pci modern transport (AERO-W7-VIRTIO v1)

All in-tree **non-KMDF** Windows 7 virtio drivers should use the canonical,
WDF-free virtio-pci modern transport in:

- `drivers/windows/virtio/pci-modern/` (`VirtioPciModernTransport*`)

The canonical transport discovers devices via PCI vendor capabilities, maps BAR0
MMIO via a small OS callback interface (`VIRTIO_PCI_MODERN_OS_INTERFACE`), and
enforces the **AERO-W7-VIRTIO v1** contract in **STRICT** mode (REV_01, BAR0
layout/length, `notify_off_multiplier`, required features, etc).

### Optional WDM wrapper (`virtio_pci_modern_wdm`)

For WDM stack drivers, this directory provides a thin wrapper that wires up:

- `GUID_PCI_BUS_INTERFACE_STANDARD` config reads
- BAR discovery via `CM_RESOURCE_LIST`
- `MmMapIoSpace` mapping

This wrapper then delegates to the canonical transport:

- `include/virtio_pci_modern_wdm.h` + `src/virtio_pci_modern_wdm.c`

This wrapper intentionally contains **no independent modern transport
implementation**; it only exposes a small `VirtioPci*` convenience API over the
canonical `VirtioPciModernTransport*` implementation.

Other shared helpers:

- `include/virtio_pci_intx_wdm.h` + `src/virtio_pci_intx_wdm.c` — WDM INTx ISR read-to-ack + DPC dispatch
- `include/virtio_pci_contract.h` + `src/virtio_pci_contract.c` — AERO-W7-VIRTIO v1 PCI identity validation helpers

## Aero contract v1 (AERO-W7-VIRTIO)

The binding device/driver contract lives at:

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md)

Contract v1 requires the virtio-pci **modern** transport:

- Contract major version is encoded in **PCI Revision ID**:
  - **Revision ID = `0x01`** for contract v1 devices.
- Device discovery is via PCI vendor-specific capabilities:
  `COMMON_CFG`, `NOTIFY_CFG`, `ISR_CFG`, `DEVICE_CFG`.
- Drivers map **BAR0 MMIO** and access the above regions as little-endian MMIO.
- BAR0 must be large enough for the fixed contract v1 layout (**>= 0x4000 bytes**).
- Feature negotiation is **64-bit** and drivers MUST accept `VIRTIO_F_VERSION_1`
  (feature bit 32). `VirtioPciModernTransportNegotiateFeatures` always enforces it.
- Interrupts must work with PCI **INTx** and the virtio ISR status byte (read-to-ack).

Contract v1 fixed BAR0 layout:

| Capability | BAR | Offset | Length |
|-----------|----:|-------:|-------:|
| Common configuration (`COMMON_CFG`) | 0 | `0x0000` | `0x0100` |
| Notify (`NOTIFY_CFG`)              | 0 | `0x1000` | `0x0100` |
| ISR (`ISR_CFG`)                   | 0 | `0x2000` | `0x0020` |
| Device config (`DEVICE_CFG`)      | 0 | `0x3000` | `0x0100` |

Notify semantics:

- `notify_off_multiplier = 4`
- Doorbell address:
  `notify_base + queue_notify_off(queue) * notify_off_multiplier`

Queue programming uses `common_cfg.queue_desc/queue_avail/queue_used` + `queue_enable`.
Because `common_cfg` contains selector registers (`*_feature_select`, `queue_select`),
multi-step sequences must be serialized (for example via a per-device spin lock).

The legacy/transitional I/O-port transport is **not** required by contract v1 and
is retained only for compatibility/testing with transitional/QEMU devices.

## Docs

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md)
- [`docs/windows/virtio-pci-modern-wdm.md`](../../../../docs/windows/virtio-pci-modern-wdm.md)
- [`docs/windows/win7-miniport-virtio-pci-modern.md`](../../../../docs/windows/win7-miniport-virtio-pci-modern.md)

## Contents of this directory

- `include/virtio_pci_intx_wdm.h` + `src/virtio_pci_intx_wdm.c`
  - Shared WDM INTx ISR + DPC helper for virtio-pci devices (ISR read-to-ack, then DPC dispatch).
- `include/virtio_pci_contract.h` + `src/virtio_pci_contract.c`
  - Optional PCI identity validation helpers (AERO-W7-VIRTIO contract checks).
- `include/virtio_pci_modern_wdm.h` + `src/virtio_pci_modern_wdm.c`
  - WDM wrapper over the canonical virtio-pci modern transport.
- `include/virtio_pci_legacy.h` + `src/virtio_pci_legacy.c`
  - Legacy/transitional virtio-pci transport (virtio 0.9 I/O-port register set).
- `include/virtqueue_split_legacy.h` + `src/virtqueue_split_legacy.c`
  - Portable split ring (`vring`) implementation (descriptor table + avail ring + used ring).
  - Used by host-side unit tests and legacy/transitional experiments.
  - Shipped Aero Windows 7 drivers use the canonical WDF-free split virtqueue engine in
    `drivers/windows/virtio/common/virtqueue_split.{c,h}`; the `_legacy` suffix exists solely to
    avoid a repository-wide filename clash with `virtqueue_split.h`.
- `include/virtio_sg.h`
  - Shared scatter/gather entry type (`virtio_sg_entry_t`) used by virtqueues and driver helpers.
- `include/virtio_queue.h` + `src/virtio_queue.c`
  - Convenience wrapper around split rings for the legacy PFN programming model (`QUEUE_PFN = (ring_pa >> 12)`).
- `include/virtio_os.h` + `os_shim/`
  - OS abstraction used by some portable code and host-side unit tests.

## Using the modern transport

At a high level, drivers using Aero contract v1 devices should:

1. Implement the `VIRTIO_PCI_MODERN_OS_INTERFACE` callbacks for PCI config reads,
   BAR0 mapping, stall/barrier, and selector serialization.
2. Call `VirtioPciModernTransportInit(...)` (**STRICT** by default; some drivers
   optionally retry with **COMPAT** for bring-up).
3. Negotiate features via `VirtioPciModernTransportNegotiateFeatures(...)`.
4. Program queues via:
   - `VirtioPciModernTransportGetQueueSize(...)`
   - `VirtioPciModernTransportGetQueueNotifyOff(...)`
   - `VirtioPciModernTransportSetupQueue(...)`
   - `VirtioPciModernTransportNotifyQueue(...)`
5. Handle INTx by reading the ISR status byte (read-to-ack) and draining queues
   at DPC level. WDM drivers may reuse `virtio_pci_intx_wdm.*`.

Note: `virtio_queue.*` is built on the legacy/transitional virtio-pci PFN queue
programming model and is not compatible with the modern transport.

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

## Legacy queue PFN programming (legacy transport only)

The legacy I/O-port transport programs the split virtqueue base address by
writing a 32-bit PFN:

```
QUEUE_PFN = (queue_physical_address >> 12)
```

For Aero contract v1 devices this is obsolete; modern devices use the 64-bit
`queue_desc/queue_avail/queue_used` registers in `common_cfg` and `queue_enable`
instead (handled by `VirtioPciModernTransport*`).

## Emulator/device-model contract (Aero + QEMU compatibility)

For Aero contract v1 devices, follow the definitive contract document:

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md)

For the optional legacy/transitional transport, the device-model must:

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

Host-buildable tests live in a few places:

- `drivers/windows7/virtio/common/tests/` covers split ring helpers + legacy
  transport behaviour using fake I/O backends.
- `drivers/windows/virtio/pci-modern/tests/` covers the canonical modern
  transport (PCI cap parsing + MMIO contract validation).
- `drivers/win7/virtio/tests/` covers the portable PCI capability parser.
