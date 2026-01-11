# Aero Windows 7 virtio common library

Permissive license: **MIT OR Apache-2.0** (see `LICENSE-MIT` and `LICENSE-APACHE`).

This directory contains a small, reusable C library intended to be shared by all
Windows 7 SP1 x86/x64 Aero guest drivers that speak virtio.

At a high level, Aero drivers use two virtio-pci transport styles:

- **Modern (Virtio 1.0+ / contract v1):** BAR MMIO + `COMMON/NOTIFY/ISR/DEVICE` capabilities
- **Legacy/transitional (virtio 0.9):** I/O-port register set (PFN-based queue programming)

This directory contains implementations and helpers for both. For modern virtio-pci on Windows there are two main consumer models:

- **Miniport-style (NDIS / StorPort):** `virtio_pci_modern_miniport.*`
  - caller provides a BAR0 MMIO mapping and a PCI config snapshot
  - parses virtio vendor-specific capabilities (COMMON/NOTIFY/ISR/DEVICE) and provides helpers
    for `common_cfg` / notify / ISR / device config
- **WDM stack drivers:** `virtio_pci_modern_wdm.*`
  - queries the PCI bus interface and maps BARs from `CM_RESOURCE_LIST`
  - also parses virtio capabilities and exposes the same high-level transport operations

Drivers still own device-specific protocol/state machines and virtqueue sizing/allocation policy.

> Important: the miniport and WDM modern transports both implement a `VirtioPci*` API surface
> (`VirtioPciResetDevice`, `VirtioPciNegotiateFeatures`, `VirtioPciSetupQueue`, etc) and therefore
> **must not** be linked into the same driver binary (duplicate symbols). Pick exactly one.

And additional shared helpers:

- **INTx helper (WDM):** `virtio_pci_intx_wdm.*` (ISR read-to-ack + DPC dispatch)
- **Contract identity validation:** `virtio_pci_contract.*` (AERO-W7-VIRTIO v1 PCI identity)
- **Split virtqueues:** `virtqueue_split.*` (portable split ring implementation)
- **Queue helper (miniport):** `virtio_queue.*` (alloc + `common_cfg` queue programming + notify)

For a WDM-focused modern transport bring-up guide (caps + BAR mapping + queues + INTx), see:
[`docs/windows/virtio-pci-modern-wdm.md`](../../../../docs/windows/virtio-pci-modern-wdm.md).

For miniports (NDIS / StorPort) bring-up, see:
[`docs/windows/win7-miniport-virtio-pci-modern.md`](../../../../docs/windows/win7-miniport-virtio-pci-modern.md).

## Aero contract v1 (AERO-W7-VIRTIO)

The binding device/driver contract lives at:

- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md)

**Contract v1 requires the virtio-pci _modern_ transport.** In particular:

- Contract major version is encoded in **PCI Revision ID**:
  - **Revision ID = `0x01`** for contract v1 devices.
- Device discovery is via PCI vendor-specific capabilities:
  `COMMON_CFG`, `NOTIFY_CFG`, `ISR_CFG`, `DEVICE_CFG`.
- Drivers map **BAR0 MMIO** and access the above regions as little-endian MMIO.
- All required virtio vendor capabilities must reference **BAR0** and BAR0 must be
  large enough to contain the fixed layout below (**>= 0x4000 bytes** per contract v1).
- Feature negotiation is **64-bit** and drivers **MUST** accept `VIRTIO_F_VERSION_1`
  (feature bit 32).
- Queues are programmed via the common configuration registers:
  `queue_desc` / `queue_avail` / `queue_used` and activated with `queue_enable`.
- Interrupts must work with PCI **INTx** and the virtio ISR status byte
  (read-to-ack).

Contract v1 also fixes a single BAR0 layout so drivers can validate conformance
without guessing:

| Capability | BAR | Offset | Length |
|-----------|----:|-------:|-------:|
| Common configuration (`COMMON_CFG`) | 0 | `0x0000` | `0x0100` |
| Notify (`NOTIFY_CFG`)              | 0 | `0x1000` | `0x0100` |
| ISR (`ISR_CFG`)                   | 0 | `0x2000` | `0x0020` |
| Device config (`DEVICE_CFG`)      | 0 | `0x3000` | `0x0100` |

Notify semantics:

- `notify_off_multiplier = 4`
- Drivers compute the doorbell address as:
  `notify_base + queue_notify_off(queue) * notify_off_multiplier`

Queue programming (modern):

- Drivers select a queue with `common_cfg.queue_select`, then:
  - read `queue_size` (max ring size, in descriptors)
  - read `queue_notify_off` (doorbell selector)
  - write `queue_desc`, `queue_avail`, `queue_used` (64-bit guest physical addrs)
  - write `queue_enable = 1` to activate
- Unlike legacy/transitional devices, there is no PFN register and the descriptor
  table, avail ring, and used ring do **not** need to live in a single 4 KiB-aligned
  physically-contiguous allocation (each structure is programmed separately).
  Contract v1 follows the standard virtio alignment rules:
  - `queue_desc`: 16-byte aligned
  - `queue_avail`: 2-byte aligned
  - `queue_used`: 4-byte aligned
- Each structure must still occupy a contiguous range in guest physical address
  space (it is a linear array/ring in memory).
- Windows 7 drivers should program the 64-bit queue address fields using two
  32-bit MMIO accesses (`*_lo` then `*_hi`) rather than a single 64-bit MMIO write.

Selector serialization:

- `common_cfg` contains device-global selector registers (`*_feature_select`,
  `queue_select`). Multi-step sequences that depend on selectors must be
  serialized (for example, via a per-device spin lock).

Feature negotiation (modern, 64-bit):

- Device features are read via the selector pattern:
  - write `device_feature_select = 0` then read `device_feature` (low 32 bits)
  - write `device_feature_select = 1` then read `device_feature` (high 32 bits)
- Driver features are written similarly via `driver_feature_select` /
  `driver_feature`.
- Aero contract v1 devices require `VIRTIO_F_VERSION_1` (bit 32) and the provided
  `VirtioPciNegotiateFeatures` helpers always enforce it.

Even though Aero contract v1 fixes the BAR0 offsets, drivers should still validate
that the device exposes the required virtio vendor-specific PCI capabilities
(cap ID `0x09`) and that they describe the expected BAR/offset/length. Miniports
typically do this by parsing a 256-byte PCI config snapshot with
`virtio_pci_cap_parser` (see `drivers/win7/virtio/virtio-core/portable/`).

The legacy/transitional I/O-port transport is **not** required by contract v1 and
is retained only for compatibility/testing with transitional/QEMU devices.

## File inventory

### Modern transport (Virtio 1.0+)

This directory provides:

- a portable, OS-agnostic modern transport (`virtio_pci_modern.*`), and
- two Windows-facing modern transports (`virtio_pci_modern_miniport.*` and `virtio_pci_modern_wdm.*`).

The two Windows-facing transports both export a `VirtioPci*` API surface, so a driver
must link **exactly one** of them.

- `include/virtio_pci_modern.h` + `src/virtio_pci_modern.c`
  - OS-agnostic virtio-pci modern transport built on `virtio_os_ops_t`.
  - Used by host-side unit tests (fake PCI config + BAR0 MMIO backends).
  - Discovers `COMMON/NOTIFY/ISR/DEVICE` capability regions and performs 64-bit
    feature negotiation (requires `VIRTIO_F_VERSION_1`).
  - Programs split virtqueues via `common_cfg` (`queue_desc/queue_avail/queue_used`) and sets `queue_enable`.

- `include/virtio_pci_modern_miniport.h` + `src/virtio_pci_modern_miniport.c`
  - Modern virtio-pci transport for miniport-style drivers (NDIS / StorPort).
  - Callers provide:
    - a BAR0 MMIO mapping, and
    - a PCI config snapshot (typically 256 bytes) used to parse virtio vendor capabilities.
  - Exposes helpers for:
    - device status/reset
    - 64-bit feature negotiation (always requires `VIRTIO_F_VERSION_1`)
    - queue programming via `queue_desc/queue_avail/queue_used` + `queue_enable`
    - notify doorbells + ISR read-to-ack
  - Designed to be used with `virtio_queue.*` and `virtqueue_split.*`.

- `include/virtio_pci_modern_wdm.h` + `src/virtio_pci_modern_wdm.c`
  - Parses PCI capability list and discovers the required virtio vendor caps
    (`COMMON/NOTIFY/ISR/DEVICE`) using the portable parser from
    `drivers/win7/virtio/virtio-core/portable/`.
  - Maps BAR MMIO using `MmMapIoSpace(MmNonCached)`.
  - Provides a per-device `KSPIN_LOCK` to serialize selector-based `common_cfg`
    sequences (`*_feature_select`, `queue_select`).
  - Queue programming uses `queue_desc/queue_avail/queue_used` + `queue_enable`
    (i.e., **no legacy PFN programming**).
  - Feature negotiation is 64-bit and `VirtioPciNegotiateFeatures()` always
    requires `VIRTIO_F_VERSION_1`.
  - IRQL:
    - init/map/unmap/uninit/negotiation helpers are **PASSIVE_LEVEL**
    - queue/config/notify helpers are **<= DISPATCH_LEVEL** (DPC-safe)
  - Not implemented (out of scope):
    - MSI/MSI-X interrupt setup (contract v1 requires INTx)
    - packed virtqueues

- `include/virtio_pci_intx_wdm.h` + `src/virtio_pci_intx_wdm.c`
  - Reusable INTx ISR + DPC pair for virtio-pci modern devices.
  - The ISR reads the ISR status byte (read-to-ack) as the INTx deassert/ack
    operation, then schedules a DPC which dispatches queue/config work.

- `include/virtio_pci_contract.h` + `src/virtio_pci_contract.c`
  - Validates Aero contract v1 PCI identity (Revision ID = `0x01`, modern device
    ID space `0x1040+<virtio device id>`).

### Legacy/transitional transport (optional)

- `include/virtio_pci_legacy.h` + `src/virtio_pci_legacy.c`
  - Legacy/transitional virtio-pci transport (virtio 0.9 I/O-port register block).
  - 32-bit feature negotiation (low 32 bits only).
  - Queue programming via `QUEUE_PFN = (queue_paddr >> 12)`.

### Virtqueues

- `include/virtqueue_split.h` + `src/virtqueue_split.c`
  - Portable split ring (`vring`) implementation (descriptor table + avail ring + used ring).

- `include/virtio_queue.h` + `src/virtio_queue.c`
  - Windows kernel convenience wrapper around split rings:
    - allocates descriptor/avail/used in a single physically-contiguous block (convenient but not required by modern)
      and maintains a descriptor free list
    - programs queues via `VirtioPciSetupQueue` and sets `queue_enable`
    - caches the queue’s notify address and kicks via the modern notify region (`NOTIFY_CFG`)

- `include/virtio_os.h` + `os_shim/`
  - OS abstraction used by the portable code and host-side unit tests.
  - Reference shims exist for NDIS, StorPort, and WDF.

## Using the modern transport

At a high level, drivers using Aero contract v1 devices should:

### Miniports (NDIS / StorPort)

1. **Read a PCI config snapshot** (typically 256 bytes) and validate identity
   (vendor/device/revision ID).
2. **Map BAR0 MMIO** using your driver framework:
   - NDIS: `NdisMMapIoSpace`
   - StorPort: `StorPortGetDeviceBase`
3. **Initialize the modern transport** (parses virtio vendor capabilities):
   - `VirtioPciModernMiniportInit(&Dev, Bar0Va, Bar0Len, PciCfg, PciCfgLen)`
4. **Negotiate features** (64-bit; always requires `VIRTIO_F_VERSION_1`):
   - `VirtioPciNegotiateFeatures(&Dev, Required, Wanted, &Negotiated)`
5. **Program queues** and cache notify addresses:
   - Either call `VirtioPciSetupQueue(...)` / `VirtioPciGetQueueNotifyAddress(...)` directly, or
   - Use the convenience wrapper `VirtioQueueCreate(&Dev, &Queue, QueueIndex)`.
6. **Handle INTx**:
   - Read the ISR status byte in the ISR to ACK/deassert (read-to-ack), then do
     queue work in a DPC.
   - Miniport primitive: `VirtioPciReadIsr(&Dev)`.

### WDM drivers

WDM drivers can use the WDM transport + INTx helper:

1. `VirtioPciModernWdmInit(LowerDeviceObject, &Dev)`
2. `VirtioPciModernWdmMapBars(&Dev, ResourcesRaw, ResourcesTranslated)`
3. `VirtioPciNegotiateFeatures(&Dev, ...)`
4. `VirtioPciSetupQueue(&Dev, ...)` / `VirtioPciNotifyQueue(&Dev, ...)` (or your own queue implementation)
5. `VirtioIntxConnect(...)` / `VirtioIntxDisconnect(...)`

Note: `virtio_queue.*` is built on the miniport `VIRTIO_PCI_DEVICE` type from
`virtio_pci_modern_miniport.h` and is primarily intended for miniports. WDM
drivers typically pair `VirtioPciSetupQueue` with `virtqueue_split.*` directly or
use their own queue wrapper.

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
instead.

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

`drivers/windows7/virtio/common/tests/` contains a small user-mode test program
that builds the portable core with fake I/O backends and validates:

- descriptor chain allocation/free under u16 index wraparound
- avail/used index handling
- indirect descriptor table building
- randomized fuzz sequences (invariants/no corruption)
- virtio-pci legacy vs modern transport behavior (cap parsing, `VIRTIO_F_VERSION_1`
  enforcement, queue programming via `queue_desc/queue_avail/queue_used` + `queue_enable`,
  notify doorbells, ISR read-to-ack)

The test CMake project also builds `virtio_pci_cap_parser_tests`, which exercises the
portable virtio PCI capability parser used by the miniport and WDM transports.

Build with CMake:

```sh
cmake -S tests -B build
cmake --build build
ctest --test-dir build --output-on-failure
```
