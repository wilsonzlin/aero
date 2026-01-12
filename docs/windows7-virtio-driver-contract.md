# Aero Windows 7 Virtio Device Contract (Definitive)
<!--
This document is a *binding* interoperability contract between:
  - Aero’s virtio device models (emulator side), and
  - the Windows 7 Aero virtio drivers (guest side).

If a behavior is not described here, do not assume QEMU behavior.
Either update this document (and bump the contract version) or do
not rely on the behavior.
-->

**Contract ID:** `AERO-W7-VIRTIO`  
**Contract version:** `1.0` (PCI Revision ID = `0x01`)  
**Applies to:** Windows 7 SP1 (x86 + x64) guest drivers: `virtio-blk`, `virtio-net`, `virtio-snd`, `virtio-input`  

Normative language: **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are to be interpreted as described in RFC 2119 / RFC 8174.

## 0. Scope and design goals

This contract intentionally specifies a **small, strict, testable** subset of virtio sufficient for reliable Windows 7 operation.

### 0.1 In scope

- **Transport:** `virtio-pci` **modern** (virtio 1.0+) using PCI vendor-specific capabilities and **MMIO** register blocks.
- **Virtqueues:** split virtqueues (descriptor table + avail ring + used ring).
- **Interrupts:** PCI **INTx** (required). MSI-X is permitted but is **not required** by contract v1 (see §1.8).

### 0.2 Out of scope (explicitly not required in contract v1)

- virtio-pci **legacy** / transitional I/O port transport (the BAR0 I/O register map).
- Packed rings (`VIRTIO_F_RING_PACKED` / packed virtqueue format).
- SR-IOV, IOMMU/IOTLB (`VIRTIO_F_IOMMU_PLATFORM` / `VIRTIO_F_ACCESS_PLATFORM`), live migration.
- Host offloads (TSO/CSO/UFO) beyond what is explicitly described.

If any out-of-scope item becomes required, it MUST be added as a **new contract major version** (see §4).

### 0.3 Reference implementations in this repo

This contract describes **device/driver-visible behavior**.

For canonical C layout of the modern common configuration structure, see:

- `drivers/win7/virtio/virtio-core/include/virtio_spec.h`

For virtio-pci modern transport reference code in this repo, see:

- `drivers/windows/virtio/pci-modern/` (generic WDF-free transport; used by `virtio-snd`)
- `drivers/windows7/virtio/common/` (miniport-friendly transport shim; used by `virtio-blk`/`virtio-net`)
- `drivers/win7/virtio/virtio-core/` (portable cap parsing + layout/identity; used by `virtio-input`)

For a host-buildable Rust crate that locks down struct sizes/offsets used by both sides, see:

- `drivers/protocol/virtio/`

## 1. Transport: virtio-pci modern (PCI capabilities + MMIO)

### 1.1 PCI identification

All Aero virtio devices are exposed as **virtio-pci modern** devices with the standard virtio PCI Vendor ID.

**Vendor ID:** `0x1AF4` (virtio)

**PCI Revision ID:** `0x01` (**MUST** match contract major version; see §4.1)

**Subsystem Vendor ID:** `0x1AF4`

#### 1.1.1 PCI Device IDs (modern-only ID space)

Contract v1 uses the virtio 1.0+ “modern” virtio-pci Device ID space:

`PCI Device ID = 0x1040 + <virtio device id>`

| Virtio device | Virtio device id | PCI Device ID |
|--------------|------------------|---------------|
| virtio-net   | 1                | `0x1041`      |
| virtio-blk   | 2                | `0x1042`      |
| virtio-input | 18               | `0x1052`      |
| virtio-snd   | 25               | `0x1059`      |

Drivers MUST bind on Vendor/Device ID and MUST assume the **modern** virtio-pci transport.

#### 1.1.2 Subsystem IDs (Aero-specific)

Subsystem Device IDs are used to distinguish device variants (for example, virtio-input keyboard vs mouse).

| Device instance | Subsystem Device ID |
|----------------|---------------------|
| virtio-net     | `0x0001` |
| virtio-blk     | `0x0002` |
| virtio-snd     | `0x0019` |
| virtio-input (keyboard) | `0x0010` |
| virtio-input (mouse)    | `0x0011` |

Drivers MUST NOT rely on these subsystem IDs for correctness; they exist to aid debugging and optional device matching.

### 1.2 BAR layout and endianness

Each Aero virtio device exposes a single MMIO BAR for virtio configuration:

- **BAR0:** 64-bit **MMIO**, little-endian, size **0x4000 bytes**.
- No I/O space BARs are required or implemented by contract v1.

All multi-byte fields in virtio MMIO regions are **little-endian**.

### 1.3 PCI capability requirements (virtio vendor-specific caps)

The PCI configuration space MUST expose a valid capability list (PCI Status bit 4 set; capability pointer at offset `0x34`), containing the following **vendor-specific** capabilities (PCI cap ID `0x09`):

- `VIRTIO_PCI_CAP_COMMON_CFG` (`cfg_type = 1`)
- `VIRTIO_PCI_CAP_NOTIFY_CFG` (`cfg_type = 2`)
- `VIRTIO_PCI_CAP_ISR_CFG` (`cfg_type = 3`)
- `VIRTIO_PCI_CAP_DEVICE_CFG` (`cfg_type = 4`)

Capability list invariants (enforced by `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`):

- The list MUST be acyclic (`cap_next` must not loop).
- Capability pointers MUST be 4-byte aligned.
- Each `virtio_pci_cap` MUST have `cap_len >= 16`.
- The notify capability MUST have `cap_len >= 20` (to include `notify_off_multiplier`).

If any required virtio capability is missing or malformed, drivers MUST treat the device as unsupported.

### 1.4 Fixed MMIO layout used by all Aero virtio devices

Although virtio-pci capabilities allow arbitrary placement, Aero contract v1 fixes a single layout so implementers can verify conformance without guessing.

All capabilities reference **BAR0** with the following offsets/lengths:

| Capability | `cfg_type` | BAR | Offset | Length |
|-----------|------------|-----|--------|--------|
| Common configuration | 1 | 0 | `0x0000` | `0x0100` |
| Notify configuration | 2 | 0 | `0x1000` | `0x0100` |
| ISR configuration    | 3 | 0 | `0x2000` | `0x0020` |
| Device configuration | 4 | 0 | `0x3000` | `0x0100` |

#### 1.4.1 Undefined MMIO behavior

- Reads from undefined MMIO offsets within BAR0 MUST return all-zeros for the requested width.
- Writes to undefined MMIO offsets within BAR0 MUST be ignored.

#### 1.4.2 Driver validation policy (permissive vs strict)

The Windows 7 virtio transport code in this repo supports **two** layout validation modes:

- **Permissive (default):** accept any valid virtio-pci modern capability placement (for example, QEMU’s multi-BAR layout), as long as the required capabilities are present and well-formed.
- **Strict (contract conformance):** enforce the fixed BAR0 layout in §1.4 and fail device initialization early if the layout does not match:
  - BAR0 is MMIO and `len >= 0x4000`
  - COMMON/NOTIFY/ISR/DEVICE all have `bar = 0` and the contract offsets (with the contract lengths as minimums)
  - `notify_off_multiplier == 4`

To enable **strict** mode in the shared transport library (`virtio-core`), build the driver(s) with:

```
VIRTIO_CORE_ENFORCE_AERO_MMIO_LAYOUT=1
```

This switch is intended for emulator/device-model conformance testing; the default permissive mode keeps QEMU usable as a compatibility test target.

### 1.5 Common configuration (`virtio_pci_common_cfg`)

The common configuration region is a MMIO mapping of `struct virtio_pci_common_cfg` (little-endian). For the canonical packed C layout, see `drivers/win7/virtio/virtio-core/include/virtio_spec.h`.

Key semantics:

#### 1.5.0 Selector register serialization (required for correctness)

The following `common_cfg` fields are **global selectors** that affect subsequent accesses:

- `device_feature_select` / `device_feature`
- `driver_feature_select` / `driver_feature`
- `queue_select` / all `queue_*` fields

Because these selectors are shared device-global state, any multi-step sequence that uses them MUST be serialized by the driver (for example, by a per-device spinlock) to avoid races between queues/DPCs/power callbacks.

The device MUST implement the selector behavior exactly as described; it MUST NOT provide per-CPU or per-queue selector state.

##### `common_cfg` MMIO offsets (contract v1)

Within the `COMMON_CFG` capability region (BAR0 + `0x0000`), `virtio_pci_common_cfg` is laid out as follows (little-endian):

| Offset | Size | Field | Access | Notes |
|--------|------|-------|--------|------|
| `0x00` | 4 | `device_feature_select` | R/W | Selector (0 = low 32 bits, 1 = high 32 bits). |
| `0x04` | 4 | `device_feature` | R | Feature bits selected by `device_feature_select`. |
| `0x08` | 4 | `driver_feature_select` | R/W | Selector (0/1). |
| `0x0C` | 4 | `driver_feature` | R/W | Feature bits selected by `driver_feature_select`. |
| `0x10` | 2 | `msix_config` | R/W | MSI-X vector for config interrupts (`VIRTIO_PCI_MSI_NO_VECTOR` / `0xFFFF` = disabled). Optional in v1. |
| `0x12` | 2 | `num_queues` | R | Number of virtqueues implemented by the device. |
| `0x14` | 1 | `device_status` | R/W | Virtio status byte. Writing 0 resets the device. |
| `0x15` | 1 | `config_generation` | R | Config generation counter. |
| `0x16` | 2 | `queue_select` | R/W | Selects which queue subsequent `queue_*` fields refer to. |
| `0x18` | 2 | `queue_size` | R | Maximum size for selected queue (in descriptors). |
| `0x1A` | 2 | `queue_msix_vector` | R/W | MSI-X vector for selected queue (`VIRTIO_PCI_MSI_NO_VECTOR` / `0xFFFF` = disabled). Optional in v1. |
| `0x1C` | 2 | `queue_enable` | R/W | 0 = disabled, 1 = enabled. |
| `0x1E` | 2 | `queue_notify_off` | R | Notify offset for selected queue. |
| `0x20` | 8 | `queue_desc` | R/W | 64-bit guest physical address of descriptor table. |
| `0x28` | 8 | `queue_avail` | R/W | 64-bit guest physical address of avail ring. |
| `0x30` | 8 | `queue_used` | R/W | 64-bit guest physical address of used ring. |

The structure size is 56 bytes (`0x38`). The offsets above are enforced in the Windows driver code via `C_ASSERT` and in Rust via `drivers/protocol/virtio` unit tests.

#### 1.5.1 Feature negotiation (64-bit)

Feature negotiation uses the selector pattern:

- Driver writes `device_feature_select = 0` then reads `device_feature` → low 32 bits.
- Driver writes `device_feature_select = 1` then reads `device_feature` → high 32 bits.

Similarly for driver features:

- Driver writes `driver_feature_select` then writes `driver_feature`.

Selector behavior:

- `*_feature_select` values other than 0 or 1 MUST be treated as reserved; reads of `*_feature` for reserved selects MUST return 0 and writes MUST be ignored.

#### 1.5.2 Required feature bits

All Aero virtio devices MUST offer and require:

- `VIRTIO_F_VERSION_1` (bit 32) = 1

Drivers MUST set (accept) `VIRTIO_F_VERSION_1`.

#### 1.5.3 Device status and reset

`device_status` implements the virtio device status state machine.

- Writing `device_status = 0` MUST reset the device:
  - All queues become disabled (`queue_enable = 0`).
  - All pending interrupts are cleared.
  - Feature negotiation state is cleared.
  - Device returns to pre-init state.

Status bits (standard virtio):

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | `ACKNOWLEDGE` | Guest has noticed the device. |
| 1   | `DRIVER` | Guest knows how to drive the device. |
| 2   | `DRIVER_OK` | Driver is fully initialized. |
| 3   | `FEATURES_OK` | Driver accepted feature set. |
| 6   | `DEVICE_NEEDS_RESET` | Device encountered an error; guest must reset. |
| 7   | `FAILED` | Driver has given up. |

The device MUST clear `FEATURES_OK` if the driver sets unsupported feature bits.

#### 1.5.4 Queue selection and programming

Queue configuration uses the selector pattern:

- Driver writes `queue_select = <index>`.
- Driver reads `queue_size` (read-only) and `queue_notify_off` (read-only).
- Driver writes `queue_desc`, `queue_avail`, `queue_used`.
- Driver writes `queue_enable = 1` to activate.

Queue index range handling:

- If `queue_select` selects a queue index `>= num_queues`:
  - `queue_size` MUST read as 0.
  - `queue_notify_off` MUST read as 0.
  - Writes to `queue_desc/avail/used` and `queue_enable` MUST be ignored.

#### 1.5.5 `config_generation`

- `config_generation` MAY remain 0 for the lifetime of the device if the device config is static.
- If the device modifies device-specific config at runtime, it MUST increment `config_generation` and SHOULD trigger a config change interrupt (see §1.8).

### 1.6 Notify region (`VIRTIO_PCI_CAP_NOTIFY_CFG`)

The notify region is a MMIO “doorbell” area.

The notify capability MUST set:

- `notify_off_multiplier = 4`

For each queue `q`, the device MUST report:

- `queue_notify_off(q) = q`

To notify a queue, the driver writes the queue index (16-bit) to:

`notify_base + queue_notify_off(q) * notify_off_multiplier`

Device behavior:

- Any write to a valid notify address MUST schedule processing for the corresponding queue.
- Devices MUST accept both 16-bit and 32-bit writes at notify addresses (some drivers/platforms use 32-bit writes).

### 1.7 ISR region (`VIRTIO_PCI_CAP_ISR_CFG`)

The ISR region contains a single 8-bit interrupt status register at offset 0.

ISR bits:

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | `QUEUE_INTERRUPT` | Device added entries to a used ring for at least one queue. |
| 1   | `CONFIG_INTERRUPT` | Device-specific config change. |
| 2-7 | - | Reserved; MUST be 0. |

ISR semantics:

- Reading the ISR byte returns the current pending bits and **acknowledges** them (read-to-ack).
- Reading MUST clear all bits that were returned.

If MSI-X is present and enabled, drivers SHOULD rely on the MSI-X vector to determine the cause; ISR is primarily for INTx and MSI fallbacks.

### 1.8 Interrupts

#### 1.8.1 INTx (required)

Contract v1 requires **legacy PCI INTx** support.

- The device MUST use PCI **INTA#** (interrupt pin = 1).
- The device MUST assert INTx when it sets any ISR cause bit (queue or config).
- The device MUST deassert INTx after the guest acknowledges all pending causes (i.e., after `ISR` is read and no further causes remain).

#### 1.8.2 Queue interrupt behavior

When the device publishes one or more used-ring entries for a queue:

- The device MUST set ISR bit 0 (`QUEUE_INTERRUPT`) and MUST assert INTx.

The device SHOULD suppress interrupts when the driver has set `VRING_AVAIL_F_NO_INTERRUPT` in the avail ring for that queue.

#### 1.8.3 Config interrupt behavior

If device-specific config changes at runtime:

- The device MUST set ISR bit 1 (`CONFIG_INTERRUPT`) and MUST assert INTx.

Contract v1 devices SHOULD NOT change config at runtime unless explicitly described in a per-device section.

#### 1.8.4 MSI-X (permitted but not required in contract v1)

Devices MAY expose a PCI MSI-X capability and MAY use `msix_config` / `queue_msix_vector` to deliver message-signaled interrupts, but Windows 7 drivers MUST remain functional when only INTx is available.

If MSI-X is implemented, it MUST preserve INTx/ISR semantics as a fallback:

- When MSI-X is enabled and a valid vector is assigned, the device MAY signal MSI-X instead of asserting INTx.
- When MSI-X is disabled or vectors are unassigned (`VIRTIO_PCI_MSI_NO_VECTOR` / `0xFFFF`), the device MUST use INTx + ISR as described above.

## 2. Virtqueue contract (split ring only)

> Implementation note (non-normative): for a Windows 7 KMDF-oriented split-ring virtqueue
> implementation guide (descriptor/free list management, ordering/barriers, optional EVENT_IDX,
> and indirect descriptor usage), see:
> - [`docs/virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md)

### 2.1 Split ring layout

Contract v1 uses the classic “split ring” (`vring`) layout:

```c
// Descriptor table (N entries, 16 bytes each)
struct vring_desc {
  le64 addr;
  le32 len;
  le16 flags;
  le16 next;
};

// Driver -> device (available ring)
struct vring_avail {
  le16 flags;
  le16 idx;
  le16 ring[N];
  // no used_event (EVENT_IDX not supported in contract v1)
};

// Device -> driver (used ring)
struct vring_used_elem {
  le32 id;
  le32 len;
};

struct vring_used {
  le16 flags;
  le16 idx;
  struct vring_used_elem ring[N];
  // no avail_event (EVENT_IDX not supported in contract v1)
};
```

#### 2.1.1 Size formulas (EVENT_IDX not supported in contract v1)

Given queue size **N**:

- `desc_bytes = 16 * N`
- `avail_bytes = 4 + 2 * N`
- `used_bytes = 4 + 8 * N`

Because `VIRTIO_F_RING_EVENT_IDX` is not negotiated in contract v1, there is no `used_event` or `avail_event` field.

### 2.2 Alignment requirements

The device MUST accept ring addresses with the standard virtio alignment requirements:

- Descriptor table address (`queue_desc`): **16-byte aligned**
- Avail ring address (`queue_avail`): **2-byte aligned**
- Used ring address (`queue_used`): **4-byte aligned**

### 2.3 Supported ring/queue feature bits

Contract v1 uses split rings only and defines the following ring-related features:

- `VIRTIO_F_RING_INDIRECT_DESC` (bit 28): **supported and MUST be offered**
- `VIRTIO_F_RING_EVENT_IDX` (bit 29): **not offered** (always-notify semantics)
- `VIRTIO_F_RING_PACKED` (bit 34): **not offered**

### 2.4 Descriptor flags and chaining

Supported descriptor flags:

| Flag | Value | Meaning | Support |
|------|-------|---------|---------|
| `NEXT` | `1` | This descriptor continues at `next`. | **MUST** |
| `WRITE` | `2` | Device writes into the buffer. | **MUST** |
| `INDIRECT` | `4` | Descriptor points to an indirect table. | **MUST** when `INDIRECT_DESC` is negotiated |

#### 2.4.1 Indirect descriptors

If `VIRTIO_F_RING_INDIRECT_DESC` is negotiated:

- The driver MAY submit a descriptor with `INDIRECT` set.
- For an indirect descriptor:
  - `addr` points to a guest-physical contiguous array of `vring_desc`.
  - `len` is the size in bytes of that array and MUST be a multiple of 16.

If the feature is not negotiated, `INDIRECT` MUST NOT be used.

### 2.5 Notifications and interrupt suppression

- Drivers MUST notify via the notify region (§1.6) after publishing new available entries.
- Devices MUST raise interrupts on used-ring updates unless suppressed via `VRING_AVAIL_F_NO_INTERRUPT`.

Contract v1 does not use EVENT_IDX; drivers MUST NOT assume `used_event/avail_event` fields exist.

### 2.6 Guest physical memory access (DMA model)

The device performs DMA by reading/writing guest physical memory pointed to by descriptor addresses and ring addresses.

Rules:

- All addresses in rings/descriptors are **guest physical (DMA) addresses**.
- The device MUST bounds-check each `(addr, len)` before access.
- The device MUST support unaligned buffer addresses and lengths.

### 2.7 DMA addresses >4 GiB

The device MUST accept and correctly access 64-bit guest physical addresses, including addresses above 4 GiB (required for Windows 7 x64 and for DMA frameworks that return high logical addresses).

## 3. Per-device contracts

All devices inherit the transport and virtqueue rules above.

### 3.1 virtio-blk (block)

#### 3.1.1 PCI IDs

- Vendor ID: `0x1AF4`
- Device ID: `0x1042`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0002`
- Revision ID: `0x01`

#### 3.1.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `requestq` | driver ↔ device | **128** |

#### 3.1.3 Feature bits

The device MUST offer:

- `VIRTIO_F_VERSION_1` (32)
- `VIRTIO_F_RING_INDIRECT_DESC` (28)
- `VIRTIO_BLK_F_SEG_MAX` (2)
- `VIRTIO_BLK_F_BLK_SIZE` (6)
- `VIRTIO_BLK_F_FLUSH` (9)

The device MUST NOT offer:

- `VIRTIO_F_RING_EVENT_IDX` (29)
- `VIRTIO_BLK_F_RO`
- Discard / write-zeroes / multi-queue features

#### 3.1.4 Device config layout (`DEVICE_CFG` capability)

virtio-blk config (little-endian):

| Offset | Size | Field | Notes |
|--------|------|-------|------|
| `0x00` | 8 | `capacity` | Number of **512-byte sectors**. Read-only. |
| `0x08` | 4 | `size_max` | Not used; MUST be 0. |
| `0x0C` | 4 | `seg_max` | Max data segments per request (excludes header + status). |
| `0x10` | 4 | `geometry` | Not used; MUST be 0. |
| `0x14` | 4 | `blk_size` | Logical block size in bytes (typically 512). |
| `0x18+` | var | - | Remaining standard fields are not required; MUST read as 0. |

#### 3.1.5 Request format and supported request types

Each request is a single descriptor chain:

1. **Header** (`virtio_blk_outhdr`, 16 bytes), device-readable:

```c
struct virtio_blk_outhdr {
  le32 type;
  le32 ioprio;   // ignored by Aero; driver MUST set to 0
  le64 sector;   // starting sector (512-byte units)
};
```

2. **Data buffers** (0 or more descriptors)
3. **Status byte** (1 byte), device-writable (last descriptor in chain)

Data buffer direction rules:

- For `VIRTIO_BLK_T_IN` (read): all data buffers MUST be device-writable (`WRITE` flag set).
- For `VIRTIO_BLK_T_OUT` (write): all data buffers MUST be device-readable (`WRITE` flag clear).
- For `VIRTIO_BLK_T_FLUSH`: no data buffers are present (header + status only).

Supported `type` values:

| Name | Value | Support |
|------|-------|---------|
| `VIRTIO_BLK_T_IN` | 0 | **MUST** |
| `VIRTIO_BLK_T_OUT` | 1 | **MUST** |
| `VIRTIO_BLK_T_FLUSH` | 4 | **MUST** |

All other request types MUST complete with status `VIRTIO_BLK_S_UNSUPP`.

Status byte values:

| Name | Value |
|------|-------|
| `VIRTIO_BLK_S_OK` | 0 |
| `VIRTIO_BLK_S_IOERR` | 1 |
| `VIRTIO_BLK_S_UNSUPP` | 2 |

I/O semantics:

- For `VIRTIO_BLK_T_IN` and `VIRTIO_BLK_T_OUT`, the request MUST contain between 1 and `seg_max` data buffer descriptors (exclusive of header + status); otherwise `IOERR`.
- Total data length MUST be a multiple of 512 bytes; otherwise `IOERR`.
- Requests beyond disk capacity MUST return `IOERR`.
- The device MUST write the status byte *before* publishing the used-ring entry.
- The device MUST publish used-ring entries with `used.len = 0` (virtio-blk drivers must not depend on used lengths).

Flush semantics:

- `VIRTIO_BLK_T_FLUSH` MUST not complete until all prior completed `OUT` writes are durable in the backing store.
- If the backing store cannot provide durability guarantees, the device MUST still implement the ordering property (flush completes after all prior writes are visible) and MUST document any durability limitations in emulator implementation notes.

### 3.2 virtio-net (network)

#### 3.2.1 PCI IDs

- Vendor ID: `0x1AF4`
- Device ID: `0x1041`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0001`
- Revision ID: `0x01`

#### 3.2.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `rxq` | device → driver | **256** |
| 1 | `txq` | driver → device | **256** |

No control queue is implemented in contract v1.

#### 3.2.3 Feature bits

The device MUST offer:

- `VIRTIO_F_VERSION_1` (32)
- `VIRTIO_F_RING_INDIRECT_DESC` (28)
- `VIRTIO_NET_F_MAC` (5)
- `VIRTIO_NET_F_STATUS` (16)

The device MUST NOT offer:

- `VIRTIO_NET_F_MRG_RXBUF` (15)
- Any checksum/GSO/TSO offload features
- `VIRTIO_NET_F_CTRL_VQ`

#### 3.2.4 Device config layout (`DEVICE_CFG` capability)

virtio-net config (little-endian):

| Offset | Size | Field | Notes |
|--------|------|-------|------|
| `0x00` | 6 | `mac` | MAC address. Read-only. |
| `0x06` | 2 | `status` | Bit0 = `VIRTIO_NET_S_LINK_UP`. Read-only. |
| `0x08` | 2 | `max_virtqueue_pairs` | MUST be 1. Read-only. |

#### 3.2.5 Packet format and virtio-net header expectations

Contract v1 uses the classic 10-byte `struct virtio_net_hdr`:

```c
struct virtio_net_hdr {
  u8  flags;
  u8  gso_type;
  le16 hdr_len;
  le16 gso_size;
  le16 csum_start;
  le16 csum_offset;
};
```

Because no offload or mergeable-RX features are negotiated:

- The driver MUST set all header fields to 0 for TX.
- The device MUST ignore the header contents for TX.
- For RX, the device MUST write a zeroed header.

#### 3.2.6 Frame size rules

Contract v1 uses classic Ethernet II frames without FCS:

- Minimum frame length: 14 bytes (Ethernet header).
- Maximum frame length: 1514 bytes (Ethernet header + 1500 MTU payload).

Frame drop semantics:

- TX: if the driver submits a frame outside the valid size range, the device MUST drop it but MUST still complete the TX descriptor chain successfully.
- RX: if the host/backend delivers a frame outside the valid size range, the device MUST drop it and MUST NOT consume a posted RX chain for it.

#### 3.2.7 TX (driver → device)

Each TX submission is a descriptor chain:

1. Descriptor 0: device-readable `virtio_net_hdr` (len >= 10)
2. Descriptor 1..k: device-readable Ethernet frame bytes (no FCS)

Completion:

- The device MUST complete the chain with `used.len = 0` (TX is device-readable only).
- If the chain contains any writable (`WRITE`) descriptors, the device MUST drop the packet but MUST still complete the chain.

#### 3.2.8 RX (device → driver)

The driver supplies receive buffers via available descriptor chains.

Buffer requirements (driver):

- Each chain MUST start with a writable buffer of at least 10 bytes for `virtio_net_hdr`.
- The chain MUST provide at least 1514 bytes of writable payload space after the header (or packets may be dropped).

Receive behavior (device):

- For each received Ethernet frame, the device consumes exactly one available chain.
- The device writes:
  - a zeroed 10-byte header into the first buffer, and
  - the full Ethernet frame into subsequent writable buffer space.
- The device completes the chain with `used.len = 10 + frame_len`.
- If the provided buffers are insufficient, the device MUST drop the incoming frame and MUST NOT consume a chain for it.

> Implementation note (non-normative): Aero’s virtio-net model will drop a received frame immediately
> (without consuming the RX chain) if the next available chain does not have enough writable capacity
> for `10 + frame_len`. If no RX chains are available, the device may queue frames until buffers
> arrive.

### 3.3 virtio-input (keyboard/mouse)

Contract v1 exposes virtio-input as a **single multi-function PCI device** with **two PCI functions**:

- Function 0: one keyboard virtio-input device (**must** advertise multi-function via `header_type = 0x80`)
- Function 1: one mouse virtio-input device

Both share the same Vendor/Device ID (`0x1AF4:0x1052`) and are distinguished by subsystem ID and config strings.

#### 3.3.1 PCI IDs

Keyboard:

- Vendor ID: `0x1AF4`
- Device ID: `0x1052`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0010`
- Revision ID: `0x01`

Mouse:

- Vendor ID: `0x1AF4`
- Device ID: `0x1052`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0011`
- Revision ID: `0x01`

#### 3.3.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `eventq` | device → driver | **64** |
| 1 | `statusq` | driver → device | **64** |

#### 3.3.3 Feature bits

The device MUST offer:

- `VIRTIO_F_VERSION_1` (32)
- `VIRTIO_F_RING_INDIRECT_DESC` (28)

#### 3.3.4 Device config: discovery model

virtio-input uses a “selector” config scheme.

```c
struct virtio_input_config {
  u8 select;
  u8 subsel;
  u8 size;
  u8 reserved[5];
  u8 payload[128];
};
```

Contract v1 requires the following selectors:

- `VIRTIO_INPUT_CFG_ID_NAME`
- `VIRTIO_INPUT_CFG_ID_DEVIDS`
- `VIRTIO_INPUT_CFG_EV_BITS`

All other selectors MUST return `size = 0`.

Required `ID_NAME` strings:

- keyboard: `"Aero Virtio Keyboard"`
- mouse: `"Aero Virtio Mouse"`

Required `ID_DEVIDS` values:

- `bustype = 0x0006` (BUS_VIRTUAL)
- `vendor = 0x1AF4`
- `product = 0x0001` (keyboard) / `0x0002` (mouse)
- `version = 0x0001`

#### 3.3.5 Event format

```c
struct virtio_input_event {
  le16 type;
  le16 code;
  le32 value;
};
```

The device MUST use Linux input event types/codes (`EV_KEY`, `EV_REL`, `EV_SYN`, etc.) and MUST emit `EV_SYN/SYN_REPORT` after batches.

##### Event queue (`eventq`) buffer contract

- The driver posts writable buffers of size >= 8 bytes.
- The device MUST write exactly one `virtio_input_event` (8 bytes) per used entry, and complete the entry with `used.len = 8`.

##### Keyboard event requirements

- `type = EV_KEY` for key press/release.
- `value = 1` press, `0` release.
- The device SHOULD NOT send auto-repeat (`value = 2`).
- Minimum required supported key codes (device MUST advertise them via `EV_BITS`; it MAY support more):
  - `KEY_A`..`KEY_Z`
  - `KEY_0`..`KEY_9`
  - `KEY_ENTER`, `KEY_ESC`, `KEY_BACKSPACE`, `KEY_TAB`, `KEY_SPACE`
  - `KEY_LEFTSHIFT`, `KEY_RIGHTSHIFT`, `KEY_LEFTCTRL`, `KEY_RIGHTCTRL`, `KEY_LEFTALT`, `KEY_RIGHTALT`
  - `KEY_CAPSLOCK`
  - `KEY_F1`..`KEY_F12`
  - `KEY_UP`, `KEY_DOWN`, `KEY_LEFT`, `KEY_RIGHT`
  - `KEY_INSERT`, `KEY_DELETE`, `KEY_HOME`, `KEY_END`, `KEY_PAGEUP`, `KEY_PAGEDOWN`

##### Mouse event requirements (relative)

- Relative motion:
  - `type = EV_REL`, `code = REL_X` and `REL_Y`
  - `value` is a signed delta in counts
- Wheel:
  - `type = EV_REL`, `code = REL_WHEEL`
  - `value` is signed tick count (typically ±1)
- Buttons:
  - `type = EV_KEY`, `code = BTN_LEFT / BTN_RIGHT / BTN_MIDDLE`
  - `value = 1` press, `0` release

##### Status queue (`statusq`) behavior

The guest MAY send output/LED events via `statusq`.

- The device MUST consume and complete all `statusq` descriptors.
- The device MAY ignore the contents (LEDs need not be modeled in contract v1).

### 3.4 virtio-snd (audio)

#### 3.4.1 PCI IDs
 
- Vendor ID: `0x1AF4`
- Device ID: `0x1059`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0019`
- Revision ID: `0x01`

#### 3.4.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `controlq` | driver → device | **64** |
| 1 | `eventq` | device → driver | **64** |
| 2 | `txq` | driver → device (PCM playback) | **256** |
| 3 | `rxq` | device → driver (PCM capture) | **64** |

#### 3.4.2.1 Event queue behavior (contract v1)

Contract v1 does not define any virtio-snd event messages. The event queue is reserved for future
extensions.

- The device MUST accept and retain driver-posted `eventq` buffers.
- The device MUST NOT complete `eventq` buffers unless it has an event to deliver.
- Drivers MUST NOT depend on any `eventq` completions in contract v1.

#### 3.4.3 Feature bits

The device MUST offer:

- `VIRTIO_F_VERSION_1` (32)
- `VIRTIO_F_RING_INDIRECT_DESC` (28)

#### 3.4.4 Minimal PCM capability

The device exposes two fixed-format PCM streams:

Stream 0 (playback/output):

- Channels: 2
- Format: S16_LE
- Rate: 48,000 Hz

Stream 1 (capture/input):

- Channels: 1 (mono)
- Format: S16_LE
- Rate: 48,000 Hz

#### 3.4.5 Device config layout (`DEVICE_CFG` capability)

virtio-snd config (little-endian):

| Offset | Size | Field | Value (contract v1) |
|--------|------|-------|---------------------|
| `0x00` | 4 | `jacks` | `0` |
| `0x04` | 4 | `streams` | `2` |
| `0x08` | 4 | `chmaps` | `0` |

#### 3.4.6 Minimal control flow

Drivers and devices MUST follow the virtio-snd specification for message formats.

Contract v1 requires at minimum the ability to:

- query stream info (streams 0 and 1)
- set params (only the fixed params in §3.4.4)
- prepare/start/stop/release playback and capture

All unsupported commands MUST return `NOT_SUPP`.

Playback data path (`txq`):

- After start, the driver submits PCM buffers on `txq`.
- The device MUST play buffers in order and complete each buffer with OK status when consumed.
- On underrun, the device MUST output silence and continue.
- The device MAY reject playback buffers with payloads larger than **4 MiB** with `BAD_MSG` (implementation safety limit).

Capture data path (`rxq`):

- After start, the driver submits capture buffers on `rxq`.
- Each buffer consists of:
  - an OUT header (`stream_id: u32`, `reserved: u32`) selecting stream 1
  - one or more IN descriptors for PCM payload bytes (S16_LE mono)
  - a final IN descriptor with a `virtio_snd_pcm_status` response (8 bytes)
- The device MUST fill payload bytes with captured PCM samples when stream 1 is running.
- If not enough captured samples are available, the device MUST fill the missing part with silence and complete the buffer with OK status.
- If stream 1 is not running, the device MUST complete the buffer with `IO_ERR`.
- The device MAY reject capture buffers with payloads larger than **4 MiB** with `BAD_MSG` (implementation safety limit).

## 4. Versioning and compatibility

### 4.1 Contract version encoding

Contract major versions are encoded as:

- **PCI Revision ID**: `0x01` for contract v1.

Drivers MUST refuse to bind to devices with an unknown major version (Revision ID not recognized), unless explicitly built to support it.

> Implementation note (non-normative): many QEMU virtio PCI device models report `REV_00` by default.
> For contract-v1 testing under QEMU, pass `x-pci-revision=0x01` on each `-device virtio-*-pci,...`
> argument (the Win7 host harness in `drivers/windows7/tests/host-harness/` does this automatically).
>
> Packaging note (non-normative): Aero’s in-tree Windows 7 virtio driver packages are also typically
> **revision-gated in the INF** (`...&REV_01`) to avoid Windows installing a driver on a non-contract
> `REV_00` device and then failing to start (for example Device Manager Code 10) when the driver
> enforces the contract version at runtime.

### 4.2 Compatibility rules

- Major version bump: breaking changes allowed.
- Minor version bump: additive changes only (new optional feature bits, appended config fields, optional commands).

## 5. Conformance checklist

### 5.1 Emulator/device-model checklist

- [ ] Expose PCI IDs exactly as specified (§1.1, §3).
- [ ] Expose BAR0 MMIO layout and virtio capabilities exactly (§1.2–§1.4).
- [ ] Implement common_cfg selectors and queue programming semantics (§1.5).
- [ ] Implement notify doorbell semantics (§1.6).
- [ ] Implement INTx assertion/deassertion and ISR read-to-ack semantics (§1.7–§1.8).
- [ ] (Optional) If MSI-X is implemented, ensure it cleanly falls back to INTx + ISR when disabled/unavailable (§1.8.4).
- [ ] Implement split rings and indirect descriptors (§2.1–§2.4).
- [ ] Offer ring-related feature bits exactly as specified: MUST offer `VIRTIO_F_RING_INDIRECT_DESC` and MUST NOT offer `VIRTIO_F_RING_EVENT_IDX` / `VIRTIO_F_RING_PACKED` (§2.3).
- [ ] Bounds-check all guest physical memory accesses (§2.6).

### 5.2 Windows 7 driver checklist

- [ ] Bind by Vendor/Device IDs; verify PCI Revision ID (`0x01`) (§4.1).
- [ ] Parse virtio-pci modern capabilities (COMMON/NOTIFY/ISR/DEVICE) (§1.3).
- [ ] Negotiate `VIRTIO_F_VERSION_1` and required per-device features (§1.5.2, §3).
- [ ] Allocate rings using the split-ring layout and size formulas in §2.1.1; MUST NOT assume `used_event/avail_event` fields exist (EVENT_IDX not supported in v1).
- [ ] Program queues via common_cfg, then notify via notify region (§1.5.4, §1.6).
- [ ] Work correctly with INTx + ISR only; use MSI-X only when available and known-good (§1.8).
