# 16 - AeroGPU Legacy VGA/VBE Compatibility (Boot Display)

## Goal

Windows 7 must be able to **show a working boot display before the AeroGPU WDDM driver is installed/loaded**:

- BIOS POST text output (INT 10h + direct text VRAM writes)
- Windows boot logo / installer UI (VBE linear framebuffer graphics mode)
- After the AeroGPU WDDM KMD loads, scanout must **handoff** to the WDDM-programmed framebuffer without requiring a second “legacy VGA” adapter.

This document specifies the emulator-side behavior required for the **AeroGPU virtual PCI device** to be VGA/VESA-compatible enough for early boot, while still supporting a clean transition to the WDDM path.

## Current status (canonical machine)

This document describes the **desired** end state for the AeroGPU device model (A3A0:0001) to own
both the legacy VGA/VBE boot display path and the modern WDDM/MMIO/ring protocol.

The canonical `aero_machine::Machine` supports **two mutually-exclusive** display configurations:

- **AeroGPU (canonical / long-term):** `MachineConfig::enable_aerogpu=true` exposes the canonical
  AeroGPU PCI identity at `00:07.0` (`A3A0:0001`) with the canonical BAR layout (BAR0 regs + BAR1
  VRAM aperture). In `aero_machine` today this wires the **BAR1 VRAM aperture** to a dedicated
  host-backed VRAM buffer and implements minimal **legacy VGA decode** (permissive VGA port I/O +
  a VRAM-backed `0xA0000..0xBFFFF` window). An MVP BAR0 device model is also present (ring decode +
  fences + scanout/cursor regs + vblank pacing + error-info latches + submission capture for external execution), and
  `Machine::display_present()`
  will prefer the WDDM-programmed scanout framebuffer once scanout0 has been **claimed** (valid config +
  `SCANOUT0_ENABLE=1`). After claim, WDDM scanout remains authoritative until the VM resets.
  Writing `SCANOUT0_ENABLE=0` acts as a visibility toggle (blanking / stopping vblank pacing) and
  does not release WDDM ownership (legacy output remains suppressed until reset).

  Concretely:

  - VRAM backing + legacy decode: `crates/aero-machine/src/lib.rs` (`AeroGpuDevice`,
    `AeroGpuLegacyVgaMmio`, `AeroGpuVgaPortWindow`)
  - BAR0 MMIO + ring/fence + scanout/vblank register storage: `crates/aero-machine/src/aerogpu.rs`
    (`AeroGpuMmioDevice`) + host presentation in `crates/aero-machine/src/lib.rs`
    (`Machine::display_present_aerogpu_scanout`)

  The BIOS VBE implementation uses a linear framebuffer inside BAR1. `aero_machine` sets the VBE
  `PhysBasePtr` to `BAR1_BASE + 0x40000` (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; see
  `crates/aero-machine/src/lib.rs::VBE_LFB_OFFSET`) so INT 10h VBE mode set/clear writes land in
  BAR1-backed VRAM (leaving the first 256KiB reserved for legacy VGA planar/text backing:
  4 × 64KiB planes).

  `aero_machine` does not execute `AEROGPU_CMD` in-process by default. Instead, it supports:
  - the **submission bridge** (browser runtime): drain decoded submissions and complete fences from an external executor (GPU worker), and/or
  - optional in-process backends for native/tests (`immediate`/`null`, plus a feature-gated wgpu backend).

  When no backend/bridge is installed, BAR0 completes fences without executing ACMD so the Win7 KMD
  doesn't deadlock.

  Shared device-side building blocks (ring helpers + backend boundary + native backend wrapper) live in
  `crates/aero-devices-gpu`, with legacy sandbox integration in `crates/emulator`
  (see: [`21-emulator-crate-migration.md`](./21-emulator-crate-migration.md)).
- **Legacy VGA/VBE (transitional):** `MachineConfig::enable_vga=true` uses the standalone
  `aero_gpu_vga` VGA/VBE device model for boot display.
  - When the PC platform is enabled, the machine exposes a minimal Bochs/QEMU-compatible “Standard VGA”
    PCI function (currently `00:0c.0`) and routes the VBE linear framebuffer (LFB) through its BAR0
    inside the PCI MMIO window. BIOS POST / the PCI resource allocator assigns BAR0 (and may relocate
    it when other PCI devices are present); `aero_machine` mirrors the chosen BAR base into the BIOS
    VBE `PhysBasePtr` and the `aero_gpu_vga::VgaDevice` LFB base so guests see hardware-like behavior.
  - When the PC platform is disabled, the LFB is mapped directly at the configured base, which
    historically defaults to `0xE000_0000` via `aero_gpu_vga::SVGA_LFB_BASE`.

`enable_aerogpu` and `enable_vga` are **mutually exclusive** (the machine rejects configurations
that enable both).

See:

- [`docs/abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md) (canonical AeroGPU VID/DID)
- [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) (BDF allocation)

---

## High-level model

At all times, the browser canvas renders from exactly one active scanout source:

```text
Legacy VGA text / VBE LFB  ──(WDDM claims scanout)──▶  WDDM scanout (owned)
            ▲                                         │
            │                                         ├──(SCANOUT0_ENABLE=0)──▶  WDDM scanout (disabled / blank)
            │                                         │                            │
            │                                         └──(SCANOUT0_ENABLE=1)◀─────┘
            └──────────────────────(VM reset)─────────┘
```

Implementation-wise, AeroGPU owns **both**:

1. A minimal legacy VGA/VBE frontend (ports + legacy VRAM window + VBE mode set)
2. The modern AeroGPU MMIO/WDDM frontend (command queue, allocations, scanout registers)

The emulator’s display subsystem selects which framebuffer to present based on a single authoritative state structure (`ScanoutState`), updated by either:

- VBE `Set Mode` during boot, or
- AeroGPU WDDM scanout registers once the driver is active.

---

## 1) PCI identity and legacy decode requirements

### PCI class

AeroGPU must enumerate as a **VGA-compatible display controller**:

- **Class code:** `0x03` (Display controller)
- **Subclass:** `0x00` (VGA compatible controller)
- **Prog IF:** `0x00`

This ensures firmware/OS treat it as the primary boot display candidate, and enables the expectation that it decodes legacy VGA ranges.

### BAR layout (recommended)

To support both WDDM and legacy/VBE:

| BAR | Type | Size | Purpose |
|-----|------|------|---------|
| BAR0 | MMIO | 64KB | AeroGPU control registers (incl. WDDM scanout regs) |
| BAR1 | Prefetchable MMIO | 64MiB (canonical profile) | Dedicated VRAM aperture (contains legacy VGA window + VBE LFB + optional “VRAM allocations”) |

The emulator BIOS assigns BAR addresses within the reserved below-4 GiB PCI/MMIO hole
(`0xC000_0000..0x1_0000_0000`). The current BAR allocator places device MMIO BARs starting at
`0xE000_0000`. Firmware and guests must treat the assigned AeroGPU BAR bases as **dynamic** (do not
assume a fixed physical address).

### Legacy ranges to decode

Regardless of BARs, AeroGPU must decode these standard VGA ranges:

#### I/O ports

- `0x3C0–0x3DF` (attribute, sequencer, graphics controller, CRTC, misc, status)
- `0x3B0–0x3BB` and `0x3D0–0x3DF` CRTC aliasing (mono vs color)
- Palette/DAC ports: `0x3C6–0x3C9` (subset of `0x3C0–0x3DF`, called out because Windows uses them)

#### Legacy VRAM window

- `0xA0000–0xBFFFF` (128KB)

The emulator’s memory bus must route this window to the AeroGPU legacy VGA frontend (not to RAM), even though it lies in the “conventional memory” address region.

---

## 2) VRAM mapping and how it relates to WDDM allocations

### Dedicated VRAM region

For determinism and to avoid conflicts with guest RAM paging, legacy VGA/VBE uses a dedicated VRAM buffer owned by AeroGPU and exposed via BAR1:

```text
BAR1 (VRAM aperture) base: BAR1_BASE (assigned by BIOS)

VRAM offset    Purpose
0x00000..0x3FFFF  Legacy VGA planar memory (4 × 64KiB planes; includes the CPU-visible `0xA0000..0xBFFFF` window backing)
0x40000..          VBE linear framebuffer (LFB) base (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; packed-pixel VBE modes)
...                Optional: WDDM allocations in VRAM (if implemented)
```

### Legacy window aliasing

The guest-visible legacy VGA decode range is still the standard 128KiB aperture:

- `0xA0000–0xBFFFF` (128KB)

In the canonical `aero_machine` implementation today, this range is VRAM-backed:

```text
# VBE inactive (VGA/text): linear alias.
0xA0000..0xBFFFF  <->  VRAM[0x00000..0x1FFFF]

# VBE active: 0xA0000..0xAFFFF becomes the VBE banked window into the VBE framebuffer region
# starting at VBE_LFB_OFFSET (selected by the current 64KiB bank).
0xA0000..0xAFFFF  <->  VRAM[VBE_LFB_OFFSET + vbe_bank*64KiB + off]
0xB0000..0xBFFFF  <->  VRAM[0x10000..0x1FFFF]
```

This is sufficient for BIOS POST + bootloader text output and for the firmware VBE implementation
to share the same VRAM backing store.

Note: legacy VGA hardware exposes a 128KiB CPU-visible window (`0xA0000..0xBFFFF`). In
`aero_machine` today this is modeled as a simple 128KiB linear alias at `VRAM[0x00000..0x1FFFF]`.

### VBE LFB base address

When AeroGPU is enabled, VBE mode info must report:

`PhysBasePtr = BAR1_BASE + 0x40000` (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; aligned to 64KB; `VBE_LFB_OFFSET` in `aero_machine`).

Windows 7 boot graphics and installer UI will draw directly into this linear framebuffer.

### Relation to WDDM allocations

The recommended rule is:

- **Legacy VGA uses a fixed reserved subregion of VRAM** (`0x00000..0x3FFFF`).
- **The VBE packed-pixel linear framebuffer starts at `0x40000`** (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`) and consumes
  `width * height * bytes_per_pixel` bytes (depending on the active VBE mode).
- **WDDM allocations (today)** in the in-tree Win7 AeroGPU driver are system-memory-backed (guest
  RAM), i.e. the driver reports no dedicated VRAM segment (see
  `docs/graphics/win7-wddm11-aerogpu-driver.md`). BAR1 exists primarily for legacy VGA/VBE
  compatibility.
- **Optional future:** WDDM allocations may use the remaining BAR1 VRAM space (not overlapping the
  active legacy/VBE region) if/when the device model + driver contract are extended to support it.

To keep the handoff simple, the scanout logic treats the WDDM-programmed scanout base as a *guest physical address* that can point to either:

1. BAR1 VRAM space, or
2. Guest RAM (for “shared” allocations)

The emulator must be able to read pixels from either backing store when presenting.

---

## 3) Minimal legacy VGA behavior (boot text visibility)

Windows 7 setup/boot mostly relies on VBE for graphics, but BIOS POST and many bootloaders rely on VGA text mode.

### Text mode: required behavior

Implement enough for mode `0x03` (80x25 color text):

- The visible text buffer is at `0xB8000` (aliased to VRAM as described above).
- Each cell is 2 bytes: `[char][attr]`.
- The renderer converts this to pixels using an 8x16 or 9x16 font (any consistent VGA-ish font is acceptable for the emulator).
- Basic cursor support is optional for Windows boot, but BIOS POST benefits from it; implement CRTC cursor registers if available.

### Mode 13h: optional behavior

Mode `0x13` (320x200x256) is not required for Windows 7 boot (which uses VBE LFB modes), but some
bootloaders and DOS-style guests use it. A minimal implementation can model mode 13h as a simple
linear 64KiB framebuffer at `0xA0000` with 8-bit palette indices and render it using the VGA DAC
palette.

### VGA ports: minimal subset

For early boot stability, most VGA ports can be permissive no-ops, but these should behave plausibly:

- `0x3C2` Misc Output (store written value; influences CRTC base port selection in real hardware)
- `0x3C4/0x3C5` Sequencer index/data (store regs)
- `0x3CE/0x3CF` Graphics controller index/data (store regs; allow reads)
- `0x3D4/0x3D5` CRTC index/data (store regs; allow reads)
- `0x3C0/0x3C1` Attribute controller (index flip-flop; store regs)
- `0x3DA` Input Status 1 (read resets attribute flip-flop; return a value with bit 3 “vertical retrace” toggling is optional)

If a port is unimplemented, returning `0xFF` on reads and ignoring writes is typically sufficient to keep guests alive.

---

## 4) VBE (VESA BIOS Extensions) for Windows 7 boot graphics

Windows 7’s boot path will query VBE via INT 10h `AX=4Fxx` and set a linear framebuffer mode.

### Required modes

Expose at least these 32bpp linear framebuffer modes:

| Resolution | BitsPerPixel | Mode number (suggested) |
|------------|--------------|--------------------------|
| 800x600    | 32           | `0x115` |
| 1024x768   | 32           | `0x118` |
| 1280x720   | 32           | `0x160` (OEM-defined) |

The specific mode numbers are not important as long as:

- They appear in the VBE mode list returned by `4F00h`
- `4F01h` returns valid mode info
- `4F02h` can set them with linear framebuffer enabled

### Pixel format

Use a standard direct-color layout:

- `BitsPerPixel = 32`
- `RedMaskSize=8`, `RedFieldPosition=16`
- `GreenMaskSize=8`, `GreenFieldPosition=8`
- `BlueMaskSize=8`, `BlueFieldPosition=0`
- `ReservedMaskSize=8`, `ReservedFieldPosition=24`

This corresponds to little-endian **B8G8R8X8** in memory.

### Pitch

Set `BytesPerScanLine = width * 4`.

If the implementation prefers alignment (e.g. 16-byte), it must be reflected in `BytesPerScanLine`, and the renderer must use the programmed pitch.

### INT 10h VBE functions to implement

Minimum viable set for Windows boot:

- `AX=4F00h` Get Controller Info
- `AX=4F01h` Get Mode Info
- `AX=4F02h` Set VBE Mode (support bit 14 = linear framebuffer)
- `AX=4F03h` Get Current Mode

All other functions can return failure (`AX=014Fh`, CF=1) unless a specific guest requires them.

---

## 5) Scanout selection and handoff to WDDM

### Scanout state (single source of truth)

Define a scanout description readable by the presentation pipeline:

```rust
#[repr(C)]
pub struct ScanoutState {
    pub generation: u32,          // increment on every complete update
    pub source: u32,              // 0=LegacyText, 1=LegacyVbeLfb, 2=Wddm
    pub base_paddr_lo: u32,       // guest physical address (low)
    pub base_paddr_hi: u32,       // guest physical address (high)
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: u32,              // AerogpuFormat / enum aerogpu_format (e.g. B8G8R8X8Unorm = 2)
}
```

`format` must store the AeroGPU format discriminant, matching
[`drivers/aerogpu/protocol/aerogpu_pci.h`](../drivers/aerogpu/protocol/aerogpu_pci.h)
`enum aerogpu_format` (i.e. it is *not* a bespoke 0-based scanout-only enum).

The update rule:

1. Writer populates all fields except `generation`
2. Writer publishes the update by incrementing `generation` last (release semantics)
3. Reader snapshots `generation`, reads fields, then re-reads `generation` to verify a consistent view

**Implementation note (recommended):** To prevent readers from observing a partially-updated
descriptor, implementations may temporarily mark `generation` as “busy” (e.g. by setting a high
bit) during an update. Readers should retry if `generation` is marked busy.

This makes scanout switching glitch-free without locks.

### Scanout format semantics (presentation)

- **sRGB vs UNORM:** sRGB variants are byte-identical to their UNORM counterparts, but the *interpretation*
  differs. Sampling should decode sRGB→linear and render-target writes/views may encode linear→sRGB.
  Presenters must avoid double-applying gamma when handling `*_SRGB` scanout formats.
- **X8 alpha semantics:** `B8G8R8X8*` / `R8G8B8X8*` formats must be treated as fully opaque when
  presenting. If converting to RGBA (e.g. browser canvas), alpha is implicitly `1.0` / `0xFF`.

### Required WDDM scanout programming surface (BAR0)

The canonical A3A0:0001 MMIO register map is defined by
[`drivers/aerogpu/protocol/aerogpu_pci.h`](../drivers/aerogpu/protocol/aerogpu_pci.h)
(grep for `AEROGPU_MMIO_REG_SCANOUT0_*`). This section exists only to summarize the scanout
programming subset relevant to **boot-display handoff**.

Scanout 0 registers (BAR0):

| Offset | Name | Width | Description |
|--------|------|-------|-------------|
| `0x0400` | `SCANOUT0_ENABLE` (`AEROGPU_MMIO_REG_SCANOUT0_ENABLE`) | 32 | 0/1 |
| `0x0404` | `SCANOUT0_WIDTH` (`AEROGPU_MMIO_REG_SCANOUT0_WIDTH`) | 32 | Width in pixels |
| `0x0408` | `SCANOUT0_HEIGHT` (`AEROGPU_MMIO_REG_SCANOUT0_HEIGHT`) | 32 | Height in pixels |
| `0x040C` | `SCANOUT0_FORMAT` (`AEROGPU_MMIO_REG_SCANOUT0_FORMAT`) | 32 | `enum aerogpu_format` / `AerogpuFormat` (e.g. `B8G8R8X8Unorm = 2`) |
| `0x0410` | `SCANOUT0_PITCH_BYTES` (`AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES`) | 32 | Bytes per scanline |
| `0x0414` | `SCANOUT0_FB_GPA_LO` (`AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO`) | 32 | Framebuffer guest physical address (low 32) |
| `0x0418` | `SCANOUT0_FB_GPA_HI` (`AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI`) | 32 | Framebuffer guest physical address (high 32) |

**Atomic / "commit" semantics (canonical A3A0):**

- There is **no** separate `COMMIT` register/bit in the canonical A3A0 MMIO ABI. The Windows KMD
  stages `SCANOUT0_*` values and then writes `SCANOUT0_ENABLE`.
- Treat the write to `SCANOUT0_ENABLE` as the **commit point**, but only claim/publish WDDM scanout
  once the configuration is **valid**:
  - `SCANOUT0_ENABLE` must be `1` and the programmed scanout config must be valid (non-zero
    framebuffer GPA, non-zero width/height, supported pixel format, pitch large enough for the row
    size, etc). If the config is invalid (e.g. `FB_GPA=0` during early init), do **not** publish a
    WDDM scanout descriptor and do **not** steal legacy VGA/VBE presentation.
  - Avoid publishing a torn 64-bit `SCANOUT0_FB_GPA`: drivers typically write LO then HI, so treat
    the HI write as the commit point for the combined 64-bit address.
- After a valid WDDM scanout has been claimed, update `ScanoutState` on configuration changes
  (including flips via `SCANOUT0_FB_GPA_*` updates before PRESENT).
- If the guest clears `SCANOUT0_ENABLE` after claim, publish a **disabled WDDM scanout descriptor**
  (`base/width/height/pitch = 0`) so legacy VGA/VBE cannot steal scanout back while WDDM ownership
  is held.
- Presentation commands read the current scanout programming: `AEROGPU_CMD_PRESENT` uses the
  currently-programmed `SCANOUT0_*` registers, and drivers may update `SCANOUT0_FB_GPA_*` before
  PRESENT to implement flips (see
  [`drivers/aerogpu/protocol/aerogpu_cmd.h`](../drivers/aerogpu/protocol/aerogpu_cmd.h)).

### When legacy VGA/VBE owns scanout

- At reset/power-on: `source = LegacyText`, base points at the legacy text buffer (or can be implicit)
- When BIOS/bootloader sets a VBE LFB mode: `source = LegacyVbeLfb`, base = `BAR1_BASE + 0x40000` (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`), width/height/pitch filled

### When WDDM owns scanout

When the AeroGPU WDDM driver is loaded, it programs the AeroGPU scanout registers (BAR0). As soon
as the device observes `SCANOUT0_ENABLE=1` with a **valid** scanout configuration (including a
committed 64-bit framebuffer address), it updates `ScanoutState`:

- `source = Wddm`
- `base_paddr = value programmed by driver`
- `width/height/pitch/format = values programmed by driver`

From this point onward, legacy VGA/VBE writes do not affect the visible display while WDDM owns scanout.

### Compatibility rule once WDDM is active

After the first successful WDDM scanout enable (`SCANOUT0_ENABLE=1`):

- Legacy VGA/VBE ports and memory windows may continue to accept reads/writes for compatibility.
- The emulator presentation must ignore legacy sources once WDDM has claimed scanout.
- If WDDM disables scanout (`SCANOUT0_ENABLE=0`), it blanks output (stopping vblank pacing) but does
  not release ownership: legacy output remains suppressed until VM reset.
- VM reset always releases WDDM ownership and reverts to legacy scanout.

This prevents legacy writes (e.g. an errant `INT 10h`) from stealing the primary display after the desktop is up.

---

## 6) Browser canvas presentation requirements

The emulator must render:

1. VGA text mode (from `0xB8000` backing) during POST/bootloader
2. VBE LFB modes for Windows 7 boot + installer graphics
3. WDDM scanout after the AeroGPU driver loads

Presentation pipeline requirements:

- Source selection is based solely on `ScanoutState`.
- Pixel conversion handles BGRA/BGRX/RGBA/RGBX (and sRGB variants) → canvas RGBA. X8 formats must force `alpha=255`.
- Switching sources is atomic and does not free/relocate the backing memory, preventing stale pointers.

---

## 7) Web runtime implementation notes (wasm32 browser runtime)

This section documents the **current** implementation used by the web/wasm32 runtime. It exists to
explain the somewhat non-obvious contract between:

- guest physical addresses (including the PCI/MMIO hole),
- the browser runtime’s `SharedArrayBuffer` layout, and
- scanout readback in the GPU worker.

### VRAM (BAR1 backing) as a SharedArrayBuffer

In the web runtime, BAR1/VRAM is represented as a dedicated `SharedArrayBuffer` (separate from the
`WebAssembly.Memory` guest RAM buffer) so that:

- VRAM does not consume wasm32 linear memory budget (wasm32 is limited to 4 GiB without `memory64`),
- the I/O worker can service MMIO writes into BAR1, and
- the GPU worker can read back scanout/cursor pixels from the same bytes without copying.

Allocation and wiring:

- VRAM is allocated by the coordinator in
  [`web/src/runtime/shared_layout.ts`](../web/src/runtime/shared_layout.ts)
  `allocateSharedMemorySegments(...)`.
  - Default size is `DEFAULT_VRAM_MIB = 64`.
  - `vramMiB=0` disables the segment (primarily for tests).
- Shared memory contract: when present, `segments.vram` backs the guest physical address range:
  `[VRAM_BASE_PADDR, VRAM_BASE_PADDR + vram.byteLength)`.
  (See `SharedMemorySegments.vram` in the same file.)
- The I/O worker maps this buffer as an MMIO region at `vramBasePaddr` so guest CPU reads/writes to
  BAR1 land in the VRAM SAB (see [`web/src/workers/io.worker.ts`](../web/src/workers/io.worker.ts),
  `DeviceManager.registerMmio(...)`).
- The VRAM aperture reserves the front of the PCI/MMIO BAR allocation window. The I/O worker
  configures the PCI BAR allocator base to start *after* VRAM (`pciMmioBase = vramBasePaddr +
  vramSizeBytes`) so other MMIO BARs do not overlap the VRAM range (see
  `DeviceManagerOptions.pciMmioBase` in [`web/src/io/device_manager.ts`](../web/src/io/device_manager.ts)).

### VRAM base paddr contract and why `base_paddr` can live in the PCI/MMIO hole

The web runtime uses fixed constants for BAR placement:

- `PCI_MMIO_BASE = 0xE000_0000` in
  [`web/src/arch/guest_phys.ts`](../web/src/arch/guest_phys.ts) and
  [`crates/aero-wasm/src/guest_layout.rs`](../crates/aero-wasm/src/guest_layout.rs).
- `VRAM_BASE_PADDR = PCI_MMIO_BASE` (i.e. VRAM lives inside the canonical PC/Q35 PCI/MMIO hole).

Note: this is a **web-runtime implementation contract**. It differs from the canonical
`aero_machine` model where BAR bases are assigned by BIOS and must be treated as dynamic by the
guest. In the browser runtime today, VRAM is reserved at a fixed guest-physical address to keep
multi-worker mapping simple; if/when BAR1 becomes a fully dynamic PCI BAR in the web runtime, this
contract may be revisited.

To avoid overlapping the contiguous wasm linear-memory guest RAM buffer with the PCI BAR window,
the web runtime clamps `guest_size <= PCI_MMIO_BASE` (see `computeGuestRamLayout(...)` in
`web/src/runtime/shared_layout.ts`).

Combined with the Q35-style physical map used by the web runtime (`LOW_RAM_END = 0xB000_0000`,
`HIGH_RAM_START = 0x1_0000_0000`), guest RAM is backed by `WebAssembly.Memory` but guest *physical*
addresses are not always identity-mapped:

- If `guest_size <= LOW_RAM_END`, RAM is contiguous and identity-mapped: `[0, guest_size)` is RAM.
- If `guest_size > LOW_RAM_END`, RAM is split:
  - Low RAM:  `[0, LOW_RAM_END)` (backed by wasm memory at offset 0)
  - Hole:     `[LOW_RAM_END, HIGH_RAM_START)` (ECAM + PCI/MMIO; **not** backed by RAM)
  - High RAM: `[HIGH_RAM_START, HIGH_RAM_START + (guest_size - LOW_RAM_END))` (backed by the
    “high” portion of the contiguous wasm buffer)

BAR1/VRAM is mapped into the hole at `VRAM_BASE_PADDR = 0xE000_0000`, so scanout/cursor pointers
can legitimately live in the PCI/MMIO region.

This is why `ScanoutState.base_paddr` can point into the PCI/MMIO hole while still being valid: it
is a **guest physical address**, not a “RAM offset”.

### RAM translation helpers (Q35 hole + high-RAM remap)

Because the PC/Q35 guest physical map includes an ECAM/MMIO hole and can remap “high RAM” above
4 GiB, the web runtime cannot treat `guestU8[paddr]` as valid once those features are in play.

Any code that needs to read guest RAM from a guest physical address must use the shared translation
helpers:

- JS: [`web/src/arch/guest_ram_translate.ts`](../web/src/arch/guest_ram_translate.ts)
  - `guestPaddrToRamOffset(ramBytes, paddr)` → `number | null`
  - `guestRangeInBounds(ramBytes, paddr, len)` → `boolean`
- Rust mirror (used by wasm-side DMA bridges): [`crates/aero-wasm/src/guest_phys.rs`](../crates/aero-wasm/src/guest_phys.rs)
  (`translate_guest_paddr_range`, `translate_guest_paddr_chunk`, etc.)

In addition to scanout/cursor readback, the GPU worker’s TypeScript AeroGPU command executor uses
the same “RAM vs VRAM aperture” resolution policy when it needs to slice guest physical memory for
DMA-style operations (uploads/copies/etc):

- `web/src/workers/aerogpu-acmd-executor.ts` (`sliceGuestChecked(...)`)

### How the GPU worker resolves scanout `base_paddr` (RAM vs VRAM)

In the browser runtime, scanout presentation happens in
[`web/src/workers/gpu-worker.ts`](../web/src/workers/gpu-worker.ts).

For `ScanoutState.source = Wddm` (`SCANOUT_SOURCE_WDDM`), the worker:

1. Snapshots the shared scanout descriptor (`scanoutState` SAB).
2. Computes the required byte range:
   `requiredReadBytes = (height-1)*pitchBytes + width*bytesPerPixel`, where `bytesPerPixel` is derived
   from `ScanoutState.format` (typically 4 for `*8G8R8*8` formats and 2 for `B5G6R5` / `B5G5R5A1`).
3. Resolves `base_paddr` to a backing store:
   - If `base_paddr ∈ [vramBasePaddr, vramBasePaddr + vramSizeBytes)`, read from the VRAM SAB
     (`vramU8`) at offset `base_paddr - vramBasePaddr`.
   - Otherwise treat it as guest RAM and use `guestRangeInBounds` + `guestPaddrToRamOffset` to
     translate it into an offset into `guestU8`.
4. Converts the scanout surface to a tightly-packed RGBA8 buffer for presentation:
   - X8 formats force `alpha=0xFF` (fully opaque); A8 formats preserve alpha.
   - For `*_SRGB` scanout formats, the GPU worker decodes sRGB→linear after swizzle so blending/presentation happens in linear space.

Notes on `base_paddr == 0` for `source=Wddm`:

- **Disabled WDDM descriptor** (matches the required scanout handoff contract above): when the guest disables scanout after WDDM has claimed it, the device model publishes a descriptor with
  `base/width/height/pitch = 0`. This represents **blank output** while WDDM retains scanout ownership (legacy output remains suppressed until reset).
- **Placeholder WDDM descriptor** (web-runtime/harness-only): some host-side harnesses publish `base_paddr=0` with **non-zero** `width/height/pitch` as a placeholder for the host-side AeroGPU path (no guest-memory scanout readback). This is not part of the guest-facing AeroGPU MMIO ABI; it is an implementation detail used by some tests.

This same “VRAM aperture fast-path” idea is also used for WDDM hardware cursor surfaces (which are
often allocated in VRAM).

### Worker init / shared-memory fields (web runtime contract)

The coordinator hands these buffers to workers via the `postMessage` init message
[`WorkerInitMessage`](../web/src/runtime/protocol.ts):

- `vram`, `vramBasePaddr`, `vramSizeBytes`: describe the shared VRAM aperture (BAR1 backing).
- `scanoutState`, `scanoutStateOffsetBytes`: shared scanout descriptor
  (layout in [`web/src/ipc/scanout_state.ts`](../web/src/ipc/scanout_state.ts)).
- `cursorState`, `cursorStateOffsetBytes`: shared hardware cursor descriptor.

Workers should treat these as immutable for the lifetime of a VM instance.

### Current limitations

- WDDM scanout readback currently supports:
  - 32bpp packed: `B8G8R8X8` / `B8G8R8A8` / `R8G8B8X8` / `R8G8B8A8` (plus their sRGB variants).
  - 16bpp packed: `B5G6R5` (opaque) and `B5G5R5A1` (1-bit alpha).
- WDDM hardware cursor surfaces support `B8G8R8X8` / `B8G8R8A8` / `R8G8B8X8` / `R8G8B8A8`
  (plus their sRGB variants).
- Readback paths require `base_paddr` and derived byte ranges to fit within JS safe integer range
  (`<= 2^53-1`).
- Some unit tests/harnesses set `vramMiB=0`, in which case VRAM-backed scanout/cursor surfaces are
  unavailable.

### Tests / validation pointers

- Rust: scanout handoff + disable semantics
  - `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_scanout_handoff --locked`
  - `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_scanout_disable_publishes_wddm_disabled --locked`
- E2E (guest RAM scanout): `bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts`
  (harness: `web/wddm-scanout-smoke.ts`)
- E2E (VRAM aperture scanout): `bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/wddm_scanout_vram_smoke.spec.ts`
  (harness: `web/wddm-scanout-vram-smoke.ts`)

---

## Acceptance checklist (manual)

In the emulator UI:

1. Power on and see BIOS POST text output.
2. Windows 7 boot graphics appears (not just blind boot).
3. Windows 7 installer UI appears in a VBE LFB mode.
4. After AeroGPU WDDM driver loads, the desktop continues rendering using the WDDM scanout path without flashing back to VGA/VBE.
