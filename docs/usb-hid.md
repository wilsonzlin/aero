# USB HID: Browser input → HID usages and reports

This project models input devices as **USB HID** (Human Interface Device) peripherals so that, once a USB controller (UHCI/EHCI/xHCI) exists, Windows can use native HID drivers rather than legacy PS/2 emulation.

This document is **separate** from PS/2 scancodes (see `docs/08-input-devices.md`), because USB HID uses **usages** (IDs from standardized tables), not scancodes.

The Rust-side mapping helpers live in `emulator::io::usb::hid::usage` (`crates/emulator/src/io/usb/hid/usage.rs`).

For USB HID **gamepad** details (composite device layout, Windows 7 driver binding expectations, and the exact gamepad report descriptor/report bytes), see
[`docs/usb-hid-gamepad.md`](./usb-hid-gamepad.md).

---

## Keyboard: `KeyboardEvent.code` → HID Usage (Keyboard/Keypad page 0x07)

### Why `code` (not `key`)

- Use `KeyboardEvent.code` because it represents the **physical key position** ("KeyA", "Digit1", …) and is stable across keyboard layouts.
- `KeyboardEvent.key` is the produced character, which depends on layout and modifiers, and is not what USB HID reports.

### Modifier keys

USB HID keyboard modifiers are special usages `0xE0..=0xE7` which map to a bitfield in the keyboard report:

| `KeyboardEvent.code` | Usage | Modifier bit |
| --- | --- | --- |
| `ControlLeft` | `0xE0` | `1<<0` |
| `ShiftLeft` | `0xE1` | `1<<1` |
| `AltLeft` | `0xE2` | `1<<2` |
| `MetaLeft` | `0xE3` | `1<<3` |
| `ControlRight` | `0xE4` | `1<<4` |
| `ShiftRight` | `0xE5` | `1<<5` |
| `AltRight` | `0xE6` | `1<<6` |
| `MetaRight` | `0xE7` | `1<<7` |

### Common key usages (subset)

USB HID usages for letters and digits are fixed (independent of layout):

| `KeyboardEvent.code` | Usage |
| --- | --- |
| `KeyA`..`KeyZ` | `0x04`..`0x1D` |
| `Digit1`..`Digit0` | `0x1E`..`0x27` |
| `Enter` | `0x28` |
| `Escape` | `0x29` |
| `Backspace` | `0x2A` |
| `Tab` | `0x2B` |
| `Space` | `0x2C` |
| `Minus` | `0x2D` |
| `Equal` | `0x2E` |
| `BracketLeft` | `0x2F` |
| `BracketRight` | `0x30` |
| `Backslash` | `0x31` |
| `Semicolon` | `0x33` |
| `Quote` | `0x34` |
| `Backquote` | `0x35` |
| `Comma` | `0x36` |
| `Period` | `0x37` |
| `Slash` | `0x38` |
| `CapsLock` | `0x39` |
| `F1`..`F12` | `0x3A`..`0x45` |
| `PrintScreen` | `0x46` |
| `ScrollLock` | `0x47` |
| `Pause` | `0x48` |
| `Insert` | `0x49` |
| `Home` | `0x4A` |
| `PageUp` | `0x4B` |
| `Delete` | `0x4C` |
| `End` | `0x4D` |
| `PageDown` | `0x4E` |
| `ArrowRight` | `0x4F` |
| `ArrowLeft` | `0x50` |
| `ArrowDown` | `0x51` |
| `ArrowUp` | `0x52` |

### Report model (boot keyboard)

The modeled keyboard uses the standard 8-byte boot keyboard input report:

```
Byte 0: modifier bits (Ctrl/Shift/Alt/GUI)
Byte 1: reserved (0)
Byte 2..7: up to 6 concurrently pressed non-modifier key usages
```

Notes:

- The implementation keeps a stable ordering of pressed keys (in press order) to avoid spurious “key up/down” events in simplistic host stacks.
- If more than 6 non-modifier keys are held, the report uses the HID `ErrorRollOver` code (`0x01`) in all 6 slots.

---

## Mouse: browser mouse events → HID usages and reports

### Buttons (`MouseEvent.buttons`)

In browsers, `MouseEvent.buttons` is a bitfield:

- `1` = left
- `2` = right
- `4` = middle
- `8` = back
- `16` = forward

The modeled mouse report supports 3 buttons (left/right/middle) and encodes them as:

| HID button | Bit |
| --- | --- |
| Button 1 (left) | `1<<0` |
| Button 2 (right) | `1<<1` |
| Button 3 (middle) | `1<<2` |

(Back/forward can be added later by expanding the report descriptor and report format.)

### Movement (`PointerLock` + `MouseEvent.movementX/Y`)

Use Pointer Lock so that `movementX` / `movementY` are **relative deltas** rather than absolute coordinates.

- HID X/Y are signed 8-bit relative values (`-127..=127`).
- Large deltas should be split across multiple reports (the device model does this internally).
- Sign convention:
  - `movementX > 0` → HID X positive (move right)
  - `movementY > 0` → HID Y positive (move down)

### Wheel (`WheelEvent.deltaY`)

HID wheel (`Usage 0x38`) is also a signed 8-bit relative value.

Browser wheel events typically use:

- `deltaY > 0` for scrolling down (wheel “towards the user”)
- `deltaY < 0` for scrolling up

For a conventional HID mouse, scroll **up** is usually represented as a positive wheel step in guest OS input APIs.
When wiring browser events to the device model, invert as needed:

```
hid_wheel_step = -WheelEvent.deltaY.signum()
```

### Report model

The modeled mouse uses a 4-byte report in **HID report protocol**:

```
Byte 0: buttons (bits 0..2), remaining bits padding
Byte 1: X delta (i8)
Byte 2: Y delta (i8)
Byte 3: wheel delta (i8)
```

It also supports **HID boot protocol** (host-selectable via `SET_PROTOCOL`), which omits the wheel byte:

```
Byte 0: buttons
Byte 1: X delta
Byte 2: Y delta
```

---

## Gamepad: Gamepad API → HID report

The emulator models a simple USB HID **Game Pad** device (`Usage 0x05` on the
Generic Desktop page) with:

- 16 digital buttons (HID usages Button 1..16)
- 1 hat switch (d-pad)
- 4 analog axes: X, Y, Rx, Ry (`int8`, `-127..=127`)

### Report model (8 bytes)

The modeled gamepad uses a fixed 8-byte input report (no report ID):

```
Byte 0..1: Buttons bitfield (u16 little-endian)
Byte 2:    Hat switch (low 4 bits). 0=Up, 1=Up-Right, … 7=Up-Left. 8 = neutral/null.
Byte 3:    X  (int8)
Byte 4:    Y  (int8)
Byte 5:    Rx (int8)
Byte 6:    Ry (int8)
Byte 7:    Padding (0)
```

Notes:

- The hat switch uses HID “Null state” (`Input (… Null)`) so the centered value
  is represented by `8` (outside the logical range `0..=7`).
- The canonical device implementation is `crates/emulator/src/io/usb/hid/gamepad.rs`;
  host capture should match that layout exactly.
