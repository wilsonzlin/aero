# 08 - Input Device Emulation

## Overview

Windows 7 requires keyboard and mouse input. The baseline approach is to capture browser events and translate them into **legacy PS/2** (works out-of-the-box), or **USB HID** (also inbox, but requires a much larger emulation surface).

### WASM / browser integration: canonical `Machine` input injection

For JS/WASM-side input testing and future in-browser integration, the canonical full-system VM (`aero_machine::Machine`) is exported to JS via `crates/aero-wasm::Machine`.

The WASM-facing wrapper exposes input injection methods that map directly to PS/2 (i8042):

- Keyboard: `Machine.inject_browser_key(code, pressed)` where `code` is DOM `KeyboardEvent.code`.
- Mouse motion + wheel: `Machine.inject_mouse_motion(dx, dy, wheel)`
  - `dx`/`dy` use browser-style coordinates: +X is right, +Y is down.
  - `wheel` uses PS/2 convention: positive is wheel up.
- Mouse buttons:
  - `Machine.inject_mouse_button(button, pressed)` uses DOM `MouseEvent.button` mapping:
    - `0`: left, `1`: middle, `2`: right (other values ignored)
  - `Machine.inject_mouse_buttons_mask(mask)` uses DOM `MouseEvent.buttons` bitmask:
    - bit0 (`0x01`): left, bit1 (`0x02`): right, bit2 (`0x04`): middle (higher bits ignored)
  - Convenience helpers also exist: `inject_mouse_left/right/middle(pressed)`.
  - For ergonomics, `crates/aero-wasm` also exports enums that mirror these DOM mappings:
    - `MouseButton` (`Left=0`, `Middle=1`, `Right=2`)
    - `MouseButtons` bit values (`Left=1`, `Right=2`, `Middle=4`) which can be OR'd into a mask

Example:

```ts
// `initWasm` is the browser/worker WASM loader in `web/src/runtime/wasm_loader.ts`.
const { api } = await initWasm({ variant: "single" });
const machine = new api.Machine(64 * 1024 * 1024);

machine.inject_browser_key("KeyA", true);
machine.inject_browser_key("KeyA", false);

machine.inject_mouse_motion(10, 5, 1); // dx=+10, dy=+5, wheel=+1 (up)
machine.inject_mouse_button(0, true); // left down
machine.inject_mouse_button(0, false); // left up
machine.inject_mouse_buttons_mask(0x01 | 0x02); // left+right held
machine.inject_mouse_buttons_mask(0x00); // release all
```

Note: the PS/2 mouse model only emits movement packets when mouse reporting is enabled (e.g. after
the guest sends `0xF4` via the i8042 “write to mouse” command). Most OS drivers enable this during
boot; very early bare-metal tests may need to do so explicitly.

For best performance and lowest complexity on the host side, we also plan a **paravirtualized virtio-input** path. This avoids USB controller emulation entirely, but requires a custom Windows 7 driver to surface the virtio device as standard HID keyboard/mouse devices. The definitive virtio-input device contract for Aero (transport + queues + event codes) is specified in [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md).

For a single end-to-end “do these steps” validation checklist (device model + Win7 driver + web runtime routing), see:

- [`virtio-input-test-plan.md`](./virtio-input-test-plan.md)

Physical device passthrough (optional): on Chromium-based browsers, Aero can also (optionally) attach a **real host-connected device** to the guest via WebHID/WebUSB. See:

- [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md)
- [`docs/webusb-passthrough.md`](./webusb-passthrough.md)

For the canonical USB stack selection for the browser runtime, see [ADR 0015](./adr/0015-canonical-usb-stack.md).

---

## Input Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Input Stack                                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Browser Events                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  KeyboardEvent, MouseEvent, PointerEvent, GamepadEvent   │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Input Capture Layer                                     │    │
│  │    - Pointer Lock API (mouse capture)                    │    │
│  │    - Keyboard event handling                             │    │
│  │    - Gamepad API                                         │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Translation + Routing                                   │    │
│  │    - Browser keyCode → PS/2 scancode (early boot)         │    │
│  │    - Browser keyCode → virtio-input events (fast path)    │    │
│  │    - Mouse movement → PS/2 packets or virtio-input REL_*  │    │
│  │    - USB HID report generation (optional)                 │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌────────────────────┐  ┌────────────────────┐  ┌─────────────┐│
│  │   PS/2 Controller  │  │   USB Controller   │  │ virtio-input ││
│  │   (i8042)          │  │   (UHCI/EHCI)      │  │ (kbd + mouse)││
│  └────────────────────┘  └────────────────────┘  └─────────────┘│
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## IRQ semantics (browser runtime)

Input devices ultimately notify the guest via IRQ lines (IRQ1/IRQ12 for PS/2,
PCI INTx for UHCI, etc). In the browser runtime these are delivered as
refcounted *line level* transitions (`raiseIrq` / `lowerIrq`). Edge-triggered
sources are represented as explicit pulses (0→1→0).

See [`docs/irq-semantics.md`](./irq-semantics.md) for the canonical contract and
guardrails (underflow/overflow behaviour, wire-OR semantics, and tests).

## Snapshot/Restore (Save States)

Input snapshots must preserve any **pending bytes** that the guest has not yet consumed, along with controller/device command state.

### What must be captured

- **USB (UHCI + HID devices)**
  - UHCI controller register state and per-port timers/flags
  - USB hub device state (per-port status/change bits and reset timers)
  - USB HID device state (address/config, endpoint halt bits, protocol/idle state)
  - For passthrough HID devices: pending **input report** queue (so restoring mid-session doesn’t drop buffered input)

- **i8042 controller**
  - status register and command byte
  - pending controller command (if awaiting a data byte)
  - output buffer contents (bytes queued for port `0x60`)
  - **IRQ behavior**: i8042 IRQ1/IRQ12 are edge-triggered in Aero’s model. The controller device
    should not snapshot an “IRQ level”; pending interrupts should be captured by the interrupt
    controller (PIC/APIC) state. On restore, the i8042 device must avoid emitting spurious IRQ
    pulses for already-buffered output bytes (see [`docs/irq-semantics.md`](./irq-semantics.md)).
- **PS/2 keyboard and mouse**
  - mode/configuration (scancode set, LEDs, sample rate, resolution, scaling)
  - command parsing state (e.g. “expecting data”)
  - device output queues

This ensures that a snapshot taken between a host keypress and guest consumption will restore deterministically.

## PS/2 Controller (i8042)

> IRQ signaling semantics (edge vs level) in the browser runtime are documented in
> [`docs/irq-semantics.md`](./irq-semantics.md).

### Controller Emulation

```rust
pub struct I8042Controller {
    // Status register
    status: u8,
    
    // Command byte
    command_byte: u8,
    
    // Output buffer
    output_buffer: u8,
    output_source: OutputSource,
    
    // Input buffer (for commands)
    input_buffer: u8,
    
    // Pending command
    pending_command: Option<u8>,
    
    // Devices
    keyboard: Ps2Keyboard,
    mouse: Ps2Mouse,
    
    // IRQ state
    keyboard_irq_pending: bool,
    mouse_irq_pending: bool,
}

// Status register bits
const STATUS_OBF: u8 = 0x01;   // Output Buffer Full
const STATUS_IBF: u8 = 0x02;   // Input Buffer Full
const STATUS_SYS: u8 = 0x04;   // System flag
const STATUS_A2: u8 = 0x08;    // Address line A2
const STATUS_INH: u8 = 0x10;   // Inhibit flag
const STATUS_MOBF: u8 = 0x20;  // Mouse Output Buffer Full
const STATUS_TOUT: u8 = 0x40;  // Timeout error
const STATUS_PERR: u8 = 0x80;  // Parity error

impl I8042Controller {
    pub fn read_port(&mut self, port: u16) -> u8 {
        match port {
            0x60 => {
                // Data port - read output buffer
                self.status &= !STATUS_OBF;
                self.output_buffer
            }
            0x64 => {
                // Status port
                self.status
            }
            _ => 0xFF,
        }
    }
    
    pub fn write_port(&mut self, port: u16, value: u8) {
        match port {
            0x60 => {
                // Data port
                if let Some(cmd) = self.pending_command.take() {
                    self.execute_controller_command_data(cmd, value);
                } else {
                    // Send to keyboard
                    self.send_to_keyboard(value);
                }
            }
            0x64 => {
                // Command port
                self.execute_controller_command(value);
            }
            _ => {}
        }
    }
    
    fn execute_controller_command(&mut self, cmd: u8) {
        match cmd {
            0x20 => {
                // Read command byte
                self.set_output(self.command_byte, OutputSource::Controller);
            }
            0x60 => {
                // Write command byte (next byte)
                self.pending_command = Some(cmd);
            }
            0xA7 => {
                // Disable mouse port
                self.command_byte |= 0x20;
            }
            0xA8 => {
                // Enable mouse port
                self.command_byte &= !0x20;
            }
            0xA9 => {
                // Test mouse port
                self.set_output(0x00, OutputSource::Controller);  // Pass
            }
            0xAA => {
                // Self test
                self.set_output(0x55, OutputSource::Controller);  // Pass
            }
            0xAB => {
                // Test keyboard port
                self.set_output(0x00, OutputSource::Controller);  // Pass
            }
            0xAD => {
                // Disable keyboard
                self.command_byte |= 0x10;
            }
            0xAE => {
                // Enable keyboard
                self.command_byte &= !0x10;
            }
            0xD4 => {
                // Write to mouse (next byte)
                self.pending_command = Some(cmd);
            }
            _ => {
                log::debug!("Unknown i8042 command: {:02x}", cmd);
            }
        }
    }
    
    fn set_output(&mut self, value: u8, source: OutputSource) {
        self.output_buffer = value;
        self.output_source = source;
        self.status |= STATUS_OBF;
        
        match source {
            OutputSource::Keyboard => {
                if self.command_byte & 0x01 != 0 {
                    self.keyboard_irq_pending = true;
                }
            }
            OutputSource::Mouse => {
                self.status |= STATUS_MOBF;
                if self.command_byte & 0x02 != 0 {
                    self.mouse_irq_pending = true;
                }
            }
            OutputSource::Controller => {}
        }
    }
}
```

### PS/2 Keyboard

```rust
pub struct Ps2Keyboard {
    // Scancode set (1, 2, or 3)
    scancode_set: u8,
    
    // LED state
    leds: u8,
    
    // Typematic settings
    typematic_delay: u8,
    typematic_rate: u8,
    
    // Output queue
    output_queue: VecDeque<u8>,
    
    // Command state
    expecting_data: bool,
    last_command: u8,
}

impl Ps2Keyboard {
    pub fn handle_command(&mut self, cmd: u8) -> Option<u8> {
        if self.expecting_data {
            self.expecting_data = false;
            return self.handle_command_data(cmd);
        }
        
        match cmd {
            0xED => {
                // Set LEDs (next byte contains LED state)
                self.expecting_data = true;
                self.last_command = cmd;
                Some(0xFA)  // ACK
            }
            0xEE => {
                // Echo
                Some(0xEE)
            }
            0xF0 => {
                // Get/Set scancode set
                self.expecting_data = true;
                self.last_command = cmd;
                Some(0xFA)
            }
            0xF2 => {
                // Identify keyboard
                self.output_queue.push_back(0xFA);
                self.output_queue.push_back(0xAB);
                self.output_queue.push_back(0x83);
                None
            }
            0xF3 => {
                // Set typematic rate/delay
                self.expecting_data = true;
                self.last_command = cmd;
                Some(0xFA)
            }
            0xF4 => {
                // Enable scanning
                Some(0xFA)
            }
            0xF5 => {
                // Disable scanning
                Some(0xFA)
            }
            0xF6 => {
                // Set default parameters
                self.scancode_set = 2;
                self.typematic_delay = 0x0B;
                self.typematic_rate = 0x0B;
                Some(0xFA)
            }
            0xFF => {
                // Reset
                self.output_queue.push_back(0xFA);
                self.output_queue.push_back(0xAA);  // Self-test passed
                None
            }
            _ => {
                log::debug!("Unknown keyboard command: {:02x}", cmd);
                Some(0xFA)
            }
        }
    }
    
    pub fn key_event(&mut self, scancode: u8, pressed: bool) {
        match self.scancode_set {
            1 => {
                // Set 1: break = make | 0x80
                if pressed {
                    self.output_queue.push_back(scancode);
                } else {
                    self.output_queue.push_back(scancode | 0x80);
                }
            }
            2 => {
                // Set 2: break = 0xF0, make
                if pressed {
                    self.output_queue.push_back(scancode);
                } else {
                    self.output_queue.push_back(0xF0);
                    self.output_queue.push_back(scancode);
                }
            }
            _ => {}
        }
    }
}
```

### PS/2 Mouse

```rust
pub struct Ps2Mouse {
    // Mouse mode
    mode: MouseMode,
    
    // Resolution (counts per mm)
    resolution: u8,
    
    // Sample rate
    sample_rate: u8,
    
    // Scaling
    scaling: Scaling,
    
    // Button state
    buttons: u8,
    
    // Accumulated movement
    dx: i32,
    dy: i32,
    dz: i32,  // Scroll wheel
    
    // Output queue
    output_queue: VecDeque<u8>,
    
    // Command state
    expecting_data: bool,
    last_command: u8,
}

#[derive(Clone, Copy, PartialEq)]
pub enum MouseMode {
    Stream,
    Remote,
    Wrap,
}

impl Ps2Mouse {
    pub fn handle_command(&mut self, cmd: u8) -> Option<u8> {
        if self.expecting_data {
            self.expecting_data = false;
            return self.handle_command_data(cmd);
        }
        
        match cmd {
            0xE6 => {
                // Set scaling 1:1
                self.scaling = Scaling::Linear;
                Some(0xFA)
            }
            0xE7 => {
                // Set scaling 2:1
                self.scaling = Scaling::Double;
                Some(0xFA)
            }
            0xE8 => {
                // Set resolution
                self.expecting_data = true;
                self.last_command = cmd;
                Some(0xFA)
            }
            0xE9 => {
                // Status request
                self.output_queue.push_back(0xFA);
                self.output_queue.push_back(self.get_status_byte());
                self.output_queue.push_back(self.resolution);
                self.output_queue.push_back(self.sample_rate);
                None
            }
            0xEA => {
                // Set stream mode
                self.mode = MouseMode::Stream;
                Some(0xFA)
            }
            0xEB => {
                // Read data (remote mode)
                self.send_movement_packet();
                Some(0xFA)
            }
            0xEC => {
                // Reset wrap mode
                Some(0xFA)
            }
            0xEE => {
                // Set wrap mode
                self.mode = MouseMode::Wrap;
                Some(0xFA)
            }
            0xF0 => {
                // Set remote mode
                self.mode = MouseMode::Remote;
                Some(0xFA)
            }
            0xF2 => {
                // Get device ID
                self.output_queue.push_back(0xFA);
                self.output_queue.push_back(self.get_device_id());
                None
            }
            0xF3 => {
                // Set sample rate
                self.expecting_data = true;
                self.last_command = cmd;
                Some(0xFA)
            }
            0xF4 => {
                // Enable data reporting
                Some(0xFA)
            }
            0xF5 => {
                // Disable data reporting
                Some(0xFA)
            }
            0xF6 => {
                // Set defaults
                self.resolution = 4;
                self.sample_rate = 100;
                self.scaling = Scaling::Linear;
                Some(0xFA)
            }
            0xFF => {
                // Reset
                self.output_queue.push_back(0xFA);
                self.output_queue.push_back(0xAA);
                self.output_queue.push_back(0x00);  // Device ID
                None
            }
            _ => {
                log::debug!("Unknown mouse command: {:02x}", cmd);
                Some(0xFA)
            }
        }
    }
    
    pub fn movement(&mut self, dx: i32, dy: i32, dz: i32) {
        self.dx += dx;
        self.dy += dy;
        self.dz += dz;
        
        if self.mode == MouseMode::Stream {
            self.send_movement_packet();
        }
    }
    
    pub fn button_event(&mut self, button: u8, pressed: bool) {
        if pressed {
            self.buttons |= button;
        } else {
            self.buttons &= !button;
        }
        
        if self.mode == MouseMode::Stream {
            self.send_movement_packet();
        }
    }
    
    fn send_movement_packet(&mut self) {
        // Clamp movement
        let dx = self.dx.clamp(-256, 255);
        let dy = self.dy.clamp(-256, 255);
        
        // Build packet
        let mut byte0 = self.buttons & 0x07;  // Buttons
        byte0 |= 0x08;  // Always 1
        
        if dx < 0 {
            byte0 |= 0x10;  // X sign
        }
        if dy < 0 {
            byte0 |= 0x20;  // Y sign (inverted in PS/2)
        }
        
        self.output_queue.push_back(byte0);
        self.output_queue.push_back((dx & 0xFF) as u8);
        self.output_queue.push_back(((-dy) & 0xFF) as u8);  // Y is inverted
        
        // Scroll wheel for IntelliMouse
        if self.get_device_id() >= 3 {
            self.output_queue.push_back((self.dz.clamp(-8, 7) & 0x0F) as u8);
        }
        
        // Clear accumulated movement
        self.dx = 0;
        self.dy = 0;
        self.dz = 0;
    }
}
```

---

## Browser Event Capture

### TypeScript Host Input Capture (Implemented)

The repository includes a concrete browser-side input capture implementation:

- `web/src/input/input_capture.ts` — attaches listeners to the emulator canvas, manages focus/blur, and requests Pointer Lock on click.
- `web/src/input/pointer_lock.ts` — minimal Pointer Lock state machine.
- `web/src/input/event_queue.ts` — allocation-free event queue and batching transport to the I/O worker.
- `web/src/input/scancodes.ts` — auto-generated `KeyboardEvent.code` → PS/2 Set 2 scancode mapping (including multi-byte sequences like PrintScreen/Pause).
- `web/src/input/scancode.ts` — small helpers (allocation-free lookup + browser preventDefault policy).

#### Worker Transport / Wire Format

Input batches are delivered to the I/O worker via `postMessage` with:

```ts
{ type: 'in:input-batch', buffer: ArrayBuffer, recycle?: true }
```

`buffer` contains a small `Int32Array`-compatible payload:

| Word | Meaning |
|------|---------|
| 0 | `count` (number of events) |
| 1 | `batchSendTimestampUs` (u32, `performance.now()*1000`, wraps) |
| 2.. | `count` events, each 4 words: `[type, eventTimestampUs, a, b]` |

Event types are defined in `web/src/input/event_queue.ts` (`InputEventType`):

- `KeyScancode (1)`: `a=packedBytesLE`, `b=byteLen` (PS/2 Set 2 bytes including `0xE0`/`0xF0`). Long sequences are split across multiple `KeyScancode` events in-order (max 4 bytes per event).
- `KeyHidUsage (6)`: `a=(usage & 0xFF) | ((pressed ? 1 : 0) << 8)`, `b=unused` (USB HID keyboard usage events on Usage Page 0x07). Emitted in addition to `KeyScancode` so the runtime can drive both PS/2 and USB HID paths from the same captured input.
- `MouseMove (2)`: `a=dx`, `b=dy` (PS/2 coords: `dx` right, `dy` up)
- `MouseButtons (3)`: `a=buttons` (bit0=left, bit1=right, bit2=middle)
- `MouseWheel (4)`: `a=dz` (positive=wheel up)
- `GamepadReport (5)`: `a=packedBytes0to3LE`, `b=packedBytes4to7LE` (8-byte USB HID gamepad input report; see `web/src/input/gamepad.ts` for packing and `crates/aero-usb/src/hid/gamepad.rs::GamepadReport` for the canonical report layout)

This keeps the hot path allocation-free and allows the worker to convert to
i8042 keyboard/mouse bytes with minimal overhead.

##### Optional buffer recycling

If `recycle: true` is set on the batch, the worker may transfer the same buffer
back to the sender once processed:

```ts
{ type: 'in:input-batch-recycle', buffer: ArrayBuffer }
```

This avoids allocating a new `ArrayBuffer` per flush on the main thread.

### Event Handler

```rust
pub struct InputCapture {
    canvas: HtmlCanvasElement,
    pointer_locked: bool,
    keyboard_buffer: VecDeque<KeyEvent>,
    mouse_buffer: VecDeque<MouseEvent>,
}

impl InputCapture {
    pub fn setup(&mut self) {
        // Keyboard events
        let keyboard_buffer = self.keyboard_buffer.clone();
        self.canvas.add_event_listener("keydown", move |event: web_sys::KeyboardEvent| {
            event.prevent_default();
            
            let scancode = js_keycode_to_scancode(event.code().as_str());
            keyboard_buffer.borrow_mut().push_back(KeyEvent {
                scancode,
                pressed: true,
            });
        });
        
        let keyboard_buffer = self.keyboard_buffer.clone();
        self.canvas.add_event_listener("keyup", move |event: web_sys::KeyboardEvent| {
            event.prevent_default();
            
            let scancode = js_keycode_to_scancode(event.code().as_str());
            keyboard_buffer.borrow_mut().push_back(KeyEvent {
                scancode,
                pressed: false,
            });
        });
        
        // Mouse events (with pointer lock)
        let mouse_buffer = self.mouse_buffer.clone();
        self.canvas.add_event_listener("mousemove", move |event: web_sys::MouseEvent| {
            mouse_buffer.borrow_mut().push_back(MouseEvent::Move {
                dx: event.movement_x(),
                dy: event.movement_y(),
            });
        });
        
        // Request pointer lock on click
        self.canvas.add_event_listener("click", |event| {
            event.target().request_pointer_lock();
        });
    }
    
    pub fn request_pointer_lock(&self) {
        self.canvas.request_pointer_lock();
    }
}
```

### Scancode Translation

```rust
// Scancode translation is generated from a single source-of-truth table:
//
//   tools/gen_scancodes/scancodes.json
//
// Outputs:
//   - src/input/scancodes.ts
//   - web/src/input/scancodes.ts
//   - crates/aero-devices-input/src/scancodes_generated.rs
//
// This keeps the JS capture side and Rust/WASM side in sync, including extended
// keys (0xE0 prefix) and special multi-byte sequences like PrintScreen/Pause.
use aero_devices_input::scancode::browser_code_to_set2_bytes;

pub fn key_event_bytes(code: &str, pressed: bool) -> Option<Vec<u8>> {
    browser_code_to_set2_bytes(code, pressed)
}
```

---

## USB HID (Optional)

For browser input → USB HID usage mapping and report format details, see
[`docs/usb-hid.md`](./usb-hid.md).

For USB HID **gamepad** details (including the composite HID topology and the exact
gamepad report descriptor + byte layout), see
[`docs/usb-hid-gamepad.md`](./usb-hid-gamepad.md).

For WebHID device passthrough (where the browser does not expose the raw HID
report descriptor bytes), see
[`docs/webhid-hid-report-descriptor-synthesis.md`](./webhid-hid-report-descriptor-synthesis.md).

For the end-to-end “real device” passthrough architecture (main thread owns the
handle; worker models UHCI + a generic HID device), see
[`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md).

### USB HID Keyboard

```rust
pub struct UsbHidKeyboard {
    endpoint: u8,
    report: KeyboardReport,
    pending_reports: VecDeque<KeyboardReport>,
}

#[repr(C, packed)]
pub struct KeyboardReport {
    modifiers: u8,     // Ctrl, Shift, Alt, GUI
    reserved: u8,
    keys: [u8; 6],     // Up to 6 simultaneous keys
}

impl UsbHidKeyboard {
    pub fn key_event(&mut self, usage: u8, pressed: bool) {
        if is_modifier(usage) {
            let modifier_bit = modifier_to_bit(usage);
            if pressed {
                self.report.modifiers |= modifier_bit;
            } else {
                self.report.modifiers &= !modifier_bit;
            }
        } else {
            if pressed {
                // Add key to report
                for slot in &mut self.report.keys {
                    if *slot == 0 {
                        *slot = usage;
                        break;
                    }
                }
            } else {
                // Remove key from report
                for slot in &mut self.report.keys {
                    if *slot == usage {
                        *slot = 0;
                    }
                }
                // Compact the array
                self.report.keys.sort_by(|a, b| {
                    if *a == 0 { std::cmp::Ordering::Greater }
                    else if *b == 0 { std::cmp::Ordering::Less }
                    else { std::cmp::Ordering::Equal }
                });
            }
        }
        
        self.pending_reports.push_back(self.report.clone());
    }
    
    pub fn get_descriptor(&self) -> &[u8] {
        // HID Report Descriptor for keyboard
        static DESCRIPTOR: &[u8] = &[
            0x05, 0x01,  // Usage Page (Generic Desktop)
            0x09, 0x06,  // Usage (Keyboard)
            0xA1, 0x01,  // Collection (Application)
            
            // Modifier keys
            0x05, 0x07,  // Usage Page (Keyboard)
            0x19, 0xE0,  // Usage Minimum (Left Control)
            0x29, 0xE7,  // Usage Maximum (Right GUI)
            0x15, 0x00,  // Logical Minimum (0)
            0x25, 0x01,  // Logical Maximum (1)
            0x75, 0x01,  // Report Size (1)
            0x95, 0x08,  // Report Count (8)
            0x81, 0x02,  // Input (Data, Variable, Absolute)
            
            // Reserved byte
            0x95, 0x01,  // Report Count (1)
            0x75, 0x08,  // Report Size (8)
            0x81, 0x01,  // Input (Constant)
            
            // Key array
            0x95, 0x06,  // Report Count (6)
            0x75, 0x08,  // Report Size (8)
            0x15, 0x00,  // Logical Minimum (0)
            0x25, 0x65,  // Logical Maximum (101)
            0x05, 0x07,  // Usage Page (Keyboard)
            0x19, 0x00,  // Usage Minimum (0)
            0x29, 0x65,  // Usage Maximum (101)
            0x81, 0x00,  // Input (Data, Array)
            
            0xC0,        // End Collection
        ];
        DESCRIPTOR
    }
}
```

---

## WebHID/WebUSB passthrough (optional “real devices”)

In addition to synthesizing PS/2 or USB HID events from browser input events,
the host can optionally pass through a **real, host-connected HID device** into
the guest using browser device APIs (WebHID for MVP; WebUSB is available but more limited/experimental).

See [`docs/webhid-webusb-passthrough.md`](./webhid-webusb-passthrough.md) for the
intended architecture, security model, and current limitations.

---

## Virtio-input (Paravirtualized Keyboard/Mouse)

Virtio-input is the virtio device for input peripherals. Conceptually it is a stream of small typed events (similar to Linux `evdev`) delivered over virtqueues, rather than a full USB bus + endpoints + HID polling model.

For Aero, virtio-input is the intended “fast path” for keyboard/mouse once the guest driver is installed:

- **Host side is simpler**: no USB host controller state machines, device enumeration, descriptors, periodic interrupt transfers, etc.
- **Lower latency / easier batching**: we can push events as they happen (or coalesce them per frame) into a ring buffer and raise an interrupt, instead of modeling USB frames/microframes.
- **Guest sees standard Windows input**: the custom virtio driver exposes a normal HID keyboard/mouse stack, so applications remain unaware of virtio.

### Browser events → virtio-input events

Virtio-input uses `virtio_input_event { type, code, value }` entries, with `EV_SYN` used to delimit a coherent “report” (similar to committing a packet).

At a high level we map:

- **Keyboard**
  - `keydown` → `EV_KEY` + `KEY_*` + `value=1`
  - `keyup` → `EV_KEY` + `KEY_*` + `value=0`
  - End of report → `EV_SYN` + `SYN_REPORT` + `value=0`
- **Mouse (relative, pointer-lock)**
  - `mousemove` (`movementX/Y`) → `EV_REL` + `REL_X`/`REL_Y` + signed delta
  - Wheel → `EV_REL` + `REL_WHEEL` (and optionally `REL_HWHEEL`)
  - Buttons (`mousedown`/`mouseup`) → `EV_KEY` + `BTN_LEFT`/`BTN_RIGHT`/`BTN_MIDDLE`… + `value=1/0`
  - End of report → `EV_SYN` + `SYN_REPORT` + `value=0`

Compared to USB HID, this is essentially a direct “event injection” interface: the host translates browser input into a small fixed struct and places it in a queue.

### Windows 7 guest driver model (HID minidriver)

Windows 7 does not ship a virtio-input driver, so virtio-input requires a custom guest driver that speaks virtio and presents a HID device to the OS.

Intended stack:

```
virtio-input device (PCI / virtqueues)
  → our virtio-input KMDF HID minidriver
    → hidclass.sys
      → kbdhid.sys / mouhid.sys
        → kbdclass.sys / mouclass.sys
```

Responsibilities of the virtio-input driver at a conceptual level:

- Initialize the virtio device and negotiate features.
- Provide buffers for the **eventq** (device → driver) and parse incoming `EV_KEY`/`EV_REL`/`EV_SYN`.
- Convert virtio-input events into **HID input reports** and submit them up to `hidclass.sys`.
- Accept **HID output reports** (keyboard LEDs) and emit virtio-input output events over the **statusq**.

### Two-queue model (eventq + statusq) and LEDs

Virtio-input uses two virtqueues:

- **eventq**: host/device publishes input events (key presses, relative motion).
- **statusq**: guest/driver publishes output events back to the host (primarily keyboard LED state like Caps Lock / Num Lock).

For Aero, we should handle LED output events even if we don’t initially surface them to the browser UI. Keeping the round-trip correct avoids subtle guest driver behavior differences (e.g., toggling Caps Lock producing output reports that must be acknowledged).

### Recommended device model: multi-function PCI virtio-input (2 functions)

Contract v1 exposes virtio-input as a **single multi-function PCI device** with two virtio-input **functions**:

1. Function 0: virtio-input **keyboard** (`SUBSYS 0x0010`, `header_type = 0x80` to advertise multi-function)
2. Function 1: virtio-input **mouse** (relative pointer, `SUBSYS 0x0011`)

This still avoids composite HID device complexity and lets Windows naturally bind the inbox `kbdhid.sys` and `mouhid.sys` clients, while keeping the PCI topology stable for driver matching.

### Testing notes

- **End-to-end test plan:** [`docs/virtio-input-test-plan.md`](./virtio-input-test-plan.md) (Rust conformance tests + browser wiring checks + Win7 driver bring-up).
- **Reference implementation**: validate the guest driver + device model in QEMU first using `virtio-keyboard-pci` and `virtio-mouse-pci` (or `virtio-tablet-pci` if experimenting with absolute coordinates).
  - For strict `AERO-W7-VIRTIO` **contract v1** driver testing, use **modern-only** virtio (`disable-legacy=on`) and force the **contract Revision ID** (`x-pci-revision=0x01`), since many QEMU virtio devices report `REV_00` by default.
  - See: `drivers/windows7/virtio-input/tests/qemu/README.md` for full command lines.
- **Windows 7 driver install**: plan on **test signing** for development (enable test mode and sign the KMDF driver with a test certificate) before tackling production signing/distribution.

---

## Gamepad Support

Gamepads can be surfaced to the guest either via a paravirtualized path (future)
or as a USB HID game controller. For the USB HID gamepad spec used by Aero, see
[`docs/usb-hid-gamepad.md`](./usb-hid-gamepad.md).

```rust
pub struct GamepadHandler {
    gamepads: HashMap<u32, GamepadState>,
}

impl GamepadHandler {
    pub fn poll(&mut self) -> Vec<GamepadEvent> {
        let mut events = Vec::new();
        
        let gamepads = navigator().get_gamepads().unwrap_or_default();
        
        for gamepad in gamepads.iter().filter_map(|g| g) {
            let id = gamepad.index();
            let old_state = self.gamepads.entry(id).or_default();
            
            // Check buttons
            for (i, button) in gamepad.buttons().iter().enumerate() {
                let pressed = button.pressed();
                if pressed != old_state.buttons[i] {
                    events.push(GamepadEvent::Button {
                        gamepad: id,
                        button: i as u8,
                        pressed,
                    });
                    old_state.buttons[i] = pressed;
                }
            }
            
            // Check axes
            for (i, axis) in gamepad.axes().iter().enumerate() {
                let value = *axis as f32;
                if (value - old_state.axes[i]).abs() > 0.01 {
                    events.push(GamepadEvent::Axis {
                        gamepad: id,
                        axis: i as u8,
                        value,
                    });
                    old_state.axes[i] = value;
                }
            }
        }
        
        events
    }
}
```

---

## Performance Targets

| Metric | Target | Notes |
|--------|--------|-------|
| Input Latency | < 16ms | One frame at 60 FPS |
| Key Repeat | Configurable | Match Windows settings |
| Mouse Resolution | ≥ 1000 DPI | High precision |
| Poll Rate | 125-1000 Hz | USB standard rates |

---

## Next Steps

- See [BIOS/Firmware](./09-bios-firmware.md) for system initialization
- See [Browser APIs](./11-browser-apis.md) for Pointer Lock API details
- See [Task Breakdown](./15-agent-task-breakdown.md) for input tasks
- See [virtio-input](./virtio-input.md) for the paravirtual keyboard/mouse fast path
