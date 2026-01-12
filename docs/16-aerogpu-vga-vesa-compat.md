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

Today, the canonical `aero_machine::Machine` does **not** yet expose the full AeroGPU PCI function.
Instead it uses the standalone `aero_gpu_vga` VGA/VBE device model for boot display, and exposes a
minimal Bochs/QEMU “Standard VGA”-like PCI stub at `00:0c.0` (`1234:1111`) solely so the fixed VBE
linear framebuffer can be routed through the PCI MMIO router.

See:

- [`docs/abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md) (canonical AeroGPU VID/DID)
- [`docs/pci-device-compatibility.md`](./pci-device-compatibility.md) (BDF allocation + transitional stub)

---

## High-level model

At all times, the browser canvas renders from exactly one active scanout source:

```text
Legacy VGA text / VBE LFB  ──(WDDM claims scanout)──▶  WDDM scanout
            ▲                                     │
            └──────────────(device reset)─────────┘
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
| BAR1 | Prefetchable MMIO | e.g. 32MB–128MB | Dedicated VRAM aperture (contains legacy VGA window + VBE LFB + optional “VRAM allocations”) |

The emulator BIOS assigns BAR addresses within the reserved below-4 GiB PCI/MMIO hole
(`0xC000_0000..0x1_0000_0000`). The current BAR allocator places device MMIO BARs starting at
`0xE000_0000` (so `0xE000_0000` is a typical BAR1 base in examples).

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
BAR1 (VRAM aperture) base: VRAM_BASE (assigned by BIOS)

VRAM offset    Purpose
0x00000..0x1FFFF  Legacy VGA window backing for 0xA0000..0xBFFFF (128KB)
0x20000..          VBE linear framebuffer (LFB) base (graphics modes)
...                Optional: WDDM allocations in VRAM (if implemented)
```

### Legacy window aliasing

The legacy physical address window maps to VRAM like this:

```text
0xA0000..0xBFFFF  <->  VRAM[0x00000..0x1FFFF]
```

This ensures:

- Text mode writes to `0xB8000` affect `VRAM[0x18000]`
- BIOS “graphics” planar writes to `0xA0000` land in `VRAM[0x00000]` (even if planar rendering is not fully implemented)

### VBE LFB base address

VBE mode info must report `PhysBasePtr = VRAM_BASE + 0x20000` (aligned to 64KB).

Windows 7 boot graphics and installer UI will draw directly into this linear framebuffer.

### Relation to WDDM allocations

The recommended rule is:

- **Legacy VGA/VBE uses a fixed reserved subregion of VRAM** (`0x00000..`).
- **WDDM allocations may use the remaining VRAM space**, or may live in pinned guest RAM (system memory), depending on the design of AeroGPU-EMU-DEV-001.

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
    pub format: u32,              // 0=B8G8R8X8 (for now)
}
```

The update rule:

1. Writer populates all fields except `generation`
2. Writer publishes the update by incrementing `generation` last (release semantics)
3. Reader snapshots `generation`, reads fields, then re-reads `generation` to verify a consistent view

**Implementation note (recommended):** To prevent readers from observing a partially-updated
descriptor, implementations may temporarily mark `generation` as “busy” (e.g. by setting a high
bit) during an update. Readers should retry if `generation` is marked busy.

This makes scanout switching glitch-free without locks.

### Required WDDM scanout programming surface (BAR0)

To allow the WDDM KMD to take ownership of scanout (and to let the emulator switch the canvas over cleanly), AeroGPU must expose a minimal set of scanout registers in BAR0.

If a broader AeroGPU protocol already exists, this section defines the *minimum required semantics*:

| Offset | Name | Width | Description |
|--------|------|-------|-------------|
| 0x0000 | `SCANOUT_CTRL` | 32 | Bit 0: `ENABLE` (1 = scanout active). Bit 1: `COMMIT` (write 1 to request atomic apply). |
| 0x0004 | `SCANOUT_BASE_LO` | 32 | Scanout base guest physical address (low 32). |
| 0x0008 | `SCANOUT_BASE_HI` | 32 | Scanout base guest physical address (high 32). |
| 0x000C | `SCANOUT_PITCH_BYTES` | 32 | Bytes per scanline. |
| 0x0010 | `SCANOUT_WIDTH` | 32 | Visible width in pixels. |
| 0x0014 | `SCANOUT_HEIGHT` | 32 | Visible height in pixels. |
| 0x0018 | `SCANOUT_FORMAT` | 32 | Pixel format enum (at least `B8G8R8X8`). |

**Atomic update rule (recommended):**

1. Driver writes `*_BASE`, `PITCH`, `WIDTH`, `HEIGHT`, `FORMAT`
2. Driver writes `SCANOUT_CTRL.ENABLE=1` and then pulses `SCANOUT_CTRL.COMMIT=1`
3. Device updates `ScanoutState` in one step on COMMIT (ignoring partially-written state)

### When legacy VGA/VBE owns scanout

- At reset/power-on: `source = LegacyText`, base points at the legacy text buffer (or can be implicit)
- When BIOS/bootloader sets a VBE LFB mode: `source = LegacyVbeLfb`, base = `VRAM_BASE + 0x20000`, width/height/pitch filled

### When WDDM owns scanout

When the AeroGPU WDDM driver is loaded, it programs the AeroGPU scanout registers (BAR0). As soon as the device observes a valid “enable” transition, it updates `ScanoutState`:

- `source = Wddm`
- `base_paddr = value programmed by driver`
- `width/height/pitch/format = values programmed by driver`

From this point onward, **VGA/VBE writes do not affect the visible display**.

### Compatibility rule once WDDM is active

After the first successful WDDM scanout enable:

- Legacy VGA/VBE ports and memory windows may continue to accept reads/writes for compatibility.
- The emulator presentation must ignore legacy sources unless WDDM explicitly disables scanout or the VM resets.

This prevents legacy writes (e.g. an errant `INT 10h`) from stealing the primary display after the desktop is up.

---

## 6) Browser canvas presentation requirements

The emulator must render:

1. VGA text mode (from `0xB8000` backing) during POST/bootloader
2. VBE LFB modes for Windows 7 boot + installer graphics
3. WDDM scanout after the AeroGPU driver loads

Presentation pipeline requirements:

- Source selection is based solely on `ScanoutState`.
- Pixel conversion handles B8G8R8X8 → canvas RGBA.
- Switching sources is atomic and does not free/relocate the backing memory, preventing stale pointers.

---

## Acceptance checklist (manual)

In the emulator UI:

1. Power on and see BIOS POST text output.
2. Windows 7 boot graphics appears (not just blind boot).
3. Windows 7 installer UI appears in a VBE LFB mode.
4. After AeroGPU WDDM driver loads, the desktop continues rendering using the WDDM scanout path without flashing back to VGA/VBE.
