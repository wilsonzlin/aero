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

This document describes the **AeroGPU** ABI for the `A3A0:0001` PCI device model.

The canonical machine (`aero_machine::Machine`) supports **two mutually-exclusive** display configurations:

- `MachineConfig::enable_aerogpu=true`: expose the canonical AeroGPU PCI identity at `00:07.0`
  (`A3A0:0001`) with the canonical BAR layout (BAR0 regs + BAR1 VRAM aperture). In `aero_machine`
  today BAR1 is backed by a dedicated VRAM buffer for legacy VGA/VBE compatibility and implements
  permissive legacy VGA decode (VGA port I/O + VRAM-backed `0xA0000..0xBFFFF` window; see
  `docs/16-aerogpu-vga-vesa-compat.md`). Note: the in-tree Win7 AeroGPU driver treats the adapter
  as system-memory-backed (no dedicated WDDM VRAM segment); BAR1 is outside the WDDM memory model.
  BAR0 implements a minimal MMIO surface:
  - ring/fence transport (submission decode/capture + fence-page/IRQ plumbing). Default bring-up
    behavior can complete fences without executing the command stream; browser/WASM runtimes can
    enable an out-of-process “submission bridge” (`Machine::aerogpu_drain_submissions` +
    `Machine::aerogpu_complete_fence`) so the GPU worker can execute submissions and report fence
    completion, and native builds can optionally install a feature-gated in-process headless wgpu
    backend (`Machine::aerogpu_set_backend_wgpu`), and
  - scanout0/vblank register storage so the host can present a guest-programmed scanout framebuffer
    and the Win7 stack can use vblank pacing primitives (see `drivers/aerogpu/protocol/vblank.md`).

  Shared device-side building blocks (regs/ring/executor + reusable PCI wrapper) live in
  `crates/aero-devices-gpu`. Command execution is provided by host-side executors/backends (GPU
  worker execution via the submission bridge, or optional in-process backends in native/test
  builds). See: [`docs/graphics/status.md`](./graphics/status.md).
- `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`): boot display is provided by
  `aero_gpu_vga` (VGA + Bochs VBE).
  - When `enable_pc_platform=false`, the VBE LFB MMIO aperture is mapped directly at the configured base.
  - When `enable_pc_platform=true`, the machine exposes a minimal Bochs/QEMU-compatible “Standard VGA”
    PCI function (currently `00:0c.0`, `1234:1111`) and routes the VBE LFB through PCI BAR0 inside the PCI MMIO
    window / BAR router. The BAR base is assigned by BIOS POST / the PCI allocator (and may be
    relocated when other PCI devices are present), and the machine mirrors it into the BIOS VBE
    `PhysBasePtr` and the VGA device model so guests observe a coherent LFB base.

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

Implementation references: canonical machine MVP (`crates/aero-machine/src/aerogpu.rs`), shared device-side PCI wrapper + ring executor (`crates/aero-devices-gpu/src/pci.rs`), and legacy sandbox integration (`crates/emulator/src/devices/pci/aerogpu.rs`).

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

- BAR0: MMIO register block, **64 KiB** (`AEROGPU_PCI_BAR0_INDEX` / `AEROGPU_PCI_BAR0_SIZE_BYTES`)
- BAR1: prefetchable MMIO VRAM aperture, **64 MiB** (`AEROGPU_PCI_BAR1_INDEX` / `AEROGPU_PCI_BAR1_SIZE_BYTES`)
  used for VGA/VBE compatibility (outside the current Win7 WDDM memory model; see
  `docs/16-aerogpu-vga-vesa-compat.md`)

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
- `AEROGPU_FEATURE_ERROR_INFO` (bit 5): error reporting registers are implemented (ABI 1.3+; see below)

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

#### 2.4.1 Error reporting registers (ABI 1.3+)

When `AEROGPU_IRQ_ERROR` is asserted, the device also latches structured error
details into the following **read-only** MMIO registers:

> These registers are present only when `AEROGPU_FEATURE_ERROR_INFO` is set in
> `AEROGPU_MMIO_REG_FEATURES_LO/HI`.

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0310` | `AEROGPU_MMIO_REG_ERROR_CODE` | RO | `enum aerogpu_error_code` (stable error code) |
| `0x0314` | `AEROGPU_MMIO_REG_ERROR_FENCE_LO` | RO | Associated fence (low 32 bits) |
| `0x0318` | `AEROGPU_MMIO_REG_ERROR_FENCE_HI` | RO | Associated fence (high 32 bits) |
| `0x031C` | `AEROGPU_MMIO_REG_ERROR_COUNT` | RO | Monotonic error counter |

Semantics:

- `ERROR_*` registers are valid **only** when an error has been recorded (typically
  accompanied by `AEROGPU_IRQ_ERROR`).
- Clearing `IRQ_STATUS.ERROR` via `IRQ_ACK` does **not** clear the latched error
  payload; the registers remain valid until overwritten by a subsequent error.

Error code values (`enum aerogpu_error_code` in `aerogpu_pci.h`):

| Name | Value | Meaning |
|---|---:|---|
| `AEROGPU_ERROR_NONE` | 0 | No error / no latched error payload |
| `AEROGPU_ERROR_CMD_DECODE` | 1 | Malformed command stream / decode failure |
| `AEROGPU_ERROR_OOB` | 2 | Out-of-bounds access / address overflow |
| `AEROGPU_ERROR_BACKEND` | 3 | Backend/device execution error |
| `AEROGPU_ERROR_INTERNAL` | `0xFFFF` | Internal/unclassified error |

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

#### 2.5.1 Scanout/cursor `enum aerogpu_format` semantics (X8 alpha + sRGB)

These semantics apply to both `AEROGPU_MMIO_REG_SCANOUT0_FORMAT` and `AEROGPU_MMIO_REG_CURSOR_FORMAT`:

- **X8 formats are fully opaque.** For `B8G8R8X8*` / `R8G8B8X8*` formats, the 8-bit `X` channel is unused.
  Consumers must ignore the stored `X` byte and treat alpha as fully opaque (`A = 1.0` / `0xFF`) when converting
  to RGBA for scanout presentation or cursor blending.
- **sRGB formats change interpretation, not layout.** `*_UNORM_SRGB` formats have the exact same byte layout as the
  corresponding `*_UNORM` formats; only the *interpretation* differs. Sampling should decode sRGB→linear and
  render-target writes/views may encode linear→sRGB. Scanout/cursor presenters must not double-apply gamma.

Vblank timing registers (only when `AEROGPU_FEATURE_VBLANK` is set; see `vblank.md` for semantics):

| Offset | Name | Access | Description |
|---:|---|:--:|---|
| `0x0420` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO` | RO | Vblank sequence (low 32 bits) |
| `0x0424` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI` | RO | Vblank sequence (high 32 bits) |
| `0x0428` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO` | RO | Last vblank time in ns since boot (low 32 bits) |
| `0x042C` | `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI` | RO | Last vblank time in ns since boot (high 32 bits) |
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

### 4.3 Extended shader stage selector (`stage_ex`)

The legacy `enum aerogpu_shader_stage` only encodes VS/PS/CS (and later `GEOMETRY=3`). Geometry-stage
packets can therefore be encoded directly using `shader_stage = GEOMETRY` with `reserved0 = 0`.

To support additional programmable stages used by D3D11 (HS/DS) without breaking older hosts, some
packets reuse their trailing `reserved0` field as an **extended stage selector** when
`shader_stage == COMPUTE` (the same encoding may also be used for GS for compatibility).

This extension is only valid for command streams with ABI minor **>= 3** (ABI 1.3+). When decoding a
command stream whose header reports an ABI minor < 3, hosts must ignore `reserved0` (treat it as
`0`) even when `shader_stage == COMPUTE`, to avoid misinterpreting legacy reserved data.

Encoding invariant (must be enforced by writers and hosts):

- If `shader_stage != COMPUTE`, then `reserved0` **must** be `0` and is ignored.
- If `shader_stage == COMPUTE`:
  - `reserved0 == 0` means the real Compute stage (legacy behavior).
  - `reserved0 != 0` means an extended stage is present and `reserved0` encodes a non-zero
    `enum aerogpu_shader_stage_ex`.

GS note: because the legacy `enum aerogpu_shader_stage` includes `GEOMETRY=3`, producers should
prefer the direct encoding (`shader_stage = GEOMETRY`, `reserved0 = 0`) for GS. The `stage_ex`
encoding (`shader_stage = COMPUTE`, `reserved0 = 2`) may still be used for compatibility. HS/DS
require `stage_ex`.

`enum aerogpu_shader_stage_ex` numeric values intentionally align with the D3D DXBC “program type”
numbers used in the shader version token (`Pixel=0`, `Vertex=1`, `Geometry=2`, `Hull=3`,
`Domain=4`, `Compute=5`), but only the **non-legacy** stages are representable:

- `2=gs`, `3=hs`, `4=ds`, `5=cs` (optional alias; writers should encode Compute via `reserved0 = 0`)

`stage_ex = 1` (Vertex / DXBC program type 1) is intentionally **invalid**; vertex shaders must be
encoded via the legacy `shader_stage = VERTEX` encoding for clarity.

Pixel shaders are intentionally not representable via this extension because `0` is reserved for
legacy compute packets; pixel shaders must use the legacy `shader_stage = PIXEL` encoding.

See `drivers/aerogpu/protocol/aerogpu_cmd.h` for the authoritative definition and which packets
carry a `reserved0(stage_ex)` field. See `docs/16-d3d10-11-translation.md` for motivation and
examples.

### 4.4 Append-only packet extensions (size\_bytes-gated)

Some command packets may grow over time without breaking older parsers by using an **append-only**
extension pattern:

- The packet has a stable prefix layout (a packed C struct) that must never change.
- New fields are appended **after** the prefix in newer ABI minor versions.
- `aerogpu_cmd_hdr.size_bytes` indicates how many bytes are present for this packet.
- Readers must:
  - validate `size_bytes >= sizeof(prefix)` and that `size_bytes` is 4-byte aligned, and then
  - decode only the fields they understand, ignoring any trailing bytes they do not.

This is in addition to the “unknown opcode” forward-compat rule: even for a *known* opcode, the
payload layout may be extended by appending fields.

**Example: `AEROGPU_CMD_BIND_SHADERS`**

The base `struct aerogpu_cmd_bind_shaders` is a stable 24-byte prefix:

```
hdr (8) + vs (4) + ps (4) + cs (4) + reserved0 (4) = 24 bytes
```

In newer streams, if `hdr.size_bytes >= 36`, the packet appends **three additional** `u32`
shader handles:

- `gs` (geometry shader)
- `hs` (hull shader / tessellation control)
- `ds` (domain shader / tessellation eval)

For best-effort compatibility with legacy hosts that only understand the 24-byte packet, writers
may also mirror `gs` into the legacy `reserved0` field. Canonical decoding treats `reserved0` as the
legacy GS handle **only when `hdr.size_bytes == 24`**; when present the appended `{gs, hs, ds}`
handles are authoritative.

See the authoritative comment block above `struct aerogpu_cmd_bind_shaders` in
`drivers/aerogpu/protocol/aerogpu_cmd.h`.

Implementation note: the repository provides convenience helpers that emit the extended packet
layout directly:

- Rust: `aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter::bind_shaders_ex(vs, ps, cs, gs, hs, ds)`
- Rust (legacy-compat GS mirror): `aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter::bind_shaders_ex_with_gs_mirror(vs, ps, cs, gs, hs, ds)`
- Rust (HS/DS-only convenience): `aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter::bind_shaders_hs_ds(hs, ds)`
- TypeScript:
  - `AerogpuCmdWriter.bindShadersEx(vs, ps, cs, gs, hs, ds, mirrorGsToReserved0?)`
  - `AerogpuCmdWriter.bindShadersEx(vs, ps, cs, {gs, hs, ds}, mirrorGsToReserved0?)`
- TypeScript (HS/DS-only convenience): `AerogpuCmdWriter.bindShadersHsDs(hs, ds)`
- C++ (UMD cmd stream writers):
  - `aerogpu::CmdStreamWriter::bind_shaders_ex(vs, ps, cs, gs, hs, ds, mirror_gs_to_reserved0=false)`
  - `aerogpu::SpanCmdStreamWriter::bind_shaders_ex(vs, ps, cs, gs, hs, ds, mirror_gs_to_reserved0=false)`
  - `aerogpu::VectorCmdStreamWriter::bind_shaders_ex(vs, ps, cs, gs, hs, ds, mirror_gs_to_reserved0=false)`
- C++ (UMD HS/DS-only convenience):
  - `aerogpu::CmdStreamWriter::bind_shaders_hs_ds(hs, ds)`
  - `aerogpu::SpanCmdStreamWriter::bind_shaders_hs_ds(hs, ds)`
  - `aerogpu::VectorCmdStreamWriter::bind_shaders_hs_ds(hs, ds)`
