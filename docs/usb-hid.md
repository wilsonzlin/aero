# USB HID: Browser input → HID usages and reports

This project models input devices as **USB HID** (Human Interface Device) peripherals so that, once a USB controller (UHCI/EHCI/xHCI) exists, Windows can use native HID drivers rather than legacy PS/2 emulation.

This document is **separate** from PS/2 scancodes (see `docs/08-input-devices.md`), because USB HID uses **usages** (IDs from standardized tables), not scancodes.

For xHCI (USB 3.x) host controller details and current limitations, see [`docs/usb-xhci.md`](./usb-xhci.md).

> Source of truth: [ADR 0015](./adr/0015-canonical-usb-stack.md) defines the canonical USB
> stack for the browser runtime (`crates/aero-usb` + `web/` host integration). This document focuses
> on HID usages and report formats on top of that stack.

For controller-level design/contract notes:

- EHCI (USB 2.0): [`docs/usb-ehci.md`](./usb-ehci.md)
- xHCI (USB 3.x): [`docs/usb-xhci.md`](./usb-xhci.md)

The Rust-side usage mapping helpers live in `aero_usb::hid::usage` (`crates/aero-usb/src/hid/usage.rs`).
Browser-oriented convenience wrappers live in `aero_usb::web` (`crates/aero-usb/src/web.rs`) (e.g.
`keyboard_code_to_hid_usage`, `mouse_button_to_hid_mask`).
The emulator re-exports these helpers at `emulator::io::usb::hid::usage`.
The browser-side `KeyboardEvent.code -> HID usage` mapping lives in `web/src/input/hid_usage.ts`.

For USB HID **gamepad** details (Windows 7 driver binding expectations, and the exact gamepad report descriptor/report bytes), see
[`docs/usb-hid-gamepad.md`](./usb-hid-gamepad.md). The Rust↔TypeScript report packing contract is pinned by
`docs/fixtures/hid_gamepad_report_vectors.json` (plus the clamping-focused
`docs/fixtures/hid_gamepad_report_clamping_vectors.json`) and validated by tests on both sides.

For WebHID passthrough (synthesizing HID report descriptors from WebHID metadata
because browsers do not expose raw report descriptor bytes), see
[`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md).

For the end-to-end “real device” passthrough architecture and security model
(main thread owns the handle; worker models a guest-visible USB controller + a generic HID device), see
[`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md).

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
| `IntlHash` | `0x32` |
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
| `IntlBackslash` | `0x64` |
| `NumpadEqual` | `0x67` |
| `NumpadComma` | `0x85` |
| `IntlRo` | `0x87` |
| `IntlYen` | `0x89` |

### Keeping the mapping consistent (Rust ↔ TypeScript)

There are two independent implementations of `KeyboardEvent.code → HID usage`:

- Rust: `crates/aero-usb/src/hid/usage.rs::keyboard_code_to_usage`
- TypeScript: `web/src/input/hid_usage.ts::keyboardCodeToHidUsage`

To prevent drift between them, we keep a shared fixture of supported mappings (including full
alphanumeric ranges like `KeyA..KeyZ`, `Digit0..Digit9`, and `F1..F12`) at:

- `docs/fixtures/hid_usage_keyboard.json`

Both sides have unit tests that validate their mapping function against that fixture:

- Rust: `crates/aero-usb/tests/hid_usage_keyboard_fixture.rs`
- TypeScript: `web/src/input/hid_usage.test.ts`

When adding support for a new key code:

1. Add it to `docs/fixtures/hid_usage_keyboard.json` (as `code` + expected usage).
2. Update **both** mapping functions.
3. Run `cargo xtask input` (recommended) or run the equivalent commands manually:
   - `cargo xtask input` runs the HID usage fixture tests as part of its focused `aero-usb` subset.
     Use `cargo xtask input --usb-all` if you want the full USB integration suite.
   - Manual Rust equivalent (focused):
     - `cargo test -p aero-usb --locked --test hid_usage_keyboard_fixture --test hid_usage_consumer_fixture`
     - (or, to run everything: `cargo test -p aero-usb --locked`)
   - `npm -w web run test:unit -- src/input`

   If you don't have Node deps available (e.g. a constrained sandbox), you can still validate the
   Rust side with `cargo xtask input --rust-only` (but it won't run the TypeScript fixture tests).

(The mapping is still not intended to be exhaustive, but the fixture is intentionally thorough so a
change on either side requires updating the shared list.)

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

## Consumer control (media keys): `KeyboardEvent.code` → HID Usage (Consumer page 0x0C)

Some “media keys” are not part of the Keyboard/Keypad usage page (`0x07`). They live on the HID
**Consumer** usage page (`0x0C`) and are exposed by browsers as `KeyboardEvent.code` values like:

- `AudioVolumeUp`
- `AudioVolumeDown`
- `AudioVolumeMute`
- `MediaPlayPause`
- `MediaStop`
- `MediaTrackNext`
- `MediaTrackPrevious`

Aero models these inputs using a dedicated USB HID **consumer-control** device model:

- Rust: `crates/aero-usb/src/hid/consumer_control.rs` (`UsbHidConsumerControl`)
  - Interrupt IN report format: **2 bytes**, little-endian `u16` usage ID (`0` = none pressed)

Mapping helpers (keep in sync):

- Rust: `crates/aero-usb/src/hid/usage.rs::keyboard_code_to_consumer_usage`
- Rust (browser convenience wrapper): `crates/aero-usb/src/web.rs::keyboard_code_to_consumer_usage`
- TypeScript: `web/src/input/hid_usage.ts::keyboardCodeToConsumerUsage`

To prevent drift, the supported mapping set is pinned by:

- `docs/fixtures/hid_usage_consumer.json`

…and validated by tests on both sides:

- Rust: `crates/aero-usb/tests/hid_usage_consumer_fixture.rs`
- TypeScript: `web/src/input/hid_usage.test.ts`

---

## Mouse: browser mouse events → HID usages and reports

### Buttons (`MouseEvent.buttons`)

In browsers, `MouseEvent.buttons` is a bitfield:

- `1` = left
- `2` = right
- `4` = middle
- `8` = back
- `16` = forward

The modeled mouse report supports 5 buttons (left/right/middle/back/forward) and encodes them as:

| HID button | Bit | Typical meaning |
| --- | --- | --- |
| Button 1 | `1<<0` | left |
| Button 2 | `1<<1` | right |
| Button 3 | `1<<2` | middle |
| Button 4 | `1<<3` | back / side |
| Button 5 | `1<<4` | forward / extra |

Note: in **HID boot protocol**, the standard boot mouse format only defines 3 buttons; Aero masks
buttons 4/5 to zero when emitting boot-protocol reports.

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

### Horizontal wheel (`WheelEvent.deltaX`)

Many mice and trackpads can also generate **horizontal scroll** events, which are exposed by browsers
as `WheelEvent.deltaX`.

In Aero's synthetic USB mouse, horizontal scroll is modeled using the HID **AC Pan** usage (Consumer
page `0x0C`, usage `0x0238`), encoded as a signed 8-bit relative value in the **report protocol**
input report.

- `deltaX > 0` (scroll right) should typically map to a **positive** AC Pan value.
- Unlike `deltaY`, horizontal scroll usually does **not** need sign inversion:

```
hid_hwheel_step = WheelEvent.deltaX.signum()
```

When both `deltaY` and `deltaX` are present (e.g. trackpads), Aero can emit **one** HID mouse report
that contains both the vertical wheel byte and the horizontal wheel (AC Pan) byte. The web runtime
uses a combined `mouse_wheel2(wheel, hwheel)` injection path when available to preserve this
“diagonal scroll in one frame” behavior.

### Report model

The modeled mouse uses a 5-byte report in **HID report protocol**:

```
Byte 0: buttons (bits 0..4), remaining bits padding
Byte 1: X delta (i8)
Byte 2: Y delta (i8)
Byte 3: wheel delta (i8)
Byte 4: horizontal wheel delta / AC Pan (i8)
```

It also supports **HID boot protocol** (host-selectable via `SET_PROTOCOL`), which omits the wheel
bytes:

```
Byte 0: buttons
Byte 1: X delta
Byte 2: Y delta
```

Note: because boot protocol reports cannot carry wheel/hwheel deltas, scroll input is ignored while
the guest has selected boot protocol (though it still counts as user activity for remote-wakeup
purposes).

---

## HID report descriptor synthesis notes (WebHID)

When synthesizing HID report descriptors from WebHID metadata, be aware that **Unit Exponent**
(global item `0x55`) is defined by HID 1.11 as a **4-bit signed value** (`-8..=7`) stored in the
low nibble of a *single* byte.

- High nibble is reserved and must be `0`.
- Examples:
  - `unitExponent = -1` → `0x55 0x0F` (not `0x55 0xFF`)
  - `unitExponent = -2` → `0x55 0x0E`

Also note that Aero currently caps WebHID passthrough **input reports** to fit in a single USB
**full-speed interrupt** packet (**64 bytes**, including the optional report ID prefix). We
currently do not support splitting a single HID input report across multiple interrupt packets, so
input reports larger than 64 bytes are rejected during normalization/descriptor synthesis.
