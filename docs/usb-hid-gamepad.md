# USB HID Gamepad + Composite HID Topology

This document describes two related pieces of the USB input story:

1. The **USB HID gamepad report format** used by the emulator.
2. The **USB composite HID device** topology used to present **keyboard + mouse + gamepad**
   while consuming only **one UHCI root hub port**.

> Source of truth: [ADR 0015](./adr/0015-canonical-usb-stack.md) defines the canonical USB
> stack for the browser runtime (`crates/aero-usb` + `web/` host integration). This document focuses
> on the HID composite/report contract on top of that stack.

---

## Why a composite HID device?

The UHCI root hub model exposes only two downstream ports (`PORTSC1`/`PORTSC2`). A naïve
approach (separate USB devices for keyboard, mouse, and gamepad) would require three
ports or an external USB hub.

The emulator provides a **single USB device** with multiple HID interfaces so the
default input stack (keyboard + mouse + gamepad) consumes only one port and leaves
the other root port free (e.g. for WebHID/WebUSB passthrough devices, potentially
behind an external USB hub device).

- Windows 7 enumerates the device as a composite device (`usbccgp.sys` parent PDO)
- Each HID interface binds via the in-box HID stack (`hidusb.sys` + `hidclass.sys`)
  and then the appropriate client driver (e.g. `kbdhid.sys` / `mouhid.sys`)

Reference implementation (browser/WASM): `aero_usb::hid::composite::UsbCompositeHidInput`
(`crates/aero-usb/src/hid/composite.rs`).

---

## Composite device interface layout

The composite device exposes **one configuration** with **three interfaces**:

| Interface | Function  | Class/Subclass/Protocol            | Interrupt IN EP | Max packet |
|----------:|-----------|------------------------------------|-----------------|-----------:|
| 0         | Keyboard  | HID / Boot (1) / Keyboard (1)      | `0x81`          | 8 bytes    |
| 1         | Mouse     | HID / Boot (1) / Mouse (2)         | `0x82`          | 4 bytes    |
| 2         | Gamepad   | HID / None (0) / None (0)          | `0x83`          | 8 bytes    |

Each interface includes:

- An interface descriptor
- A HID descriptor (`0x21`) that references that interface’s report descriptor length
- An interrupt IN endpoint descriptor

---

## Gamepad report format

The emulator models a USB HID **Game Pad** top-level collection (`Usage 0x05` on the
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
- The canonical report struct is `aero_usb::hid::GamepadReport`
  (`crates/aero-usb/src/hid/gamepad.rs`).
- The browser-side pack/unpack helpers live in `web/src/input/gamepad.ts`.

### Keeping report packing consistent (Rust ↔ TypeScript)

There are two independent implementations of the 8-byte gamepad report packing:

- Rust: `crates/aero-usb/src/hid/gamepad.rs::GamepadReport::to_bytes`
- TypeScript: `web/src/input/gamepad.ts::packGamepadReport` + `unpackGamepadReport`

To prevent drift between them, we keep a shared fixture of report field values
and their expected packed bytes at:

- `docs/fixtures/hid_gamepad_report_vectors.json`
- `docs/fixtures/hid_gamepad_report_clamping_vectors.json` (includes out-of-range inputs to pin down clamping/masking semantics)

Both sides validate their packing logic against this fixture:

- Rust: `crates/aero-usb/tests/hid_gamepad_report_fixture.rs`
- Rust (clamping): `crates/aero-usb/tests/hid_gamepad_report_clamping_fixture.rs`
- TypeScript: `web/src/input/gamepad.test.ts`

### Button bitfield mapping (browser host)

When capturing a controller via the browser **Gamepad API** using the **standard mapping**
(`Gamepad.mapping === "standard"`), the host maps Gamepad button indices into the 16-bit
button bitfield as follows:

| Bit | Gamepad button index | Meaning |
| --- | --- | --- |
| 0 | 0 | A / Cross |
| 1 | 1 | B / Circle |
| 2 | 2 | X / Square |
| 3 | 3 | Y / Triangle |
| 4 | 4 | LB / L1 |
| 5 | 5 | RB / R1 |
| 6 | 6 | LT / L2 (digital `pressed`) |
| 7 | 7 | RT / R2 (digital `pressed`) |
| 8 | 8 | Back / Select |
| 9 | 9 | Start |
| 10 | 10 | Left stick press |
| 11 | 11 | Right stick press |
| 12 | 16 | Guide / Home |
| 13 | 17 | Extra (if present) |
| 14 | 18 | Extra (if present) |
| 15 | 19 | Extra (if present) |

The d-pad quartet (`buttons[12..15]`) is converted into the hat value and is not included
in the bitfield.
