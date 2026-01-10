# AeroGPU protocol addendum: vblank / present timing primitives (Win7 WDDM 1.1)

This file proposes the **minimum device-visible primitives** AeroGPU should expose so that a Windows 7 WDDM 1.1 miniport driver can:

* notify vblank interrupts (`D3DKMTWaitForVerticalBlankEvent`)
* provide a monotonic notion of “frame number” and “last vblank time” (useful for `GetScanLine` and diagnostics)

It intentionally does **not** specify the rendering command protocol.

See also: `docs/graphics/win7-vblank-present-requirements.md`.

---

## Design goals

* Extremely small surface area (MVP-friendly).
* Works with a **level-triggered interrupt** model (typical PCI INTx/MSI).
* Supports a single scanout source initially (multi-source extension is mechanical).

---

## IRQ bits

Define an IRQ cause bit for “vblank occurred”:

* `IRQ_VBLANK0` (bit 0): vblank event for VidPn source 0

Rules:

* The device sets `IRQ_VBLANK0` once per vblank tick.
* If `IRQ_VBLANK0` is masked/disabled, the bit may still be latched in `IRQ_STATUS` but **must not** assert the interrupt line.
* If a new vblank occurs while `IRQ_VBLANK0` is already pending, the device **may coalesce** into a single pending bit; the vblank sequence counter continues to advance.

---

## Required registers (MMIO)

This addendum is agnostic to the rest of AeroGPU’s register map. The offsets below are **suggested**; if a register block already exists, place these fields there.

All registers are little-endian.

### Interrupt registers

| Offset | Name          | Access | Description |
| ------ | ------------- | ------ | ----------- |
| 0x0000 | `IRQ_STATUS`  | RO     | Pending IRQ causes (bitset). Includes `IRQ_VBLANK0`. |
| 0x0004 | `IRQ_ENABLE`  | RW     | IRQ enable mask (bitset). Interrupt line is asserted when `(IRQ_STATUS & IRQ_ENABLE) != 0`. |
| 0x0008 | `IRQ_ACK`     | WO     | Write-1-to-clear (W1C) for `IRQ_STATUS` bits. Writing bit `n` clears `IRQ_STATUS[n]`. |

Semantics:
* Level-triggered: interrupt line is asserted while any enabled cause bit is pending.
* `IRQ_ACK` must be safe to write from the ISR (no long stalls).

### Vblank timing registers (source 0)

| Offset | Name                   | Access | Description |
| ------ | ---------------------- | ------ | ----------- |
| 0x0010 | `VBLANK0_SEQ_LO`       | RO     | Lower 32 bits of `vblank_seq` (increments once per vblank). |
| 0x0014 | `VBLANK0_SEQ_HI`       | RO     | Upper 32 bits of `vblank_seq`. |
| 0x0018 | `VBLANK0_TIME_NS_LO`   | RO     | Lower 32 bits of `last_vblank_time_ns`. |
| 0x001C | `VBLANK0_TIME_NS_HI`   | RO     | Upper 32 bits of `last_vblank_time_ns`. |

`vblank_seq` rules:
* starts at 0 (or 1; either is fine as long as it is monotonic)
* increments by 1 per vblank tick
* must never go backwards

`last_vblank_time_ns` rules:
* monotonic, in nanoseconds since “device boot” (any stable epoch is acceptable)
* updated at each vblank tick
* must never go backwards

### Optional: vblank period configuration

| Offset | Name                 | Access | Description |
| ------ | -------------------- | ------ | ----------- |
| 0x0020 | `VBLANK0_PERIOD_NS`  | RW     | Nominal vblank period in nanoseconds (default 16_666_667). |

Notes:
* For Win7 MVP, a fixed 60 Hz device is acceptable; this register is optional.
* If writable, changes apply on the next tick and must not make counters/timestamps go backwards.

---

## Multi-source extension (future)

To support multiple VidPn sources:
* replicate `VBLANKn_*` register sets per source
* allocate additional IRQ bits: `IRQ_VBLANK1`, `IRQ_VBLANK2`, ...

