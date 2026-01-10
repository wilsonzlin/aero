# 08 - Input Device Emulation

## Overview

Windows 7 requires keyboard, mouse, and optionally USB input device support. We must capture browser events and translate them to PS/2 or USB HID protocols.

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
│  │  Scancode Translation                                    │    │
│  │    - Browser keyCode → PS/2 scancode                     │    │
│  │    - Mouse movement → PS/2 packets                       │    │
│  │    - USB HID report generation                           │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│       ▼                                                          │
│  ┌────────────────────┐  ┌────────────────────┐                 │
│  │   PS/2 Controller  │  │   USB Controller   │                 │
│  │   (i8042)          │  │   (UHCI/EHCI)      │                 │
│  └────────────────────┘  └────────────────────┘                 │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## PS/2 Controller (i8042)

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
pub fn js_keycode_to_scancode(code: &str) -> u8 {
    // Map JavaScript key codes to PS/2 Set 2 scancodes
    match code {
        "Escape" => 0x76,
        "F1" => 0x05,
        "F2" => 0x06,
        "F3" => 0x04,
        "F4" => 0x0C,
        "F5" => 0x03,
        "F6" => 0x0B,
        "F7" => 0x83,
        "F8" => 0x0A,
        "F9" => 0x01,
        "F10" => 0x09,
        "F11" => 0x78,
        "F12" => 0x07,
        
        "Backquote" => 0x0E,
        "Digit1" => 0x16,
        "Digit2" => 0x1E,
        "Digit3" => 0x26,
        "Digit4" => 0x25,
        "Digit5" => 0x2E,
        "Digit6" => 0x36,
        "Digit7" => 0x3D,
        "Digit8" => 0x3E,
        "Digit9" => 0x46,
        "Digit0" => 0x45,
        "Minus" => 0x4E,
        "Equal" => 0x55,
        "Backspace" => 0x66,
        
        "Tab" => 0x0D,
        "KeyQ" => 0x15,
        "KeyW" => 0x1D,
        "KeyE" => 0x24,
        "KeyR" => 0x2D,
        "KeyT" => 0x2C,
        "KeyY" => 0x35,
        "KeyU" => 0x3C,
        "KeyI" => 0x43,
        "KeyO" => 0x44,
        "KeyP" => 0x4D,
        "BracketLeft" => 0x54,
        "BracketRight" => 0x5B,
        "Backslash" => 0x5D,
        
        "CapsLock" => 0x58,
        "KeyA" => 0x1C,
        "KeyS" => 0x1B,
        "KeyD" => 0x23,
        "KeyF" => 0x2B,
        "KeyG" => 0x34,
        "KeyH" => 0x33,
        "KeyJ" => 0x3B,
        "KeyK" => 0x42,
        "KeyL" => 0x4B,
        "Semicolon" => 0x4C,
        "Quote" => 0x52,
        "Enter" => 0x5A,
        
        "ShiftLeft" => 0x12,
        "KeyZ" => 0x1A,
        "KeyX" => 0x22,
        "KeyC" => 0x21,
        "KeyV" => 0x2A,
        "KeyB" => 0x32,
        "KeyN" => 0x31,
        "KeyM" => 0x3A,
        "Comma" => 0x41,
        "Period" => 0x49,
        "Slash" => 0x4A,
        "ShiftRight" => 0x59,
        
        "ControlLeft" => 0x14,
        "AltLeft" => 0x11,
        "Space" => 0x29,
        
        // Extended keys (need 0xE0 prefix)
        "ArrowUp" => 0x75,     // E0 75
        "ArrowDown" => 0x72,   // E0 72
        "ArrowLeft" => 0x6B,   // E0 6B
        "ArrowRight" => 0x74,  // E0 74
        "Home" => 0x6C,        // E0 6C
        "End" => 0x69,         // E0 69
        "PageUp" => 0x7D,      // E0 7D
        "PageDown" => 0x7A,    // E0 7A
        "Insert" => 0x70,      // E0 70
        "Delete" => 0x71,      // E0 71
        
        _ => {
            log::debug!("Unknown key code: {}", code);
            0x00
        }
    }
}

pub fn is_extended_key(code: &str) -> bool {
    matches!(code, 
        "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" |
        "Home" | "End" | "PageUp" | "PageDown" |
        "Insert" | "Delete" |
        "ControlRight" | "AltRight" |
        "NumpadEnter" | "NumpadDivide"
    )
}
```

---

## USB HID (Optional)

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

## Gamepad Support

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
