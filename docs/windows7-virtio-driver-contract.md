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

### In scope

- **Transport:** `virtio-pci` **legacy** (I/O port BAR), split virtqueues
- **Devices:** `virtio-blk`, `virtio-net`, `virtio-input`, `virtio-snd`
- **Interrupts:** legacy PCI INTx (no MSI-X in contract v1)

### Out of scope (explicitly not required in contract v1)

- virtio-pci **modern** transport (PCI capabilities / MMIO common config)
- Packed rings (`VIRTIO_F_RING_PACKED` / packed virtqueue format)
- MSI-X, SR-IOV, IOMMU/IOTLB, live migration, device hotplug
- Host offloads (TSO/CSO/UFO, etc.) beyond what is explicitly described

If any out-of-scope item becomes required, it MUST be added as a **new contract version** (see §5).

## 1. Transport: virtio-pci legacy (I/O port BAR)

### 1.1 PCI identification

All Aero virtio devices are exposed as **virtio-pci legacy** devices with the standard virtio PCI Vendor ID.

**Vendor ID:** `0x1AF4` (virtio)

**PCI Revision ID:** `0x01` (**MUST** match contract major version; see §5)

**PCI Header Type:** `0x00` (normal endpoint)

**Subsystem Vendor ID:** `0x1AF4`  
**Subsystem Device ID:** device-specific (see per-device sections)

#### 1.1.1 PCI Device IDs (legacy/transitional ID space)

Contract v1 uses the legacy/transitional virtio-pci device ID space:

| Virtio device | Virtio ID | PCI Device ID |
|--------------|-----------|---------------|
| virtio-net   | 1         | `0x1000`      |
| virtio-blk   | 2         | `0x1001`      |
| virtio-input | 18        | `0x1011`      |
| virtio-snd   | 25        | `0x1018`      |

These IDs are chosen to match the conventional virtio-pci legacy ID scheme used by other hypervisors so that tooling and driver code can re-use well-known constants.

> Note: Contract v1 does **not** implement virtio “modern-only” device IDs (`0x1040+` range).

#### 1.1.2 PCI class codes (informational)

Drivers MUST match on Vendor/Device ID, not class code. Aero sets class codes as follows to aid OS device categorization:

| Device        | Base / Sub / ProgIF |
|--------------|----------------------|
| virtio-net   | `0x02 / 0x00 / 0x00` (Network / Ethernet) |
| virtio-blk   | `0x01 / 0x00 / 0x00` (Mass storage / SCSI-like) |
| virtio-input | `0x09 / 0x00 / 0x00` (Input device) |
| virtio-snd   | `0x04 / 0x01 / 0x00` (Multimedia / Audio) |

### 1.2 BAR layout and register endianness

Each virtio device exposes:

- **BAR0:** I/O space BAR (“port I/O”), **little-endian**, size **at least 0x100 bytes**.
- No MMIO BARs are required by this contract.

All multi-byte register and config accesses are **little-endian**.

### 1.3 Legacy virtio-pci register map (BAR0)

All offsets below are from the BAR0 I/O base.

| Offset | Size | Name | Access | Description |
|--------|------|------|--------|-------------|
| `0x00` | 4    | `HOST_FEATURES` | R | Device-offered feature bits `[31:0]`. |
| `0x04` | 4    | `GUEST_FEATURES` | W | Driver-accepted feature bits `[31:0]`. |
| `0x08` | 4    | `QUEUE_PFN` | R/W | Queue base physical page number (PFN). |
| `0x0C` | 2    | `QUEUE_NUM` | R | Queue size (number of descriptors) for selected queue. |
| `0x0E` | 2    | `QUEUE_SEL` | R/W | Selects which virtqueue subsequent accesses refer to. |
| `0x10` | 2    | `QUEUE_NOTIFY` | W | Doorbell: writing queue index notifies device. |
| `0x12` | 1    | `STATUS` | R/W | Virtio device status (see §1.5). |
| `0x13` | 1    | `ISR` | R | Interrupt status; read-to-ack (see §1.4). |
| `0x14` | var  | `DEVICE_CONFIG` | R/W* | Device-specific config space (see per-device sections). |

\* Device config is **mostly read-only** in contract v1. Any writeable fields are explicitly called out per device.

### 1.4 Interrupt mechanism: INTx + ISR semantics

Contract v1 uses **PCI INTx** interrupts only.

- The device MUST use PCI **INTA#** (interrupt pin = 1).
- The device MUST assert INTx when it has a pending interrupt reason and the guest has not yet acknowledged it.

#### 1.4.1 ISR register (`0x13`)

`ISR` is an 8-bit register with **read-to-ack** semantics:

- Reading `ISR` returns a bitmask of pending interrupt causes.
- Reading `ISR` MUST clear all currently returned bits (acknowledge).
- If no causes are pending, reading returns `0x00`.

Bit assignments:

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | `QUEUE_INTERRUPT` | Device added entries to a used ring for at least one queue. |
| 1   | `CONFIG_INTERRUPT` | Device-specific config change. |
| 2-7 | - | Reserved; MUST be 0. |

The device MUST deassert INTx after the guest acknowledges all pending causes (i.e., after `ISR` is read and no further causes remain).

### 1.5 Reset and status state machine

#### 1.5.1 Status bits

`STATUS` is an 8-bit bitmask. The guest driver MUST use the standard virtio initialization flow:

| Bit | Name | Meaning |
|-----|------|---------|
| 0   | `ACKNOWLEDGE` | Guest has noticed the device. |
| 1   | `DRIVER` | Guest knows how to drive the device. |
| 2   | `DRIVER_OK` | Driver is fully initialized. |
| 3   | `FEATURES_OK` | Driver accepted feature set. |
| 6   | `DEVICE_NEEDS_RESET` | Device has encountered an error and needs reset. |
| 7   | `FAILED` | Driver has given up on the device. |

Bits not listed MUST be treated as reserved and written as 0 by drivers.

#### 1.5.2 Reset

Writing `0x00` to `STATUS` MUST reset the device:

- All virtqueues become inactive (`QUEUE_PFN` reads as 0 for all queues).
- Interrupt state is cleared (INTx deasserted; `ISR` reads as 0).
- Device returns to the pre-initialization state.
- Device-specific config is reset to power-on defaults (usually static).

The device MUST tolerate reset at any time.

### 1.6 Feature negotiation (32-bit)

Because contract v1 uses the legacy transport, feature negotiation is limited to **bits 0–31** via `HOST_FEATURES` / `GUEST_FEATURES`.

Rules:

1. Driver MUST read `HOST_FEATURES`, mask to supported features, then write `GUEST_FEATURES`.
2. Driver MUST set `FEATURES_OK` in `STATUS` and read back `STATUS` to confirm the bit remains set.
3. The device MUST clear `FEATURES_OK` if the driver requested unsupported features.

Contract v1 never offers features requiring >32-bit feature negotiation (e.g., `VIRTIO_F_VERSION_1`).

## 2. Virtqueue contract (split ring only)

### 2.1 Queue selection and activation (legacy PFN model)

- The driver selects a queue by writing its index to `QUEUE_SEL`.
- The driver reads `QUEUE_NUM` to obtain the queue size **N** (number of descriptors).
- The driver allocates a physically contiguous ring region for the queue (see §2.2).
- The driver writes the ring base PFN (`physical_address >> 12`) to `QUEUE_PFN`.

The queue is considered **active** when `QUEUE_PFN != 0` for that queue.

### 2.2 Split ring layout, sizes, and alignment

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

#### 2.2.1 Alignment requirements

- Queue base address (the address written via `QUEUE_PFN`) MUST be **4 KiB aligned**.
- `vring_desc` table starts at offset 0.
- `vring_avail` MUST be aligned to **2 bytes** (naturally satisfied by the descriptor table size).
- `vring_used` MUST be aligned to **4 bytes**; i.e. `vring_used` starts at `align_up(avail_end, 4)`.

#### 2.2.2 Size calculations (EVENT_IDX not supported)

Given queue size **N**:

- `desc_bytes = 16 * N`
- `avail_bytes = 4 + 2 * N`
- `used_bytes = 4 + 8 * N`
- `used_offset = align_up(desc_bytes + avail_bytes, 4)`
- `total_bytes = used_offset + used_bytes`

The driver MUST allocate at least `total_bytes` bytes of physically contiguous memory per queue.

### 2.3 Descriptor flags and chaining

Supported descriptor flags:

| Flag | Value | Meaning | Support |
|------|-------|---------|---------|
| `NEXT` | `1` | This descriptor continues at `next`. | **MUST** |
| `WRITE` | `2` | Device writes into the buffer. | **MUST** |
| `INDIRECT` | `4` | Descriptor points to an indirect table. | **MUST** if feature negotiated (see below) |

#### 2.3.1 Indirect descriptors

Contract v1 supports indirect descriptors via `VIRTIO_RING_F_INDIRECT_DESC` (bit 28).

- If the feature is negotiated, the driver MAY submit a descriptor with `INDIRECT` set.
- For an indirect descriptor:
  - `addr` points to a guest-physical contiguous array of `vring_desc`.
  - `len` is the size in bytes of that array and MUST be a multiple of 16.
  - The indirect table entries follow the same rules for flags/chaining.

If the feature is not negotiated, the device MUST treat `INDIRECT` as an error for that request and complete it with a device-specific failure status (see per-device sections).

### 2.4 Notification and interrupt suppression

#### 2.4.1 Driver -> device notifications

Contract v1 uses **always-notify** semantics (no EVENT_IDX):

- After making one or more new available entries visible, the driver MUST notify the device by writing the queue index to `QUEUE_NOTIFY`.
- The device MUST treat any write to `QUEUE_NOTIFY` as a doorbell and schedule processing for that queue.

The device MAY coalesce doorbells; drivers MUST tolerate spurious wakeups (i.e., device may process even if no new work is found).

#### 2.4.2 Device -> driver interrupts

The device MUST raise an interrupt when it adds at least one entry to any used ring, **unless** the driver has requested suppression:

- If `vring_avail.flags & 0x0001` (the traditional `VRING_AVAIL_F_NO_INTERRUPT`) is set at the time the device is about to interrupt, the device SHOULD suppress the interrupt.

Even when suppressed, the device MUST still update the used ring correctly; the driver will poll.

> Note: Contract v1 does not require the device to implement `VRING_USED_F_NO_NOTIFY` (driver suppression of doorbells), because the contract uses always-notify semantics. Drivers MAY set it, but devices MAY ignore it.

### 2.5 Guest physical memory access (DMA model)

The device performs DMA by reading/writing guest physical memory pointed to by descriptor addresses.

Rules:

- Descriptor `addr` fields are **guest physical addresses**.
- The device MUST bounds-check each `(addr, len)` before access:
  - If any range is outside guest RAM, the device MUST treat the request as failed (device-specific status) and MUST NOT read or write outside guest RAM.
- The device MUST support reading and writing buffers that are not aligned to any particular boundary.
- The device MUST support scatter/gather by following descriptor chains.

### 2.6 DMA addresses >4 GiB

The split ring descriptor `addr` is 64-bit. Contract v1 requires 64-bit DMA correctness:

- The device MUST accept and correctly access guest physical addresses above 4 GiB **if** the guest physical address space includes RAM above 4 GiB.
- If the guest provides an address above the implemented guest physical map, the device MUST fail the request cleanly (no out-of-bounds accesses).

Drivers MUST NOT assume buffers are below 4 GiB on x64 Windows 7.

### 2.7 Ordering model (what is guaranteed)

To avoid driver/device divergence, contract v1 defines a strict ordering model:

- The device MUST process available descriptors in monotonically increasing `avail.idx` order.
- The device MUST complete used entries in the same order it processes them for a given queue.

This is stricter than generic virtio (which permits some reordering) and is intended to simplify Windows 7 driver correctness.

## 3. Per-device contracts

All devices inherit the transport and virtqueue rules above.

### 3.1 virtio-blk (block)

#### 3.1.1 PCI IDs

- Vendor ID: `0x1AF4`
- Device ID: `0x1001`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0002` (Aero virtio-blk)
- Revision ID: `0x01`

#### 3.1.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `requestq` | driver ↔ device | **128** |

#### 3.1.3 Feature bits

The device MUST offer the following bits in `HOST_FEATURES`:

- `VIRTIO_RING_F_INDIRECT_DESC` (28) = 1
- `VIRTIO_BLK_F_SEG_MAX` (2) = 1
- `VIRTIO_BLK_F_BLK_SIZE` (6) = 1
- `VIRTIO_BLK_F_FLUSH` (9) = 1

The device MUST NOT offer:

- `VIRTIO_RING_F_EVENT_IDX` (29)
- Read-only (`VIRTIO_BLK_F_RO`)
- Discard / write-zeroes / multi-queue features

The driver MUST accept (set) all offered feature bits listed above.

#### 3.1.4 Device config layout (BAR0 + `0x14`)

virtio-blk config follows the standard virtio-blk layout (little-endian):

| Offset (from `DEVICE_CONFIG`) | Size | Field | Notes |
|------------------------------|------|-------|------|
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
  le32 type;     // request type
  le32 ioprio;   // ignored by Aero; driver MUST set to 0
  le64 sector;   // starting sector (512-byte units)
};
```

2. **Data buffers** (0 or more descriptors):
   - For `IN` (read): buffers MUST be device-writable (`WRITE` flag set).
   - For `OUT` (write): buffers MUST be device-readable (`WRITE` flag clear).
3. **Status byte** (1 byte), device-writable.

Supported `type` values:

| Name | Value | Support |
|------|-------|---------|
| `VIRTIO_BLK_T_IN` | 0 | **MUST** |
| `VIRTIO_BLK_T_OUT` | 1 | **MUST** |
| `VIRTIO_BLK_T_FLUSH` | 4 | **MUST** |

All other request types MUST complete with status `VIRTIO_BLK_S_UNSUPP`.

Status byte values:

| Name | Value | Meaning |
|------|-------|---------|
| `VIRTIO_BLK_S_OK` | 0 | Success. |
| `VIRTIO_BLK_S_IOERR` | 1 | I/O error (including invalid ranges). |
| `VIRTIO_BLK_S_UNSUPP` | 2 | Unsupported request type. |

#### 3.1.6 I/O semantics

For `IN`/`OUT`:

- Total transfer length = sum of data buffer descriptor lengths.
- Transfer length MUST be a multiple of 512 bytes; otherwise the device MUST return `IOERR`.
- `(sector * 512 + length)` MUST be within the underlying virtual disk size; otherwise `IOERR`.
- The device MUST complete a request by writing the status byte *before* placing the used-ring entry.

For `FLUSH`:

- The device MUST ensure all prior completed `OUT` writes are durable in the backing store before completing the flush.
- If the backing store cannot guarantee durability, the device MUST still complete flush successfully but MUST document the limitation in the emulator implementation notes (not here). (Drivers treat flush as a correctness boundary.)

### 3.2 virtio-net (network)

#### 3.2.1 PCI IDs

- Vendor ID: `0x1AF4`
- Device ID: `0x1000`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0001` (Aero virtio-net)
- Revision ID: `0x01`

#### 3.2.2 Virtqueues

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `rxq` | device → driver | **256** |
| 1 | `txq` | driver → device | **256** |

No control queue is implemented in contract v1.

#### 3.2.3 Feature bits

The device MUST offer:

- `VIRTIO_RING_F_INDIRECT_DESC` (28) = 1
- `VIRTIO_NET_F_MAC` (5) = 1 (MAC address in config)
- `VIRTIO_NET_F_STATUS` (16) = 1 (link status in config)

The device MUST NOT offer:

- `VIRTIO_NET_F_MRG_RXBUF` (15)
- Any checksum/GSO/TSO offload features (driver MUST assume none)
- Control virtqueue (`VIRTIO_NET_F_CTRL_VQ`)
- Multi-queue, RSS, VLAN, MTU config, etc.

The driver MUST accept all offered bits listed above.

#### 3.2.4 Device config layout (BAR0 + `0x14`)

virtio-net config (contract v1):

| Offset | Size | Field | Notes |
|--------|------|-------|------|
| `0x00` | 6 | `mac` | MAC address. Read-only. |
| `0x06` | 2 | `status` | Link status. Bit0 = `VIRTIO_NET_S_LINK_UP`. Read-only. |

The device MUST report link-up (`status & 1 == 1`) after `DRIVER_OK` and SHOULD keep it up for the lifetime of the VM.

#### 3.2.5 Packet format and virtio-net header expectations

Contract v1 uses the classic `virtio_net_hdr` (10 bytes) and does **not** use the “merged RX buffer” header variant.

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

Because no offload features are negotiated:

- The driver MUST set all header fields to 0 for TX.
- The device MUST ignore the header contents for TX and MUST NOT perform offloads.
- For RX, the device MUST write a zeroed header.

#### 3.2.6 TX (driver → device)

Each TX packet submission is a descriptor chain:

1. Descriptor 0: device-readable `virtio_net_hdr` (len >= 10)
2. Descriptor 1..k: device-readable payload bytes (an entire Ethernet frame, no FCS)

The device MUST complete the chain by placing a used entry with:

- `id = head descriptor index`
- `len = total bytes consumed` (including the 10-byte header and payload)

Oversized/undersized behavior:

- Minimum payload length: **14 bytes** (Ethernet header). If shorter, device MUST drop the frame but still complete the descriptor chain successfully.
- Maximum payload length: **1514 bytes** (Ethernet header + 1500 MTU payload). If longer, device MUST drop the frame but still complete the chain successfully.

#### 3.2.7 RX (device → driver)

The driver supplies receive buffers via available descriptor chains.

Buffer requirements (driver):

- Each chain MUST start with a writable buffer of at least 10 bytes for `virtio_net_hdr`.
- The chain MUST provide at least **1524 bytes** of writable payload space total (to hold up to 1514-byte frames plus the 10-byte header), or packets may be dropped.

Receive behavior (device):

- For each received Ethernet frame, the device consumes exactly one available chain.
- The device writes:
  - a zeroed 10-byte header into the first buffer, and
  - the full Ethernet frame into subsequent buffer space.
- The device completes the chain with `used.len = 10 + frame_len`.
- If the provided buffers are insufficient, the device MUST drop the incoming frame and MUST NOT consume a chain for it.

### 3.3 virtio-input (keyboard/mouse)

Contract v1 uses **two separate virtio-input devices**:

- One keyboard device
- One mouse device

Both share the same Vendor/Device ID (`0x1AF4:0x1011`) and are distinguished by subsystem IDs and config strings.

#### 3.3.1 PCI IDs

Keyboard:

- Vendor ID: `0x1AF4`
- Device ID: `0x1011`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0010` (Aero virtio-input keyboard)
- Revision ID: `0x01`

Mouse:

- Vendor ID: `0x1AF4`
- Device ID: `0x1011`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0011` (Aero virtio-input mouse)
- Revision ID: `0x01`

#### 3.3.2 Virtqueues

virtio-input defines two queues:

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `eventq` | device → driver | **64** |
| 1 | `statusq` | driver → device | **64** |

#### 3.3.3 Feature bits

The device MUST offer:

- `VIRTIO_RING_F_INDIRECT_DESC` (28) = 1

No device-specific feature bits are required by contract v1.

#### 3.3.4 Device config: discovery model

virtio-input uses a “selector” config scheme. The guest writes `select`/`subsel`, then reads `size` and the associated payload.

Config header (little-endian where applicable; most are bytes):

```c
struct virtio_input_config {
  u8 select;
  u8 subsel;
  u8 size;
  u8 reserved[5];
  u8 payload[128]; // meaning depends on (select, subsel)
};
```

Contract v1 requires the following selectors to be implemented:

- `VIRTIO_INPUT_CFG_ID_NAME` → returns a NUL-terminated UTF-8 device name string
- `VIRTIO_INPUT_CFG_ID_DEVIDS` → returns `virtio_input_devids`
- `VIRTIO_INPUT_CFG_EV_BITS` → returns event-type bitmaps for supported event types

All other selectors MUST return `size = 0`.

`virtio_input_devids` (payload for `ID_DEVIDS`):

```c
struct virtio_input_devids {
  le16 bustype;
  le16 vendor;
  le16 product;
  le16 version;
};
```

Required values:

- `bustype = 0x0006` (BUS_VIRTUAL)
- `vendor = 0x1AF4`
- `product`:
  - keyboard: `0x0001`
  - mouse: `0x0002`
- `version = 0x0001`

Required `ID_NAME` strings:

- keyboard: `"Aero Virtio Keyboard"`
- mouse: `"Aero Virtio Mouse"`

#### 3.3.5 Event format and semantics

Events are delivered as `virtio_input_event` records (8 bytes):

```c
struct virtio_input_event {
  le16 type;
  le16 code;
  le32 value; // signed
};
```

The device MUST send events using Linux input event types/codes (as in `input-event-codes.h`).

##### Keyboard events

- `type = EV_KEY` for key press/release
- `value = 1` press, `0` release
- The device SHOULD NOT send auto-repeat (`value = 2`); Windows handles repeat policy.
- The device MUST emit `EV_SYN / SYN_REPORT` after a batch of related events.

Minimum required supported key codes (device MUST advertise them via `EV_BITS` and may support more):

- `KEY_A`..`KEY_Z`
- `KEY_0`..`KEY_9`
- `KEY_ENTER`, `KEY_ESC`, `KEY_BACKSPACE`, `KEY_TAB`, `KEY_SPACE`
- `KEY_LEFTSHIFT`, `KEY_RIGHTSHIFT`, `KEY_LEFTCTRL`, `KEY_RIGHTCTRL`, `KEY_LEFTALT`, `KEY_RIGHTALT`
- `KEY_CAPSLOCK`
- `KEY_F1`..`KEY_F12`
- `KEY_UP`, `KEY_DOWN`, `KEY_LEFT`, `KEY_RIGHT`
- `KEY_INSERT`, `KEY_DELETE`, `KEY_HOME`, `KEY_END`, `KEY_PAGEUP`, `KEY_PAGEDOWN`

##### Mouse events (relative)

- Relative motion:
  - `type = EV_REL`, `code = REL_X` and `REL_Y`
  - `value` is a signed delta in “counts” (implementation-defined scaling); drivers MUST treat it as relative motion.
- Wheel:
  - `type = EV_REL`, `code = REL_WHEEL`
  - `value` is positive/negative tick count (typically ±1)
- Buttons:
  - `type = EV_KEY`, `code = BTN_LEFT / BTN_RIGHT / BTN_MIDDLE`
  - `value = 1` press, `0` release
- The device MUST emit `EV_SYN / SYN_REPORT` after a batch of related events.

##### statusq behavior

The guest MAY send output events (e.g., LED state changes) via `statusq`.

- The device MUST consume and complete all `statusq` descriptors.
- The device MAY ignore the contents (LEDs need not be modeled in contract v1).

### 3.4 virtio-snd (audio)

Contract v1 defines a **minimal** virtio-snd device sufficient for Windows 7 audio output.

#### 3.4.1 PCI IDs

- Vendor ID: `0x1AF4`
- Device ID: `0x1018`
- Subsystem Vendor ID: `0x1AF4`
- Subsystem Device ID: `0x0020` (Aero virtio-snd)
- Revision ID: `0x01`

#### 3.4.2 Virtqueues

virtio-snd requires four queues:

| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `controlq` | driver → device (request/response) | **64** |
| 1 | `eventq` | device → driver (async events) | **64** |
| 2 | `txq` | driver → device (PCM playback buffers) | **256** |
| 3 | `rxq` | device → driver (PCM capture buffers) | **64** (unused in v1) |

#### 3.4.3 Feature bits

The device MUST offer:

- `VIRTIO_RING_F_INDIRECT_DESC` (28) = 1

No optional virtio-snd feature bits are required in contract v1 (device_features other than the above MUST be 0).

#### 3.4.4 Device config layout (BAR0 + `0x14`)

virtio-snd config (standard layout):

| Offset | Size | Field | Value (contract v1) |
|--------|------|-------|---------------------|
| `0x00` | 4 | `jacks` | `0` |
| `0x04` | 4 | `streams` | `1` |
| `0x08` | 4 | `chmaps` | `0` |

Meaning:

- No jack discovery is required.
- Exactly one PCM stream is exposed: **stream ID 0**.
- No channel map discovery is required.

#### 3.4.5 Minimal PCM capability

The single PCM stream (ID 0) is **playback-only** (output).

The device MUST support the following PCM parameters:

- Channels: **2** (stereo)
- Sample format: **signed 16-bit little-endian** (S16_LE)
- Sample rate: **48,000 Hz**
- Interleaved samples: `[L0, R0, L1, R1, ...]`

No other formats/rates are required in contract v1.

#### 3.4.6 Control flow and required commands

Drivers and devices MUST follow the virtio-snd control model as defined by the virtio-snd specification, with the following minimum supported request types:

- `PCM_INFO` (query stream capabilities)
- `PCM_SET_PARAMS` (configure the stream)
- `PCM_PREPARE`
- `PCM_START`
- `PCM_STOP`
- `PCM_RELEASE`

All other requests MUST return `NOT_SUPP`.

The device MUST accept only parameters matching §3.4.5; mismatched params MUST return `NOT_SUPP`.

#### 3.4.7 Playback data path (`txq`)

After `PCM_START`, the driver submits playback buffers on `txq` using the standard virtio-snd PCM transfer message format (request header + data + status), as defined by the virtio-snd specification.

Contract v1 requires:

- The device MUST play PCM buffers in the order submitted on `txq`.
- The device MUST complete each buffer with an OK status once fully consumed.
- On underrun (not enough buffers), the device MUST output silence and continue.

`eventq` is present but no events are required in contract v1; it MAY remain idle.

## 4. Compatibility rules and invariants

### 4.1 “No guessing” invariants

These invariants are intentionally strict so implementers do not need to infer QEMU behavior:

1. Only **virtio-pci legacy** I/O port transport is used.
2. Only **split rings** are used.
3. Queue sizes are fixed per device in this contract.
4. Interrupts are **INTx** with virtio ISR semantics.
5. Each device has an explicit, minimal feature set.

### 4.2 Error handling

If the device detects a malformed descriptor chain (invalid addresses, invalid lengths, unsupported flags when not negotiated):

- For request/response devices (blk/snd control), the device MUST complete the request with the device-specific error status (`IOERR` / `NOT_SUPP` / `BAD_MSG` as appropriate).
- The device MUST NOT crash or access out-of-bounds guest memory.
- The device MAY set `DEVICE_NEEDS_RESET` if it cannot continue safely.

Drivers MUST treat `DEVICE_NEEDS_RESET` as fatal and reinitialize from reset.

## 5. Versioning and backwards compatibility

### 5.1 Contract version encoding

Contract major versions are encoded as:

- **PCI Revision ID** (`0x08` in PCI config header): `0x01` for contract v1.

Drivers MUST refuse to bind to devices with an unknown major version (Revision ID not recognized), unless explicitly built to support it.

### 5.2 Backwards compatibility policy

- **Major version bump (X.0 → X+1.0):** breaking changes allowed (register layout, queue counts, incompatible semantics).
- **Minor version bump (X.Y → X.Y+1):** only additive changes allowed:
  - new optional feature bits
  - new optional device config fields at the end of a config struct (never reordering existing fields)
  - new optional commands/requests that return `NOT_SUPP` when unimplemented

### 5.3 Feature-bit based extensibility

All new functionality MUST be gated behind:

- a virtio feature bit, and/or
- a new queue that is only enabled when a feature bit is negotiated.

Drivers MUST ignore unknown feature bits (do not set them in `GUEST_FEATURES`).

## 6. Conformance checklist

### 6.1 Emulator/device-model checklist

- [ ] Expose PCI IDs exactly as specified (§1.1, §3).
- [ ] Implement BAR0 I/O register map exactly (§1.3).
- [ ] Implement INTx + ISR read-to-ack semantics (§1.4).
- [ ] Implement reset semantics on `STATUS=0` (§1.5.2).
- [ ] Implement split rings with required alignments and sizes (§2.2).
- [ ] Implement indirect descriptors when negotiated (§2.3.1).
- [ ] Bounds-check all guest physical memory accesses (§2.5).
- [ ] Implement per-device queue sizes, feature bits, and config layouts (§3).

### 6.2 Windows 7 driver checklist

- [ ] Bind by Vendor/Device IDs; verify PCI Revision ID (`0x01`) (§5.1).
- [ ] Use virtio legacy BAR0 I/O registers only (§1.3).
- [ ] Negotiate only defined feature bits; set `FEATURES_OK` (§1.6).
- [ ] Allocate split rings with correct alignment/size (§2.2).
- [ ] Always notify via `QUEUE_NOTIFY` (§2.4.1).
- [ ] Tolerate INTx interrupts and ISR read-to-ack (§1.4).
- [ ] Treat unknown config fields as zero; do not assume QEMU-only behaviors.
