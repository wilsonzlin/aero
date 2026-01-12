# Aero Windows 7 virtio common helpers

Permissive license: **MIT OR Apache-2.0** (see `LICENSE-MIT` and `LICENSE-APACHE`).

This directory contains small reusable helpers shared by Aero's Windows 7 SP1
x86/x64 virtio drivers.

## Virtio-pci modern transport (AERO-W7-VIRTIO v1)

Aero’s contract-v1 devices use the virtio-pci **modern** transport (virtio 1.0+)
with a fixed BAR0 MMIO layout and INTx interrupts.

Depending on driver model, this repo provides two WDF-free ways to talk to these
devices:

- **Miniport drivers (NDIS / StorPort):** `include/virtio_pci_modern_miniport.h` +
  `src/virtio_pci_modern_miniport.c` (`VirtioPci*` API). Callers provide:
  - a BAR0 MMIO mapping, and
  - a snapshot of PCI config space (typically 256 bytes).
- **Generic transport:** `drivers/windows/virtio/pci-modern/` (`VirtioPciModernTransport*` API),
  which uses a small OS callback interface (`VIRTIO_PCI_MODERN_OS_INTERFACE`) to read PCI config
  and map BAR0.

See:

- [`docs/windows/virtio-pci-modern-wdm.md`](../../../../docs/windows/virtio-pci-modern-wdm.md) (WDM)
- [`docs/windows/win7-miniport-virtio-pci-modern.md`](../../../../docs/windows/win7-miniport-virtio-pci-modern.md) (NDIS/StorPort)

This directory also provides shared helpers often used alongside either
transport:

- `virtio_pci_intx_wdm.*` — WDM INTx ISR read-to-ack + DPC dispatch
- `virtio_pci_contract.*` — AERO-W7-VIRTIO v1 PCI identity validation helpers

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
  (feature bit 32). `VirtioPciNegotiateFeatures` and
  `VirtioPciModernTransportNegotiateFeatures` always enforce it.
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

- `include/virtio_pci_modern_miniport.h` + `src/virtio_pci_modern_miniport.c`
  - Modern virtio-pci transport helper for miniport-style drivers (NDIS / StorPort).
  - Parses PCI vendor capabilities from a caller-provided PCI config snapshot.
  - Exposes:
    - device status/reset
    - 64-bit feature negotiation (always requires `VIRTIO_F_VERSION_1`)
    - device config reads with `config_generation` retry logic (`VirtioPciReadDeviceConfig`)
    - queue programming via `queue_desc/queue_avail/queue_used` + `queue_enable`
    - notify doorbells + ISR read-to-ack
- `include/virtio_pci_intx_wdm.h` + `src/virtio_pci_intx_wdm.c`
  - Shared WDM INTx ISR + DPC helper for virtio-pci devices (ISR read-to-ack, then DPC dispatch).
- `include/virtio_pci_contract.h` + `src/virtio_pci_contract.c`
  - Optional PCI identity validation helpers (AERO-W7-VIRTIO contract checks).
- `include/virtio_pci_legacy.h` + `src/virtio_pci_legacy.c`
  - Legacy/transitional virtio-pci transport (virtio 0.9 I/O-port register set).
- `include/virtqueue_split_legacy.h` + `src/virtqueue_split_legacy.c`
  - Portable split ring (`vring`) implementation (descriptor table + avail ring + used ring).
  - Used by Aero's Windows 7 virtio miniport drivers (`aero_virtio_net` / `aero_virtio_blk`) and by host-side unit tests.
  - The `_legacy` suffix exists solely to avoid a repository-wide filename clash with
    `drivers/windows/virtio/common/virtqueue_split.h`.
- `include/virtio_sg.h`
  - Shared scatter/gather entry type (`virtio_sg_entry_t`) used by virtqueues and driver helpers.
- `include/virtio_queue.h` + `src/virtio_queue.c`
  - Convenience wrapper around split rings for the legacy PFN programming model (`QUEUE_PFN = (ring_pa >> 12)`).
- `include/virtio_os.h` + `os_shim/`
  - OS abstraction used by some portable code and host-side unit tests.

## Using the modern transport

At a high level, drivers using Aero contract v1 devices should:

1. Read a PCI config snapshot and map BAR0 MMIO (miniport/WDM specific).
2. Initialize a modern transport:
   - **Miniports (NDIS/StorPort):** `VirtioPciModernMiniportInit(...)`
   - **Generic transport:** `VirtioPciModernTransportInit(...)` (**STRICT** by default; some drivers optionally retry with **COMPAT** for bring-up).
3. Negotiate features (64-bit; always requires `VIRTIO_F_VERSION_1`).
4. Allocate and program split virtqueues:
   - build a ring layout using a split-ring engine:
     - Miniports in this repo use `virtqueue_split_legacy.*` (for example `virtqueue_split_alloc_ring` + `virtqueue_split_init`).
     - WDM drivers like `virtio-snd` use the canonical `drivers/windows/virtio/common/virtqueue_split.{c,h}` engine (`VIRTQ_SPLIT` / `VirtqSplit*` API).
   - activate it via `VirtioPciSetupQueue(...)` (miniport) or `VirtioPciModernTransportSetupQueue(...)` (generic)
   - optionally pre-cache notify doorbell addresses so `VirtioPciNotifyQueue(...)` stays lock-free on hot paths.
5. Handle INTx:
   - read-to-ack the ISR byte (`VirtioPciReadIsr(...)` / `VirtioPciModernTransportReadIsrStatus(...)`), then drain queues in a DPC.
   - WDM drivers may reuse the INTx helper in this directory: `virtio_pci_intx_wdm.*`.

Note: `virtio_queue.*` is built on the legacy/transitional virtio-pci PFN queue
programming model and is not compatible with the modern transport. Modern drivers
pair the transport with a split-ring implementation:

- Miniports (`virtio-blk` / `virtio-net`) use `virtqueue_split_legacy.*`.
- WDM drivers like `virtio-snd` use the canonical `drivers/windows/virtio/common/virtqueue_split.{c,h}` engine.

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
instead (handled by the modern transport helpers).

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
  transport behaviour using fake I/O backends, plus host-buildable shims/tests
  for selected WDK-dependent helpers (e.g. `virtio_pci_intx_wdm` and the Win7
  modern miniport transport). These tests compile against a local
  `wdk_stubs/ntddk.h` stub header (no WDK required).
- `drivers/windows/virtio/pci-modern/tests/` covers the canonical modern
  transport (PCI cap parsing + MMIO contract validation).
- `drivers/win7/virtio/tests/` covers the portable PCI capability parser.
