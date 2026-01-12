# 16 - AeroGPU Guest↔Emulator Protocol (PCI/MMIO + Submission Ring + Command Stream)

This document describes the **canonical AeroGPU ABI** used by the Windows 7 AeroGPU WDDM
driver stack and the Aero emulator’s virtual GPU device model.
> Note: The repository also contains a deprecated legacy bring-up AeroGPU ABI
> (`drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`, legacy `"ARGP"` device model).
> The Win7 KMD supports both legacy and versioned devices (auto-detected via BAR0 MMIO magic),
> but the emulator's legacy device model is feature-gated (`emulator/aerogpu-legacy`) and the
> shipped Win7 INFs intentionally bind only to the canonical, versioned device (`A3A0:0001`,
> MMIO magic `"AGPU"`). This document describes only the **versioned** ABI
> (`aerogpu_pci.h`/`aerogpu_ring.h`/`aerogpu_cmd.h`). See `docs/abi/aerogpu-pci-identity.md`
> for the canonical mapping.

## Current status (canonical machine)

This document describes the **AeroGPU** ABI for the `A3A0:0001` PCI device model. The canonical
full-system machine (`aero_machine::Machine`) reserves `00:07.0` for that identity, but does **not**
yet expose the full AeroGPU WDDM PCI device function.

Today, boot display in the canonical machine is provided by `aero_gpu_vga` (VGA + Bochs VBE), plus
a minimal Bochs/QEMU “Standard VGA”-like PCI stub at `00:0c.0` (`1234:1111`) used only for VBE LFB
MMIO routing.

See:

- [`docs/abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md)
- [`docs/16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md)
- [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md)

## Normative source-of-truth (and generated mirrors)

The normative, versioned ABI is the C headers under [`drivers/aerogpu/protocol/`](../drivers/aerogpu/protocol/):

- [`drivers/aerogpu/protocol/README.md`](../drivers/aerogpu/protocol/README.md) – high-level overview and versioning rules
- [`drivers/aerogpu/protocol/aerogpu_pci.h`](../drivers/aerogpu/protocol/aerogpu_pci.h) – PCI IDs, BAR0 size, MMIO register map, shared enums
- [`drivers/aerogpu/protocol/aerogpu_ring.h`](../drivers/aerogpu/protocol/aerogpu_ring.h) – submission ring, submit descriptor, allocation table, fence page
- [`drivers/aerogpu/protocol/aerogpu_cmd.h`](../drivers/aerogpu/protocol/aerogpu_cmd.h) – command stream packet formats (“AeroGPU IR”)
- [`drivers/aerogpu/protocol/aerogpu_escape.h`](../drivers/aerogpu/protocol/aerogpu_escape.h) – stable Escape packet header + base ops (`DxgkDdiEscape` / `D3DKMTEscape`)

Host-side mirrors are provided for parsing/validation:

- [`emulator/protocol/aerogpu/*.rs`](../emulator/protocol/aerogpu/)
- [`emulator/protocol/aerogpu/*.ts`](../emulator/protocol/aerogpu/)

Related repository docs:

- [`docs/abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md) – mapping of PCI IDs ↔ ABI generations
- [`docs/graphics/aerogpu-protocols.md`](./graphics/aerogpu-protocols.md) – overview of similarly named protocols

Implementation reference (emulator): `crates/emulator/src/devices/pci/aerogpu.rs`.

If this document disagrees with the headers, **the headers win**.

---

## 1. PCI identity

Defined in `aerogpu_pci.h`:

- Vendor ID: `0xA3A0` (`AEROGPU_PCI_VENDOR_ID`)
- Device ID: `0x0001` (`AEROGPU_PCI_DEVICE_ID`)
- Subsystem vendor ID: `0xA3A0` (`AEROGPU_PCI_SUBSYSTEM_VENDOR_ID`)
- Subsystem ID: `0x0001` (`AEROGPU_PCI_SUBSYSTEM_ID`)
- Class code: `0x03` (Display controller)
- Subclass: `0x00` (VGA compatible controller)
- Programming interface: `0x00`

BARs:

- BAR0: MMIO register block, **64 KiB** (`AEROGPU_PCI_BAR0_SIZE_BYTES`)

---

## 2. BAR0 / MMIO register map

Rules (from `aerogpu_pci.h`):

- All registers are **little-endian**.
- MMIO registers are **32-bit** wide unless documented otherwise.
- 64-bit values are split into consecutive `*_LO` / `*_HI` 32-bit halves.

### 2.1 Discovery / feature registers

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0000` | `AEROGPU_MMIO_REG_MAGIC` | RO | Must read as `AEROGPU_MMIO_MAGIC` (`0x55504741`, `"AGPU"` LE) |
| `0x0004` | `AEROGPU_MMIO_REG_ABI_VERSION` | RO | `AEROGPU_ABI_VERSION_U32` (`(ABI_MAJOR<<16) \| ABI_MINOR`) |
| `0x0008` | `AEROGPU_MMIO_REG_FEATURES_LO` | RO | Low 32 bits of the 64-bit feature mask |
| `0x000C` | `AEROGPU_MMIO_REG_FEATURES_HI` | RO | High 32 bits of the 64-bit feature mask |

Feature bits (`FEATURES_LO/HI` combined as a 64-bit value):

- `AEROGPU_FEATURE_FENCE_PAGE` (bit 0): shared fence page is supported
- `AEROGPU_FEATURE_CURSOR` (bit 1): cursor registers are implemented
- `AEROGPU_FEATURE_SCANOUT` (bit 2): scanout registers are implemented
- `AEROGPU_FEATURE_VBLANK` (bit 3): vblank IRQ + vblank timing registers are implemented (see [`vblank.md`](../drivers/aerogpu/protocol/vblank.md))
- `AEROGPU_FEATURE_TRANSFER` (bit 4): transfer/copy commands are supported (e.g. `COPY_BUFFER`, `COPY_TEXTURE2D`) and may optionally require host→guest writeback for destination resources (ABI 1.1+)

### 2.2 Ring programming + doorbell

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0100` | `AEROGPU_MMIO_REG_RING_GPA_LO` | RW | Guest physical address of the `aerogpu_ring_header` (low 32 bits) |
| `0x0104` | `AEROGPU_MMIO_REG_RING_GPA_HI` | RW | Guest physical address of the `aerogpu_ring_header` (high 32 bits) |
| `0x0108` | `AEROGPU_MMIO_REG_RING_SIZE_BYTES` | RW | Total bytes mapped at `RING_GPA` (must be **>=** `aerogpu_ring_header.size_bytes`) |
| `0x010C` | `AEROGPU_MMIO_REG_RING_CONTROL` | RW | Ring control bits |
| `0x0200` | `AEROGPU_MMIO_REG_DOORBELL` | WO | Write any value to notify the device that new submissions are available |

`AEROGPU_MMIO_REG_RING_CONTROL` bits:

- `AEROGPU_RING_CONTROL_ENABLE` (bit 0): driver sets to 1 after ring init/programming
- `AEROGPU_RING_CONTROL_RESET` (bit 1): write 1 to request a ring reset

### 2.3 Fence / completion registers

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0120` | `AEROGPU_MMIO_REG_FENCE_GPA_LO` | RW | GPA of `aerogpu_fence_page` (low 32 bits), if `AEROGPU_FEATURE_FENCE_PAGE` |
| `0x0124` | `AEROGPU_MMIO_REG_FENCE_GPA_HI` | RW | GPA of `aerogpu_fence_page` (high 32 bits) |
| `0x0130` | `AEROGPU_MMIO_REG_COMPLETED_FENCE_LO` | RO | Completed fence value (low 32 bits) |
| `0x0134` | `AEROGPU_MMIO_REG_COMPLETED_FENCE_HI` | RO | Completed fence value (high 32 bits) |

### 2.4 Interrupt registers

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0300` | `AEROGPU_MMIO_REG_IRQ_STATUS` | RO | Pending IRQ cause bits |
| `0x0304` | `AEROGPU_MMIO_REG_IRQ_ENABLE` | RW | IRQ enable mask |
| `0x0308` | `AEROGPU_MMIO_REG_IRQ_ACK` | WO | Write-1-to-clear (W1C) `IRQ_STATUS` bits |

IRQ bits (`IRQ_STATUS` / `IRQ_ENABLE`):

- `AEROGPU_IRQ_FENCE` (bit 0): completed fence advanced
- `AEROGPU_IRQ_SCANOUT_VBLANK` (bit 1): scanout vblank tick (only if `AEROGPU_FEATURE_VBLANK`)
- `AEROGPU_IRQ_ERROR` (bit 31): fatal device error

The interrupt line is asserted when `(IRQ_STATUS & IRQ_ENABLE) != 0`.

### 2.5 Scanout 0 registers (framebuffer + timing)

> These registers are present only when `AEROGPU_FEATURE_SCANOUT` is set.

Scanout configuration:

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0400` | `AEROGPU_MMIO_REG_SCANOUT0_ENABLE` | RW | 0/1 |
| `0x0404` | `AEROGPU_MMIO_REG_SCANOUT0_WIDTH` | RW | Width in pixels |
| `0x0408` | `AEROGPU_MMIO_REG_SCANOUT0_HEIGHT` | RW | Height in pixels |
| `0x040C` | `AEROGPU_MMIO_REG_SCANOUT0_FORMAT` | RW | `enum aerogpu_format` |
| `0x0410` | `AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES` | RW | Bytes per row |
| `0x0414` | `AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO` | RW | Framebuffer GPA (low 32 bits) |
| `0x0418` | `AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI` | RW | Framebuffer GPA (high 32 bits) |

Vblank timing registers (only when `AEROGPU_FEATURE_VBLANK` is set; see `vblank.md` for semantics):

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0420` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO` | RO | Vblank sequence (low 32 bits) |
| `0x0424` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI` | RO | Vblank sequence (high 32 bits) |
| `0x0428` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO` | RO | Last vblank time in ns (low 32 bits) |
| `0x042C` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI` | RO | Last vblank time in ns (high 32 bits) |
| `0x0430` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS` | RO | Nominal vblank period in ns |

### 2.6 Cursor registers

> These registers are present only when `AEROGPU_FEATURE_CURSOR` is set.

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0500` | `AEROGPU_MMIO_REG_CURSOR_ENABLE` | RW | 0/1 |
| `0x0504` | `AEROGPU_MMIO_REG_CURSOR_X` | RW | Signed X position (pixels) |
| `0x0508` | `AEROGPU_MMIO_REG_CURSOR_Y` | RW | Signed Y position (pixels) |
| `0x050C` | `AEROGPU_MMIO_REG_CURSOR_HOT_X` | RW | Hotspot X |
| `0x0510` | `AEROGPU_MMIO_REG_CURSOR_HOT_Y` | RW | Hotspot Y |
| `0x0514` | `AEROGPU_MMIO_REG_CURSOR_WIDTH` | RW | Width in pixels |
| `0x0518` | `AEROGPU_MMIO_REG_CURSOR_HEIGHT` | RW | Height in pixels |
| `0x051C` | `AEROGPU_MMIO_REG_CURSOR_FORMAT` | RW | `enum aerogpu_format` |
| `0x0520` | `AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO` | RW | Cursor framebuffer GPA (low 32 bits) |
| `0x0524` | `AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI` | RW | Cursor framebuffer GPA (high 32 bits) |
| `0x0528` | `AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES` | RW | Bytes per row |

---

## 3. Submission transport: shared ring + submit descriptors

The device uses a single shared submission ring in guest memory.
Completion is signaled via a **monotonic 64-bit fence**.

Ring and submission structs are defined in `aerogpu_ring.h`.

### 3.1 Ring layout (`struct aerogpu_ring_header`)

The ring is a contiguous guest memory region starting at `RING_GPA`:

```
ring_gpa:
  +0x00  struct aerogpu_ring_header (64 bytes)
  +0x40  ring slots[entry_count] (each slot is `entry_stride_bytes` bytes and begins with an `aerogpu_submit_desc` prefix)
```

`aerogpu_ring_header` fields (packed; 64 bytes total):

| Offset | Type | Field | Notes |
|---:|---|---|---|
| `0x00` | `u32` | `magic` | Must be `AEROGPU_RING_MAGIC` (`0x474E5241`, `"ARNG"` LE) |
| `0x04` | `u32` | `abi_version` | Must be `AEROGPU_ABI_VERSION_U32` |
| `0x08` | `u32` | `size_bytes` | Declared ring size in bytes (must be `<= AEROGPU_MMIO_REG_RING_SIZE_BYTES`) |
| `0x0C` | `u32` | `entry_count` | Number of slots; must be power-of-two |
| `0x10` | `u32` | `entry_stride_bytes` | Must be `>= sizeof(struct aerogpu_submit_desc)` (forward-compatible extension space) |
| `0x14` | `u32` | `flags` | Reserved (0) |
| `0x18` | `volatile u32` | `head` | Device-owned; monotonically increasing submission index |
| `0x1C` | `volatile u32` | `tail` | Driver-owned; monotonically increasing submission index |
| `0x20` | `u32` | `reserved0` | Must be 0 |
| `0x24` | `u32` | `reserved1` | Must be 0 |
| `0x28` | `u64[3]` | `reserved2` | Must be 0 |

Head/tail semantics:

- `head` and `tail` are **monotonic indices** (not masked).
- The ring slot for an index is `(index % entry_count)`.
- The driver must not advance `tail` so far that it would overwrite unconsumed entries
  (i.e. it must ensure `(tail - head) < entry_count` modulo `u32` wraparound rules).

### 3.2 Submitting work

For each submission:

1. Write an `aerogpu_submit_desc` into slot `(tail % entry_count)`.
2. Increment `ring->tail` by 1.
3. Write any value to `AEROGPU_MMIO_REG_DOORBELL`.

The device consumes entries in order, updating `ring->head`.

### 3.3 Submission descriptor (`struct aerogpu_submit_desc`)

The submit descriptor prefix is 64 bytes (packed). `desc_size_bytes` is a minimum so newer minor
versions can append fields (and must be `<= ring.entry_stride_bytes`):

| Offset | Type | Field | Description |
|---:|---|---|---|
| `0x00` | `u32` | `desc_size_bytes` | Must be `>= sizeof(struct aerogpu_submit_desc)` (64) |
| `0x04` | `u32` | `flags` | `enum aerogpu_submit_flags` |
| `0x08` | `u32` | `context_id` | Driver-defined (0 == default/unknown) |
| `0x0C` | `u32` | `engine_id` | `enum aerogpu_engine_id` (only `AEROGPU_ENGINE_0`) |
| `0x10` | `u64` | `cmd_gpa` | Command buffer guest physical address |
| `0x18` | `u32` | `cmd_size_bytes` | Command buffer size in bytes |
| `0x1C` | `u32` | `cmd_reserved0` | Must be 0 |
| `0x20` | `u64` | `alloc_table_gpa` | Optional allocation table GPA (0 if not present) |
| `0x28` | `u32` | `alloc_table_size_bytes` | Optional allocation table size (0 if not present) |
| `0x2C` | `u32` | `alloc_table_reserved0` | Must be 0 |
| `0x30` | `u64` | `signal_fence` | Fence value to signal when the submission completes |
| `0x38` | `u64` | `reserved0` | Must be 0 |

Descriptor validation rules (from `aerogpu_ring.h`):

- `cmd_gpa` and `cmd_size_bytes` must be both zero (empty submission) or both non-zero.
- When `cmd_gpa/cmd_size_bytes` are non-zero, `cmd_gpa + cmd_size_bytes` must not overflow.
- `alloc_table_gpa` and `alloc_table_size_bytes` must be both zero (absent) or both non-zero
  (present).
- When `alloc_table_gpa/alloc_table_size_bytes` are non-zero, `alloc_table_gpa + alloc_table_size_bytes`
  must not overflow.

Submission flags (`enum aerogpu_submit_flags`):

- `AEROGPU_SUBMIT_FLAG_PRESENT` (bit 0): submission contains a present (hint for scheduling/pacing)
- `AEROGPU_SUBMIT_FLAG_NO_IRQ` (bit 1): do not raise a fence IRQ for this submission

### 3.4 Optional allocation table

Each submission may reference a sideband allocation table, allowing commands to refer to guest memory via small `alloc_id`s.

The table is a guest-memory blob at `alloc_table_gpa` with:

1. `struct aerogpu_alloc_table_header` (magic `"ALOC"`)
2. `struct aerogpu_alloc_entry entries[entry_count]`

See `aerogpu_ring.h` for the exact structs and validation rules.

### 3.5 Fence / completion model (+ optional fence page)

Fences are monotonic 64-bit values chosen by the guest driver:

- Each submission provides `signal_fence`.
- Once that submission finishes, the device updates the completed fence to at least that value.

Completion is observable via:

- MMIO `AEROGPU_MMIO_REG_COMPLETED_FENCE_LO/HI` (**always available**), and
- optionally a shared `struct aerogpu_fence_page` if the device reports `AEROGPU_FEATURE_FENCE_PAGE`
  and the driver programs `AEROGPU_MMIO_REG_FENCE_GPA_LO/HI`.

If interrupts are enabled, the device raises `AEROGPU_IRQ_FENCE` when the completed fence advances
(unless the submission requested `AEROGPU_SUBMIT_FLAG_NO_IRQ`).

### 3.6 Optional fence page (`struct aerogpu_fence_page`)

If the device reports `AEROGPU_FEATURE_FENCE_PAGE`, the driver may program
`AEROGPU_MMIO_REG_FENCE_GPA_LO/HI` with the GPA of a guest page containing:

- `magic = AEROGPU_FENCE_PAGE_MAGIC` (`0x434E4546`, `"FENC"` LE)
- `abi_version = AEROGPU_ABI_VERSION_U32`
- `completed_fence` (volatile `u64`)

`aerogpu_fence_page` fields (packed; 56 bytes used, but the mapping should be a single 4 KiB guest page):

| Offset | Type | Field | Notes |
|---:|---|---|---|
| `0x00` | `u32` | `magic` | Must be `AEROGPU_FENCE_PAGE_MAGIC` |
| `0x04` | `u32` | `abi_version` | Must be `AEROGPU_ABI_VERSION_U32` |
| `0x08` | `volatile u64` | `completed_fence` | Mirrors MMIO `COMPLETED_FENCE_*` |
| `0x10` | `u64[5]` | `reserved0` | Must be 0 |

---

## 4. Command stream (“AeroGPU IR”)

Command buffers are byte streams in guest memory, referenced by:

- `aerogpu_submit_desc::cmd_gpa`
- `aerogpu_submit_desc::cmd_size_bytes`

Command formats are defined in `aerogpu_cmd.h`.

### 4.1 Stream header (`struct aerogpu_cmd_stream_header`)

Every command buffer begins with:

- `magic = AEROGPU_CMD_STREAM_MAGIC` (`0x444D4341`, `"ACMD"` LE)
- `abi_version = AEROGPU_ABI_VERSION_U32`

The header includes `size_bytes`, the total bytes in the stream including the header. It must be
`<= aerogpu_submit_desc::cmd_size_bytes`; any trailing bytes in the command buffer beyond
`size_bytes` are ignored (forward-compatible padding/extension space).

### 4.2 Packet framing (`struct aerogpu_cmd_hdr`)

After the stream header is a sequence of packets, each beginning with:

```
struct aerogpu_cmd_hdr {
  u32 opcode;     // enum aerogpu_cmd_opcode
  u32 size_bytes; // total packet size including this header
};
```

Forward-compat rules (from `aerogpu_cmd.h`):

- `size_bytes` **includes** the packet header.
- `size_bytes` must be `>= sizeof(aerogpu_cmd_hdr)` and **4-byte aligned**.
- Unknown opcodes must be **skipped** using `size_bytes` (do not treat as fatal).

The full opcode list and packet payload layouts live in `aerogpu_cmd.h`.
