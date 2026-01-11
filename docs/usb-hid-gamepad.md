# USB HID gamepad + composite HID topology (Windows 7)

This document specifies **how Aero exposes keyboard + mouse + gamepad over USB HID**, and what a Windows 7 guest is expected to do with it.

It is complementary to [`docs/usb-hid.md`](./usb-hid.md), which focuses on browser-event → HID usage mapping for keyboard/mouse.

---

## Why the UHCI root hub is limited to 2 ports

The UHCI host controller interface exposes exactly two downstream ports via two port status/control registers:

- `PORTSC1` (`0x10`)
- `PORTSC2` (`0x12`)

Because Aero models the UHCI **root hub** directly through these registers (see `crates/emulator/src/io/usb/uhci/regs.rs` and `crates/emulator/src/io/usb/hub.rs`), a single UHCI controller instance naturally provides **2 ports**.

If we need more USB attach points later, the typical options are:

- Emulate an additional host controller (another UHCI function, EHCI, xHCI, …).
- Emulate a USB hub device behind one root port.

---

## Why a single USB composite HID device (kbd + mouse + gamepad)

We want the guest to see **three input devices at once**:

- HID keyboard (boot keyboard)
- HID mouse (boot mouse)
- HID gamepad (generic desktop “Game Pad”)

With only **2 root ports** on UHCI, exposing these as three separate USB devices would require adding a hub or another host controller.

Instead, we expose a **single USB composite device** with multiple HID interfaces:

- Requires only **one** downstream port
- Uses **Windows 7 in-box drivers** (no custom driver)
- Produces distinct device nodes for keyboard/mouse/gamepad, so Windows binds the correct HID “client” drivers (`kbdhid.sys`, `mouhid.sys`, `hidgame.sys`)

---

## Windows 7 in-box driver binding expectations

On a correctly described composite HID device, Windows 7 should bind:

### Composite parent

- `usbccgp.sys` — **USB Composite Generic Parent**

Windows creates one child PDO per interface on the composite device.

### HID transport + class (per interface)

- `hidusb.sys` — HID transport (USB miniport)
- `hidclass.sys` — HID class driver

### HID “client” drivers (chosen by top-level collection usage)

- Keyboard interface → `kbdhid.sys`
- Mouse interface → `mouhid.sys`
- Gamepad interface → `hidgame.sys` (exposed to user-mode primarily via **DirectInput**)

High-level stack (conceptual):

```text
USB composite device
  → usbccgp.sys
    → (Interface 0) hidusb.sys → hidclass.sys → kbdhid.sys
    → (Interface 1) hidusb.sys → hidclass.sys → mouhid.sys
    → (Interface 2) hidusb.sys → hidclass.sys → hidgame.sys
```

Notes:

- `kbdclass.sys`/`mouclass.sys` also appear above `kbdhid.sys`/`mouhid.sys`, but are omitted here because they are not specific to USB.
- If the device is *not* composite (or is incorrectly described), Windows may bind a single HID stack and you’ll only get one functional device.

---

## Composite configuration layout (interfaces + endpoints)

The composite device is **full-speed** and bus-powered.

### Endpoint 0 (control)

- EP0 max packet size: **64 bytes**

### Configuration 1

| Interface | Function | Class/Subclass/Protocol | Interrupt IN endpoint | `wMaxPacketSize` | `bInterval` |
|---:|---|---|---|---:|---:|
| 0 | Keyboard | HID / Boot / Keyboard (`0x03/0x01/0x01`) | `0x81` | 8 | 10 ms |
| 1 | Mouse | HID / Boot / Mouse (`0x03/0x01/0x02`) | `0x82` | 4 | 10 ms |
| 2 | Gamepad | HID / None / None (`0x03/0x00/0x00`) | `0x83` | 8 | 10 ms |

Rationale for this layout:

- Boot protocol on keyboard/mouse keeps early-boot behavior predictable (BIOS/WinPE style expectations).
- Separate interrupt IN endpoints let the guest poll each function independently.
- The gamepad endpoint size matches the gamepad report length (8 bytes; see below).

---

## Gamepad (HID) report descriptor

This is the **exact** report descriptor for the gamepad interface.

It defines:

- 16 digital buttons (`Button 1` … `Button 16`)
- 1 hat switch (4-bit) with **null state**
- 4 analog axes: `X`, `Y`, `Rx`, `Ry` (8-bit, signed, `-127..=127`)
- 1 byte of constant padding (to make the report 8 bytes total)

### Descriptor bytes

```text
05 01        Usage Page (Generic Desktop)
09 05        Usage (Game Pad)
A1 01        Collection (Application)

05 09        Usage Page (Button)
19 01        Usage Minimum (Button 1)
29 10        Usage Maximum (Button 16)
15 00        Logical Minimum (0)
25 01        Logical Maximum (1)
75 01        Report Size (1)
95 10        Report Count (16)
81 02        Input (Data,Var,Abs)

05 01        Usage Page (Generic Desktop)
09 39        Usage (Hat switch)
15 00        Logical Minimum (0)
25 07        Logical Maximum (7)
35 00        Physical Minimum (0)
46 3B 01     Physical Maximum (315)
65 14        Unit (Eng Rot: Angular Pos)
75 04        Report Size (4)
95 01        Report Count (1)
81 42        Input (Data,Var,Abs,Null)
65 00        Unit (None)
75 04        Report Size (4)
95 01        Report Count (1)
81 01        Input (Const,Array,Abs)   ; nibble padding

09 30        Usage (X)
09 31        Usage (Y)
09 33        Usage (Rx)
09 34        Usage (Ry)
15 81        Logical Minimum (-127)
25 7F        Logical Maximum (127)
75 08        Report Size (8)
95 04        Report Count (4)
81 02        Input (Data,Var,Abs)

75 08        Report Size (8)
95 01        Report Count (1)
81 01        Input (Const,Array,Abs)   ; byte padding

C0           End Collection
```

---

## Gamepad input report format (8 bytes)

The gamepad interface sends one **8-byte input report** on its interrupt IN endpoint.

### Byte/bit layout

| Byte | Bits | Field | Meaning / range |
|---:|---|---|---|
| 0 | 0..7 | Buttons 1–8 | bit set = pressed |
| 1 | 0..7 | Buttons 9–16 | bit set = pressed |
| 2 | 0..3 | Hat switch | `0..=7` direction, `8` = neutral (null state) |
| 2 | 4..7 | Padding | always `0` |
| 3 | 0..7 | X | `i8`, `-127..=127` (negative=left, positive=right) |
| 4 | 0..7 | Y | `i8`, `-127..=127` (negative=up, positive=down) |
| 5 | 0..7 | Rx | `i8`, `-127..=127` (negative=left, positive=right) |
| 6 | 0..7 | Ry | `i8`, `-127..=127` (negative=up, positive=down) |
| 7 | 0..7 | Padding | always `0` |

When mapping from the browser Gamepad API (standard mapping), the intended axis mapping is:

- `axes[0]` → X
- `axes[1]` → Y
- `axes[2]` → Rx
- `axes[3]` → Ry

Convert the Gamepad API float range `[-1.0, 1.0]` to the report `i8` range by:

```text
i8_value = clamp(round(axis * 127.0), -127, 127)
```

### Hat switch encoding (HID standard)

The hat switch represents 8 directions in 45° steps:

| Value | Direction |
|---:|---|
| 0 | Up |
| 1 | Up-Right |
| 2 | Right |
| 3 | Down-Right |
| 4 | Down |
| 5 | Down-Left |
| 6 | Left |
| 7 | Up-Left |
| 8 | Neutral (**null state**) |

Important: because the descriptor sets **Null State** (`Input …,Null`), the neutral state is encoded as **8**, not `0xF`.

### Button numbering (host-side mapping contract)

Buttons are intentionally left “generic” at the HID layer (Windows will expose “Button 1”, …).
For deterministic mapping from the browser Gamepad API (when `gamepad.mapping === "standard"`), Aero should map:

| HID button | Browser Gamepad API index (standard mapping) | Typical label |
|---:|---:|---|
| 1 | 0 | A / Cross |
| 2 | 1 | B / Circle |
| 3 | 2 | X / Square |
| 4 | 3 | Y / Triangle |
| 5 | 4 | LB |
| 6 | 5 | RB |
| 7 | 6 | LT (digital threshold) |
| 8 | 7 | RT (digital threshold) |
| 9 | 8 | Back / Select |
| 10 | 9 | Start |
| 11 | 10 | Left stick press |
| 12 | 11 | Right stick press |
| 13 | 16 | Guide / Home |
| 14–16 | (unused) | reserved |

The D-pad is mapped to the **hat** using buttons `12..=15` (Up/Down/Left/Right) from the standard mapping.

---

## DirectInput vs XInput (what games will see)

- This is a **HID gamepad**, so Windows will expose it primarily via **DirectInput**.
- Many modern PC games only read **XInput** (Xbox 360+ controllers) and may ignore DirectInput devices.

Future option (not part of this HID spec):

- Add an alternate “Xbox 360 compatible” device mode (XUSB) to get in-box XInput support on Windows 7.

---

## Troubleshooting (Windows 7 Device Manager)

With the composite HID device attached, Device Manager should show roughly:

- **Universal Serial Bus controllers**
  - `USB Root Hub`
  - `USB Composite Device` (driver: `usbccgp.sys`)
- **Keyboards**
  - `HID Keyboard Device` (stack includes `hidusb.sys`, `hidclass.sys`, `kbdhid.sys`)
- **Mice and other pointing devices**
  - `HID-compliant mouse` (stack includes `hidusb.sys`, `hidclass.sys`, `mouhid.sys`)
- **Human Interface Devices**
  - `HID-compliant game controller` (stack includes `hidusb.sys`, `hidclass.sys`, `hidgame.sys`)

If you do not see three logical HID devices:

- Verify the USB device descriptor uses “per-interface” class (`bDeviceClass = 0`) and the configuration reports **3 interfaces**.
- Verify the gamepad interface report descriptor top-level collection is `Usage Page (Generic Desktop)`, `Usage (Game Pad)`.
