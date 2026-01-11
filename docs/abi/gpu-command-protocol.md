# AeroGPU Command Protocol (toy/prototype, v0.1)

This document specifies a **toy/prototype** guest ↔ host command ABI (commands like
`CREATE_SURFACE`/`PRESENT`) that was used during early bring-up.

The in-tree implementation previously lived under `crates/aero-emulator`, but it has since
been removed in favor of the canonical Win7/WDDM path.

It also used stale placeholder PCI IDs (deprecated vendor `VEN_1AE0`) and must not be used as a
Windows driver contract (see `docs/abi/aerogpu-pci-identity.md`).

It is **not** the Windows 7 / WDDM AeroGPU protocol. For the Win7/WDDM target ABI, see
`drivers/aerogpu/protocol/*` and `docs/graphics/aerogpu-protocols.md`.

## 1. Wire rules

### Endianness

All multi-byte fields are **little-endian**.

### Alignment

- All command and event entries in rings are **8-byte aligned**.
- `size_bytes` for ring entries **must be a multiple of 8**.
- Ring buffer size must be a multiple of 8.

Rationale: commands frequently contain 64-bit guest physical addresses; 8-byte alignment avoids straddling across cache lines and simplifies wrap handling.

### Versioning

The ABI is versioned as `{major, minor, patch}`:

- **Major** increments on incompatible layout/semantic changes.
- **Minor** increments when new opcodes/capabilities are added in a backwards-compatible way.
- **Patch** increments for bugfixes with no ABI changes.

The device exposes a packed version in `MMIO.VERSION`:

```
VERSION = (major << 16) | (minor << 8) | patch
```

Guests should:

1) Read `VERSION`
2) Check `major` is supported
3) Use `minor`/`CAPS` to enable optional features

## 2. Shared memory layout

The device provides a shared-memory region (e.g. a PCI BAR, or a fixed MMIO mapping) that contains:

- A **capabilities struct**
- A **command ring** (guest → host)
- An optional **event ring** (host → guest)

### 2.1 Capabilities

The host exposes a capability bitmask (`CAPS` register) and a shared `Caps` struct. The current minimal capabilities are:

- `CAPS_EVENT_RING` (bit 0): event ring is present
- `CAPS_FORMAT_RGBA8888` (bit 1): `SurfaceFormat::Rgba8888` is supported

#### `Caps` struct layout

The shared capabilities struct is a fixed-size, little-endian blob:

```c
// size: 16 bytes
struct Caps {
  u32 caps_bits;
  u32 max_surface_width;
  u32 max_surface_height;
  u32 max_surfaces;
}
```

### 2.2 Rings

Rings are single-producer/single-consumer and use atomic indices in shared memory:

- `head`: read position (advanced by consumer)
- `tail`: committed write position (advanced by producer *after* writing full entries)

Indices are measured in **bytes**, are **monotonic modulo 2³²**, and are interpreted modulo `ring_size_bytes` when indexing into the ring data array.

#### Ring control layout (conceptual)

```c
struct RingControl {
  atomic_u32 head; // bytes
  atomic_u32 tail; // bytes
  u32 ring_size_bytes;
  u32 reserved;
  u8  ring_data[ring_size_bytes];
}
```

#### Wrap-around handling

Ring entries are required to be **contiguous** within the ring’s byte array. If a producer cannot fit a command at the end of the ring, it must:

1) Emit a **NOP** padding entry that consumes the remaining bytes to the end of the ring
2) Continue writing the next entry at offset 0

The consumer skips NOP padding entries.

#### Backpressure (full ring)

Producer must not advance `tail` if there isn’t enough free space (`ring_size_bytes - used_bytes`) to write the entry (and any required wrap padding). If full, the producer must retry later.

## 3. MMIO register block

The device exposes a small MMIO register block (offsets in bytes):

| Offset | Name | R/W | Description |
|---:|---|:--:|---|
| 0x00 | `VERSION` | R | Packed ABI version |
| 0x04 | `CAPS` | R | Capability bitmask |
| 0x08 | `CMD_RING_DOORBELL` | W | Rings the doorbell to wake the host GPU worker |
| 0x0C | `IRQ_STATUS` | R | Interrupt status bits |
| 0x10 | `IRQ_ACK` | W | Write-1-to-clear interrupt bits |
| 0x14 | `CMD_RING_HEAD` | R | Debug: current command ring head (mod ring size) |
| 0x18 | `CMD_RING_TAIL` | R | Debug: current command ring tail (mod ring size) |
| 0x1C | `RESET` | W | Device reset (clears rings/resources) |

### Interrupt bits (`IRQ_STATUS`)

- bit 0: `CMD_PROCESSED` – some command(s) have been processed since last ACK
- bit 1: `PRESENT_DONE` – a `PRESENT` has completed

## 4. Ring entry formats

### 4.1 Command header

All commands begin with:

```c
struct CmdHeader {
  u32 opcode;
  u32 size_bytes; // total entry size including this header
}
```

### 4.2 Event header

All events begin with:

```c
struct EventHeader {
  u32 event_type;
  u32 size_bytes; // total entry size including this header
}
```

## 5. Minimal opcodes (smoke testing)

All opcodes below are **required** for the v0.1 smoke tests.

Unless otherwise specified, all fields are `u32` little-endian.

### 5.1 `CREATE_SURFACE` (opcode = 1)

Creates a new host-side surface and returns its ID via an event.

Payload:

```
u32 width
u32 height
u32 format
```

### 5.2 `UPDATE_SURFACE` (opcode = 2)

Updates a surface’s pixel contents by copying from guest physical memory.

Payload:

```
u32 surface_id
u64 guest_phys_addr
u32 stride_bytes
```

### 5.3 `CLEAR_RGBA` (opcode = 3)

Clears the entire surface to a constant color.

Payload:

```
u32 surface_id
u32 rgba // packed r | g<<8 | b<<16 | a<<24
```

### 5.4 `DRAW_TRIANGLE_TEST` (opcode = 4)

Diagnostic opcode: draws a fixed red triangle into the surface (software reference implementation for now).

Payload:

```
u32 surface_id
```

### 5.5 `PRESENT` (opcode = 5)

Presents a surface to the device’s front buffer and triggers `PRESENT_DONE`.

Payload:

```
u32 surface_id
```

## 6. Error reporting

The primary error-reporting channel is the **event ring**.

For each processed command, the host emits an `EVENT_CMD_STATUS` entry:

```
event_type = 1 (CMD_STATUS)
payload:
  u32 opcode
  u32 status
  u32 data0
  u32 data1
  u32 data2
  u32 data3
```

`status` is one of:

- `0 OK`
- `1 INVALID_OPCODE`
- `2 INVALID_SIZE`
- `3 INVALID_ARGUMENT`
- `4 SURFACE_NOT_FOUND`
- `5 UNSUPPORTED_FORMAT`
- `6 GUEST_MEMORY_FAULT`
- `7 OUT_OF_MEMORY`

Unknown opcodes must not crash the host; they must return `INVALID_OPCODE` and continue processing subsequent commands.
