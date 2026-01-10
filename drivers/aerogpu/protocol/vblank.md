# AeroGPU protocol addendum: vblank / present timing primitives (Win7 WDDM 1.1)

This file specifies the **minimum device-visible primitives** AeroGPU should expose so that a Windows 7 WDDM 1.1 miniport driver can:

* deliver vblank interrupts (`D3DKMTWaitForVerticalBlankEvent`), and
* provide monotonic vblank counters/timestamps (useful for `GetScanLine`, diagnostics, and sanity checks).

It intentionally does **not** specify the rendering command protocol.

This addendum is intended to extend the existing BAR0 layout in:

* `drivers/aerogpu/protocol/aerogpu_pci.h`

See also: `docs/graphics/win7-vblank-present-requirements.md`.

---

## Design goals

* Minimal surface area (MVP-friendly).
* Works with the existing **level-triggered** interrupt model (IRQ status/enable/W1C-ack).
* Single scanout source first; multi-source extension should be mechanical.

---

## Feature discovery

Add a feature bit indicating that the vblank timing block is present:

* `AEROGPU_FEATURE_VBLANK` (FEATURES_LO bit 3)

When `AEROGPU_FEATURE_VBLANK` is set, the device **must** implement:

* `AEROGPU_IRQ_SCANOUT_VBLANK` in the IRQ registers, and
* the `SCANOUT0_VBLANK_*` timing registers described below.

---

## IRQ bit (vblank event)

The base protocol already defines an IRQ cause bit:

* `AEROGPU_IRQ_SCANOUT_VBLANK` (IRQ bit 1): vblank tick for scanout 0

Rules:

* The device increments vblank counters/timestamps every vblank tick (independent of IRQ enable).
* When `AEROGPU_IRQ_SCANOUT_VBLANK` is enabled in `AEROGPU_MMIO_REG_IRQ_ENABLE`, the device sets the corresponding `IRQ_STATUS` bit once per tick.
* Coalescing is allowed: if the bit is already pending, the device may keep it set (single pending bit) while counters continue to advance.

> Rationale for “only latch when enabled”: if vblank status bits accumulate while masked, re-enabling can cause an immediate stale interrupt and break `WaitForVerticalBlankEvent` pacing.

---

## MMIO registers

### Existing interrupt registers (already in `aerogpu_pci.h`)

| Register | Access | Description |
| --- | --- | --- |
| `AEROGPU_MMIO_REG_IRQ_STATUS` | RO | Pending IRQ cause bits (includes `AEROGPU_IRQ_SCANOUT_VBLANK`). |
| `AEROGPU_MMIO_REG_IRQ_ENABLE` | RW | Enable mask. Interrupt line is asserted when `(IRQ_STATUS & IRQ_ENABLE) != 0`. |
| `AEROGPU_MMIO_REG_IRQ_ACK` | WO | Write-1-to-clear (W1C) for `IRQ_STATUS` bits. |

### New: scanout0 vblank timing registers (proposed)

All fields are little-endian.

| Register | Access | Description |
| --- | --- | --- |
| `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO` | RO | Lower 32 bits of `vblank_seq` (increments once per vblank). |
| `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI` | RO | Upper 32 bits of `vblank_seq`. |
| `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO` | RO | Lower 32 bits of `last_vblank_time_ns`. |
| `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI` | RO | Upper 32 bits of `last_vblank_time_ns`. |
| `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS` | RO | Nominal vblank period in nanoseconds (default 16_666_667 for 60 Hz). |

`vblank_seq` rules:

* starts at 0 (or 1; either is fine as long as it is monotonic)
* increments by 1 per vblank tick
* must never go backwards

`last_vblank_time_ns` rules:

* monotonic, in nanoseconds since “device boot” (stable epoch chosen by device)
* updated at each vblank tick
* must never go backwards

---

## Multi-source extension (future)

To support multiple VidPn sources:

* replicate `SCANOUTn_VBLANK_*` register sets per scanout
* either:
  * allocate additional IRQ cause bits (`AEROGPU_IRQ_SCANOUT1_VBLANK`, ...), or
  * keep one vblank cause bit and require the driver to read per-scanout seq counters to disambiguate (less preferred).
