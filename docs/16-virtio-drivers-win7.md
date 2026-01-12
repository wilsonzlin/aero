# 16 - Virtio Drivers (Windows 7 guest)

## Scope

This document captures the shared plumbing required to build **virtio 1.0 PCI “modern”** drivers for a **Windows 7** guest (typically KMDF/WDF). Device-specific drivers (virtio-blk, virtio-net, virtio-input, etc.) should reuse a common transport + virtqueue layer rather than reimplementing the spec repeatedly.

The goal is to describe what needs to exist *before* writing any device-specific virtio driver.

For the definitive Aero interoperability contract (virtio device IDs, required features, queue sizes, and transport rules), treat:

- [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO`)

as authoritative. If this document ever disagrees with the contract, the contract wins.

---

## Virtio 1.0 PCI modern transport

### Capability discovery (PCI vendor-specific capabilities)

Virtio PCI devices expose their modern interface via **PCI vendor-specific capabilities** (capability ID `0x09`). Each capability is a `virtio_pci_cap` (or extension of it) that points at a sub-region of a BAR:

- `bar` – which BAR contains the region
- `offset` / `length` – byte range inside the BAR
- `cfg_type` – which virtio structure the region contains

For modern virtio 1.0 drivers, the important `cfg_type` values are:

| cfg_type | Name                       | Purpose |
| ---: | -------------------------- | ------- |
| 1 | Common configuration         | Feature negotiation, status, queue programming |
| 2 | Notify configuration         | Queue “doorbell” writes (kicks) |
| 3 | ISR status                   | Interrupt cause + acknowledgement (read-to-clear) |
| 4 | Device-specific configuration| Per-device config space (e.g., MAC, capacity) |
| 5 | PCI configuration access     | Optional/rarely needed |

Notes for Windows driver authors:

- In `EvtDevicePrepareHardware`, enumerate the device’s PCI capabilities (via bus interface / config space reads) and record the capabilities you need.
- Map BAR memory from the translated resource list (`CmResourceTypeMemory` / `CmResourceTypeMemoryLarge`) and compute each capability’s effective virtual address as `bar_va + cap.offset`.
  - On some x64 systems, PCI MMIO ranges (especially BARs above 4 GiB) can be reported as `CmResourceTypeMemoryLarge`. The `Length40/48/64` fields are stored in scaled units and must be decoded back to bytes (see `docs/windows/win7-miniport-virtio-pci-modern.md` for details).
- Multiple virtio capabilities can live in the same BAR; only map each BAR once.

#### Portable capability-list parser (hardware-free regression tests)

This repo includes a small **portable C99** module that implements the capability-list walk + `virtio_pci_cap` parsing logic, along with synthetic config-space unit tests that run on Linux CI:

- Parser: `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.{h,c}`
- Tests: `drivers/win7/virtio/tests/virtio_pci_cap_parser_test.c`

Run locally:

```bash
bash ./drivers/win7/virtio/tests/build_and_run.sh
```

### Required MMIO regions

A minimal modern virtio driver should expect to map these regions:

1. **Common config** (`cfg_type = 1`)
   - Contains `device_status`, `device_feature[_select]`, `driver_feature[_select]`, `num_queues`, and the queue programming registers.
2. **Notify area** (`cfg_type = 2`)
   - A write-only MMIO region used to notify (“kick”) a queue.
   - Uses `notify_off_multiplier` and `queue_notify_off` to compute the address to write.
3. **ISR status** (`cfg_type = 3`)
   - A single byte; reading it returns interrupt cause bits and clears them.
4. **Device config** (`cfg_type = 4`)
   - Device-specific structure defined by the device type (blk/net/input/etc.).

Many virtio drivers also want MSI-X resources (standard PCI MSI-X capability), but that is *not* a virtio vendor capability.

---

## Driver init sequence (status bits + feature negotiation)

Virtio devices use the `device_status` field in the common config. The important status bits are:

| Bit | Name | Meaning |
| ---: | ---- | ------- |
| 0x01 | ACKNOWLEDGE | Driver found the device |
| 0x02 | DRIVER | Driver knows how to drive the device |
| 0x04 | DRIVER_OK | Driver is fully set up |
| 0x08 | FEATURES_OK | Feature negotiation completed |
| 0x40 | DEVICE_NEEDS_RESET | Device hit an error and wants reset |
| 0x80 | FAILED | Driver gave up / fatal error |

Implementation-oriented sequence (modern transport):

1. **RESET**
   - Write `0` to `device_status`.
2. **ACK → DRIVER**
   - Write `ACKNOWLEDGE`, then `ACKNOWLEDGE | DRIVER`.
3. **FEATURES**
   - Read device features via:
     - write `device_feature_select = 0`, read `device_feature`
     - write `device_feature_select = 1`, read `device_feature`
   - Compute supported driver feature mask.
   - Write driver features via `driver_feature_select` + `driver_feature`.
4. **FEATURES_OK**
   - Set `FEATURES_OK` in `device_status`.
   - Read `device_status` back; if `FEATURES_OK` is not still set, the device rejected features → set `FAILED`.
5. **QUEUES**
   - Discover queue count via `num_queues`.
   - For each required queue:
     - program addresses for descriptor/avail/used rings
     - set `queue_size`, `queue_enable`, `queue_msix_vector` (if using MSI-X)
6. **DRIVER_OK**
   - Set `DRIVER_OK` once queues, interrupts, and device config are ready.

If at any point `DEVICE_NEEDS_RESET` is observed, the safe recovery path is to reset the device (write 0 to `device_status`) and restart initialization.

### Config generation (safe device-config reads)

Modern virtio provides `config_generation` to let the driver read device-specific config atomically:

1. Read `gen0 = config_generation`.
2. Read the device-specific config structure (possibly multiple MMIO reads).
3. Read `gen1 = config_generation`.
4. If `gen0 != gen1`, retry.

This matters most for larger config structures (e.g., net config) or when the device can update config at runtime.

---

## Virtqueue split ring (virtio 1.0)

Virtio 1.0 drivers commonly use **split virtqueues** (descriptor table + avail ring + used ring) in guest memory.

For the detailed split-ring virtqueue implementation algorithms (descriptor free list + cookies, ordering/barriers, EVENT_IDX, indirect descriptors, and end-to-end virtio-input-style usage), see:

* [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md)

### Memory layout

A split virtqueue consists of:

1. **Descriptor table** (`virtq_desc[qsz]`)
   - 16 bytes each: `{ addr: u64, len: u32, flags: u16, next: u16 }`
2. **Avail ring**
   - `{ flags: u16, idx: u16, ring: u16[qsz], (used_event: u16 if EVENT_IDX) }`
3. **Used ring**
   - `{ flags: u16, idx: u16, ring: used_elem[qsz], (avail_event: u16 if EVENT_IDX) }`
   - `used_elem` is 8 bytes: `{ id: u32, len: u32 }`

Alignment requirements (practical rules):

- Descriptor table: 16-byte aligned (natural alignment works if the base is aligned).
- Avail ring: 2-byte aligned.
- Used ring: 4-byte aligned.

When packing into one allocation, round up the start of the used ring to a 4-byte boundary.

### Why WDF common buffers are used on Windows 7

Virtqueue rings must live in **DMA-visible, nonpaged memory**:

- The device performs DMA reads of descriptors/avail and DMA writes to the used ring.
- Physical addresses of the rings are programmed into the common config (`queue_desc`, `queue_avail`, `queue_used`).

On Windows 7 KMDF, a practical approach is:

1. Create a DMA enabler (`WdfDmaEnablerCreate`) with a profile compatible with the device (typically scatter/gather).
2. Allocate per-queue ring memory as a **common buffer** (`WdfCommonBufferCreate`).
   - Common buffers are contiguous, nonpaged, and provide:
     - a CPU virtual address (for the driver to fill rings)
     - a device physical/logical address (to program the queue registers)

For request data buffers (e.g., block I/O payloads), device-specific drivers typically use WDF DMA transactions and build descriptor chains that reference those DMA-mapped buffers. The ring structures themselves still live in a common buffer.

### Notify (“kick”) path

Modern virtio uses the notify capability plus per-queue notify offsets:

1. Read `queue_notify_off` from common config for the selected queue.
2. Compute:
   - `notify_addr = notify_base + queue_notify_off * notify_off_multiplier`
3. Write the queue index (usually a 16-bit value) to `notify_addr`.

Drivers typically “kick” only after updating the avail ring and performing appropriate ordering (e.g., memory barriers) so the device never sees a partially written descriptor chain.

---

## Interrupt handling (MSI-X and legacy INTx)

Virtio devices can signal interrupts via:

- **Legacy INTx** (pin-based, level-triggered)
- **MSI-X** (message-signaled, multiple vectors)

### ISR status capability (read-to-clear)

The ISR status capability (`cfg_type = 3`) is a single byte:

- Bit 0: “queue interrupt”
- Bit 1: “device config changed”

Reading this byte acknowledges/clears the pending interrupt cause in the device (read-to-clear).

For **INTx** (level-triggered), reading this register in the ISR is required to deassert the line. For **MSI-X**, drivers typically do not rely on `isr_status` for ACK/routing (the message vector already identifies the source), but the register may still be useful as a fallback/debug signal.

### MSI-X on Windows 7 (message-signaled interrupts)

When a PCI device is configured for MSI-X, Windows exposes one or more **message interrupt resources**. In WDF, this typically means:

- In `EvtDevicePrepareHardware`, identify `CmResourceTypeInterrupt` descriptors where the translated flags indicate a message interrupt.
- Create one `WDFINTERRUPT` per message (or whatever mapping strategy the driver uses), then associate:
  - one message vector with the device configuration change interrupt
  - one message vector per virtqueue (common for net with separate RX/TX)

Virtio’s side of MSI-X routing is programmed through `virtio_pci_common_cfg`:

- `msix_config` selects which MSI-X vector the device uses for config-change interrupts.
- `queue_msix_vector` (for a selected queue via `queue_select`) selects which MSI-X vector the device uses for that queue.
- Writing `0xFFFF` disables MSI-X for that vector.

If MSI-X resources are not available, the driver should fall back to a single INTx interrupt and use the ISR status byte to demultiplex causes.

### Interrupt service flow

A typical WDF flow is:

1. **ISR**: do the minimum:
   - read ISR status (for INTx, always; for MSI-X, often still safe)
   - mask/unmask as needed
   - queue a DPC
2. **DPC**:
   - drain used ring entries for affected virtqueues
   - complete pending requests / indicate packets / report input events

Device-specific drivers should avoid doing heavy work in the ISR.

## See also (in-repo bring-up guides)

- WDM bring-up (caps + BAR mapping + queues + INTx): [`docs/windows/virtio-pci-modern-wdm.md`](./windows/virtio-pci-modern-wdm.md)
- Miniport bring-up (NDIS/StorPort): [`docs/windows/win7-miniport-virtio-pci-modern.md`](./windows/win7-miniport-virtio-pci-modern.md)
- KMDF interrupts guide (MSI-X vs INTx): [`docs/windows/virtio-pci-modern-interrupts.md`](./windows/virtio-pci-modern-interrupts.md)
