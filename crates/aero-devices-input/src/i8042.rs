use std::collections::VecDeque;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::ps2_keyboard::Ps2Keyboard;
use crate::ps2_mouse::{Ps2Mouse, Ps2MouseButton};
use crate::scancode::{browser_code_to_set2, Set2Scancode};

/// Sink for wiring device IRQs into the rest of the system (PIC/APIC).
pub trait IrqSink {
    fn raise_irq(&mut self, irq: u8);
}

/// Sink for wiring i8042 "system control" side effects into the rest of the system.
///
/// Real i8042 controllers expose an output port that commonly controls:
/// - the A20 gate (bit 1)
/// - CPU reset (bit 0, active-low)
pub trait SystemControlSink {
    fn set_a20(&mut self, enabled: bool);
    fn request_reset(&mut self);

    /// Returns the current platform A20 gate state if it is observable by the sink.
    ///
    /// When this returns `Some`, the i8042 model will:
    /// - report the A20 state via command `0xD0` (read output port)
    /// - use the returned value to detect A20 state changes even if the internal
    ///   output-port latch is stale (e.g. A20 toggled via port 0x92)
    ///
    /// Returning `None` preserves legacy behaviour: the output-port A20 bit
    /// reflects only the last value written via i8042 commands.
    fn a20_enabled(&self) -> Option<bool> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputSource {
    Keyboard,
    Mouse,
    Controller,
}

#[derive(Debug, Clone, Copy)]
struct OutputByte {
    value: u8,
    source: OutputSource,
}

// i8042 status register bits.
const STATUS_OBF: u8 = 0x01; // Output buffer full.
const STATUS_IBF: u8 = 0x02; // Input buffer full.
const STATUS_SYS: u8 = 0x04; // System flag.
const STATUS_A2: u8 = 0x08; // Last write to command port.
const STATUS_AUX_OBF: u8 = 0x20; // Mouse output buffer full.

// i8042 output port bits.
const OUTPUT_PORT_RESET: u8 = 0x01; // CPU reset line (active-low).
const OUTPUT_PORT_A20: u8 = 0x02; // A20 gate.

/// Maximum number of bytes buffered in the controller's pending-output queue.
///
/// This queue holds bytes that have been produced by the PS/2 devices or the controller itself,
/// but have not yet been loaded into the guest-visible output buffer.
pub const MAX_PENDING_OUTPUT: usize = 4096;

#[derive(Debug, Clone, Copy)]
enum PendingWrite {
    CommandByte,
    OutputPort,
    WriteToMouse,
    /// Writes the next data byte directly into the controller output buffer as
    /// keyboard-originated data (i8042 command 0xD2).
    WriteToOutputBufferKeyboard,
    /// Writes the next data byte directly into the controller output buffer as
    /// mouse-originated data (i8042 command 0xD3).
    WriteToOutputBufferMouse,
}

/// Set-2 -> Set-1 translation state, used when the command-byte translation bit
/// is enabled (bit 6).
#[derive(Debug, Default)]
struct Set2ToSet1 {
    saw_e0: bool,
    saw_f0: bool,
}

impl Set2ToSet1 {
    fn feed(&mut self, byte: u8) -> Option<u8> {
        match byte {
            0xE0 => {
                self.saw_e0 = true;
                Some(0xE0)
            }
            // Used by Pause/Break.
            0xE1 => {
                self.saw_e0 = false;
                self.saw_f0 = false;
                Some(0xE1)
            }
            0xF0 => {
                self.saw_f0 = true;
                None
            }
            _ => {
                let extended = self.saw_e0;
                let break_code = self.saw_f0;
                self.saw_e0 = false;
                self.saw_f0 = false;

                let mut out = set2_to_set1(byte, extended);
                if break_code {
                    out |= 0x80;
                }
                Some(out)
            }
        }
    }
}

fn set2_to_set1(code: u8, extended: bool) -> u8 {
    match (code, extended) {
        // Non-extended
        (0x76, false) => 0x01, // Esc
        (0x16, false) => 0x02, // 1
        (0x1E, false) => 0x03, // 2
        (0x26, false) => 0x04, // 3
        (0x25, false) => 0x05, // 4
        (0x2E, false) => 0x06, // 5
        (0x36, false) => 0x07, // 6
        (0x3D, false) => 0x08, // 7
        (0x3E, false) => 0x09, // 8
        (0x46, false) => 0x0A, // 9
        (0x45, false) => 0x0B, // 0
        (0x4E, false) => 0x0C, // -
        (0x55, false) => 0x0D, // =
        (0x66, false) => 0x0E, // Backspace
        (0x0D, false) => 0x0F, // Tab
        (0x15, false) => 0x10, // Q
        (0x1D, false) => 0x11, // W
        (0x24, false) => 0x12, // E
        (0x2D, false) => 0x13, // R
        (0x2C, false) => 0x14, // T
        (0x35, false) => 0x15, // Y
        (0x3C, false) => 0x16, // U
        (0x43, false) => 0x17, // I
        (0x44, false) => 0x18, // O
        (0x4D, false) => 0x19, // P
        (0x54, false) => 0x1A, // [
        (0x5B, false) => 0x1B, // ]
        (0x5A, false) => 0x1C, // Enter
        (0x14, false) => 0x1D, // Left Ctrl
        (0x1C, false) => 0x1E, // A
        (0x1B, false) => 0x1F, // S
        (0x23, false) => 0x20, // D
        (0x2B, false) => 0x21, // F
        (0x34, false) => 0x22, // G
        (0x33, false) => 0x23, // H
        (0x3B, false) => 0x24, // J
        (0x42, false) => 0x25, // K
        (0x4B, false) => 0x26, // L
        (0x4C, false) => 0x27, // ;
        (0x52, false) => 0x28, // '
        (0x0E, false) => 0x29, // `
        (0x12, false) => 0x2A, // Left Shift
        (0x5D, false) => 0x2B, // \
        (0x1A, false) => 0x2C, // Z
        (0x22, false) => 0x2D, // X
        (0x21, false) => 0x2E, // C
        (0x2A, false) => 0x2F, // V
        (0x32, false) => 0x30, // B
        (0x31, false) => 0x31, // N
        (0x3A, false) => 0x32, // M
        (0x41, false) => 0x33, // ,
        (0x49, false) => 0x34, // .
        (0x4A, false) => 0x35, // /
        (0x59, false) => 0x36, // Right Shift
        (0x11, false) => 0x38, // Left Alt
        (0x29, false) => 0x39, // Space
        (0x58, false) => 0x3A, // CapsLock
        (0x05, false) => 0x3B, // F1
        (0x06, false) => 0x3C, // F2
        (0x04, false) => 0x3D, // F3
        (0x0C, false) => 0x3E, // F4
        (0x03, false) => 0x3F, // F5
        (0x0B, false) => 0x40, // F6
        (0x83, false) => 0x41, // F7
        (0x0A, false) => 0x42, // F8
        (0x01, false) => 0x43, // F9
        (0x09, false) => 0x44, // F10
        (0x78, false) => 0x57, // F11
        (0x07, false) => 0x58, // F12
        (0x77, false) => 0x45, // NumLock
        (0x7E, false) => 0x46, // ScrollLock
        (0x6C, false) => 0x47, // Numpad7
        (0x75, false) => 0x48, // Numpad8
        (0x7D, false) => 0x49, // Numpad9
        (0x7B, false) => 0x4A, // NumpadSubtract
        (0x6B, false) => 0x4B, // Numpad4
        (0x73, false) => 0x4C, // Numpad5
        (0x74, false) => 0x4D, // Numpad6
        (0x79, false) => 0x4E, // NumpadAdd
        (0x69, false) => 0x4F, // Numpad1
        (0x72, false) => 0x50, // Numpad2
        (0x7A, false) => 0x51, // Numpad3
        (0x70, false) => 0x52, // Numpad0
        (0x71, false) => 0x53, // NumpadDecimal
        (0x7C, false) => 0x37, // NumpadMultiply
        (0x61, false) => 0x56, // IntlBackslash (ISO 102-key)
        // Extended
        (0x14, true) => 0x1D, // Right Ctrl
        (0x11, true) => 0x38, // Right Alt
        (0x75, true) => 0x48, // Up
        (0x72, true) => 0x50, // Down
        (0x6B, true) => 0x4B, // Left
        (0x74, true) => 0x4D, // Right
        (0x6C, true) => 0x47, // Home
        (0x69, true) => 0x4F, // End
        (0x7D, true) => 0x49, // PageUp
        (0x7A, true) => 0x51, // PageDown
        (0x70, true) => 0x52, // Insert
        (0x71, true) => 0x53, // Delete
        (0x5A, true) => 0x1C, // Numpad Enter
        (0x4A, true) => 0x35, // Numpad Divide
        (0x1F, true) => 0x5B, // Left Meta / Windows
        (0x27, true) => 0x5C, // Right Meta / Windows
        (0x2F, true) => 0x5D, // ContextMenu
        (0x12, true) => 0x2A, // PrintScreen sequence
        (0x7C, true) => 0x37, // PrintScreen sequence
        _ => code,
    }
}

/// i8042 (PS/2 controller) model exposing ports 0x60/0x64.
pub struct I8042Controller {
    status: u8,
    command_byte: u8,
    output_port: u8,
    output_buffer: Option<OutputByte>,
    pending_output: VecDeque<OutputByte>,
    dropped_output_bytes: u64,
    pending_write: Option<PendingWrite>,
    last_write_was_command: bool,

    keyboard: Ps2Keyboard,
    mouse: Ps2Mouse,
    translator: Set2ToSet1,

    irq_sink: Option<Box<dyn IrqSink>>,
    sys_ctrl: Option<Box<dyn SystemControlSink>>,
    prefer_mouse: bool,
}

impl I8042Controller {
    pub fn new() -> Self {
        // Default command byte matches typical PC firmware expectations:
        //  - IRQ1 enabled
        //  - system flag set
        //  - translation enabled (Set-2 -> Set-1)
        let command_byte = 0x45;
        Self {
            status: STATUS_SYS,
            command_byte,
            // Platform dependent; bit0 is typically deasserted (1), A20 typically disabled.
            output_port: OUTPUT_PORT_RESET,
            output_buffer: None,
            pending_output: VecDeque::new(),
            dropped_output_bytes: 0,
            pending_write: None,
            last_write_was_command: false,
            keyboard: Ps2Keyboard::new(),
            mouse: Ps2Mouse::new(),
            translator: Set2ToSet1::default(),
            irq_sink: None,
            sys_ctrl: None,
            prefer_mouse: false,
        }
    }

    /// Returns the current length of the controller's pending-output queue.
    ///
    /// This is primarily intended for tests/fuzzing to assert that internal buffering remains
    /// bounded even with adversarial guest I/O streams.
    pub fn pending_output_len(&self) -> usize {
        self.pending_output.len()
    }

    pub fn set_irq_sink(&mut self, sink: Box<dyn IrqSink>) {
        self.irq_sink = Some(sink);
    }

    pub fn set_system_control_sink(&mut self, sink: Box<dyn SystemControlSink>) {
        // The sink is expected to observe edges (set_output_port) rather than receive an initial
        // callback on attach. We only push state when A20 is already enabled, which is useful
        // for restore paths where the sink can be attached after `load_state`.
        let mut sink = sink;
        if (self.output_port & OUTPUT_PORT_A20) != 0 {
            sink.set_a20(true);
        }
        self.sys_ctrl = Some(sink);
    }

    /// Reset the controller back to its power-on state.
    ///
    /// This keeps host-side integration points (`irq_sink` and `sys_ctrl`) attached so the
    /// platform wiring does not need to be rebuilt after a guest-initiated reboot.
    pub fn reset(&mut self) {
        let prev_output_port = self.output_port;
        let irq_sink = self.irq_sink.take();
        let sys_ctrl = self.sys_ctrl.take();

        *self = Self::new();

        self.irq_sink = irq_sink;

        if let Some(mut sink) = sys_ctrl {
            // `Self::new()` resets `output_port` to OUTPUT_PORT_RESET (A20 disabled). If the
            // previous state had A20 enabled, explicitly deassert it so the platform A20 gate
            // handle stays consistent across resets.
            if (prev_output_port & OUTPUT_PORT_A20) != 0 {
                sink.set_a20(false);
            }
            self.sys_ctrl = Some(sink);
        }
    }

    pub fn keyboard(&self) -> &Ps2Keyboard {
        &self.keyboard
    }

    pub fn keyboard_mut(&mut self) -> &mut Ps2Keyboard {
        &mut self.keyboard
    }

    pub fn mouse(&self) -> &Ps2Mouse {
        &self.mouse
    }

    pub fn mouse_mut(&mut self) -> &mut Ps2Mouse {
        &mut self.mouse
    }

    pub fn read_port(&mut self, port: u16) -> u8 {
        match port {
            0x60 => self.read_data(),
            0x64 => self.read_status(),
            _ => 0xFF,
        }
    }

    pub fn write_port(&mut self, port: u16, value: u8) {
        match port {
            0x60 => self.write_data(value),
            0x64 => self.write_command(value),
            _ => {}
        }
    }

    /// Host-side injection helper: translate a browser code to Set-2 and feed it
    /// into the keyboard device.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        // If the keyboard port is disabled, the controller suppresses keyboard output (clock line
        // held low). Host-side key injection should not buffer scancodes and deliver them later
        // once the guest re-enables the port, as that would manifest as a burst of stale key
        // events.
        if !self.keyboard_port_enabled() {
            return;
        }
        let Some(sc) = browser_code_to_set2(code) else {
            return;
        };
        self.inject_set2_key(sc, pressed);
    }

    /// Host-side injection helper: enqueue raw keyboard scancode bytes.
    ///
    /// This is primarily intended for the browser runtime, where the JS capture
    /// layer already translates `KeyboardEvent.code` into PS/2 Set-2 scancode
    /// byte sequences (`web/src/input/scancodes.ts`).
    ///
    /// Bytes injected here are treated as keyboard-originated output and are
    /// therefore subject to:
    /// - keyboard scanning enabled/disabled state, and
    /// - i8042 command-byte Set-2 -> Set-1 translation when enabled.
    pub fn inject_keyboard_bytes(&mut self, bytes: &[u8]) {
        if !self.keyboard_port_enabled() {
            return;
        }
        self.keyboard.inject_bytes(bytes);
        self.service_output();
    }

    pub fn inject_set2_key(&mut self, scancode: Set2Scancode, pressed: bool) {
        if !self.keyboard_port_enabled() {
            return;
        }
        self.keyboard.inject_key(scancode, pressed);
        self.service_output();
    }

    /// Host-side injection helper: push a raw Set-2 scancode byte sequence into the keyboard.
    ///
    /// The browser input pipeline already generates Set-2 scancode sequences (including `E0` and
    /// `F0` prefixes) and transports them efficiently as packed bytes. This method feeds those raw
    /// bytes into the keyboard device output queue so that:
    /// - the i8042 controller can apply Set-2 -> Set-1 translation when the command-byte
    ///   translation bit is enabled, and
    /// - IRQ generation matches the normal "keyboard produced output" path.
    pub fn inject_key_scancode_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if !self.keyboard_port_enabled() {
            return;
        }
        self.keyboard.inject_scancode_bytes(bytes);
        self.service_output();
    }

    pub fn inject_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        // If the aux port is disabled, the controller suppresses mouse traffic (clock line held
        // low). Host-side injection should not buffer motion and later deliver it when the guest
        // re-enables the port; doing so would cause a large "cursor jump".
        if !self.mouse_port_enabled() {
            return;
        }
        self.mouse.inject_motion(dx, dy, wheel);
        self.service_output();
    }

    pub fn inject_mouse_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        if !self.mouse_port_enabled() {
            // When the AUX port is disabled, the controller suppresses mouse output. We still keep
            // an up-to-date button image so that the next injected motion packet (after the port
            // is re-enabled) carries the correct button bits.
            self.mouse.set_button_state(button, pressed);
            return;
        }

        self.mouse.inject_button(button, pressed);
        self.service_output();
    }

    pub fn mouse_buttons_mask(&self) -> u8 {
        self.mouse.buttons_mask()
    }

    /// Total number of bytes evicted from the bounded output queue due to overflow.
    ///
    /// The i8042 model caps `pending_output` at `MAX_PENDING_OUTPUT` by dropping the oldest byte
    /// when full. This counter provides host-side telemetry for diagnosing situations where
    /// output is produced faster than the guest drains port 0x60.
    pub fn dropped_output_bytes(&self) -> u64 {
        self.dropped_output_bytes
    }

    fn read_status(&mut self) -> u8 {
        let mut status = self.status;
        if self.last_write_was_command {
            status |= STATUS_A2;
        } else {
            status &= !STATUS_A2;
        }
        status
    }

    fn read_data(&mut self) -> u8 {
        let Some(out) = self.output_buffer.take() else {
            return 0x00;
        };

        self.status &= !STATUS_OBF;
        self.status &= !STATUS_AUX_OBF;

        // Immediately load any queued bytes and potentially raise the next IRQ.
        self.service_output();
        out.value
    }

    fn write_command(&mut self, cmd: u8) {
        self.last_write_was_command = true;
        self.status |= STATUS_IBF;

        match cmd {
            0x20 => {
                // Read command byte.
                self.push_controller_output(self.command_byte);
            }
            0x60 => {
                // Write command byte (next data write).
                self.pending_write = Some(PendingWrite::CommandByte);
            }
            0xA7 => {
                // Disable mouse port.
                self.command_byte |= 0x20;
            }
            0xA8 => {
                // Enable mouse port.
                self.command_byte &= !0x20;
            }
            0xA9 => {
                // Test mouse port (0x00 = pass).
                self.push_controller_output(0x00);
            }
            0xAA => {
                // Controller self-test (0x55 = pass).
                self.push_controller_output(0x55);
            }
            0xAB => {
                // Test keyboard port (0x00 = pass).
                self.push_controller_output(0x00);
            }
            0xAD => {
                // Disable keyboard port.
                self.command_byte |= 0x10;
            }
            0xAE => {
                // Enable keyboard port.
                self.command_byte &= !0x10;
            }
            0xD0 => {
                // Read output port.
                self.push_controller_output(self.output_port_for_guest());
            }
            0xD1 => {
                // Write output port (next data write).
                self.pending_write = Some(PendingWrite::OutputPort);
            }
            0xD2 => {
                // Write output buffer as keyboard data (next data write).
                self.pending_write = Some(PendingWrite::WriteToOutputBufferKeyboard);
            }
            0xD3 => {
                // Write output buffer as mouse data (next data write).
                self.pending_write = Some(PendingWrite::WriteToOutputBufferMouse);
            }
            0xD4 => {
                // Next data write goes to the mouse.
                self.pending_write = Some(PendingWrite::WriteToMouse);
            }
            0xDD => {
                // Non-standard (seen in some firmware): disable A20.
                self.set_output_port(self.output_port & !OUTPUT_PORT_A20);
            }
            0xDF => {
                // Non-standard (seen in some firmware): enable A20.
                self.set_output_port(self.output_port | OUTPUT_PORT_A20);
            }
            0xFE => {
                // Pulse output port bit 0 low (system reset).
                if let Some(sink) = self.sys_ctrl.as_deref_mut() {
                    sink.request_reset();
                }
            }
            _ => {}
        }

        self.status &= !STATUS_IBF;
        self.service_output();
    }

    fn write_data(&mut self, value: u8) {
        self.last_write_was_command = false;
        self.status |= STATUS_IBF;

        if let Some(pending) = self.pending_write.take() {
            match pending {
                PendingWrite::CommandByte => {
                    // The command-byte translation bit (bit 6) enables Set-2 -> Set-1 translation.
                    //
                    // The translator is stateful (tracks `E0`/`F0` prefixes). If the guest toggles
                    // translation mid-stream, any prefix state from the previous mode must be
                    // cleared; otherwise the next scancode byte may be misinterpreted as extended
                    // or as a break code.
                    let was_translation_enabled = self.translation_enabled();
                    self.command_byte = value;
                    let translation_enabled = self.translation_enabled();
                    if was_translation_enabled != translation_enabled {
                        self.translator = Set2ToSet1::default();
                    }
                }
                PendingWrite::OutputPort => {
                    self.set_output_port(value);
                }
                PendingWrite::WriteToMouse => {
                    self.mouse.receive_byte(value);
                }
                PendingWrite::WriteToOutputBufferKeyboard => {
                    // Bypass translation and device state; this is a controller command
                    // that forces the output buffer to appear as if the keyboard produced
                    // the byte.
                    self.push_pending_output(OutputByte {
                        value,
                        source: OutputSource::Keyboard,
                    });
                }
                PendingWrite::WriteToOutputBufferMouse => {
                    // Same as 0xD2, but marks the byte as mouse-originated (AUX).
                    self.push_pending_output(OutputByte {
                        value,
                        source: OutputSource::Mouse,
                    });
                }
            }
            self.status &= !STATUS_IBF;
            self.service_output();
            return;
        }

        // Default: send to keyboard.
        self.keyboard.receive_byte(value);
        self.status &= !STATUS_IBF;
        self.service_output();
    }

    fn set_output_port(&mut self, value: u8) {
        let prev = self.output_port;
        self.output_port = value;

        if let Some(sink) = self.sys_ctrl.as_deref_mut() {
            let prev_a20 = sink.a20_enabled().unwrap_or((prev & OUTPUT_PORT_A20) != 0);
            let new_a20 = (value & OUTPUT_PORT_A20) != 0;
            if prev_a20 != new_a20 {
                sink.set_a20(new_a20);
            }

            // Reset line is active-low: transitioning from 1 -> 0 asserts reset.
            let prev_reset_deasserted = (prev & OUTPUT_PORT_RESET) != 0;
            let new_reset_deasserted = (value & OUTPUT_PORT_RESET) != 0;
            if prev_reset_deasserted && !new_reset_deasserted {
                sink.request_reset();
            }
        }
    }

    fn output_port_for_guest(&self) -> u8 {
        let mut value = self.output_port;
        if let Some(sink) = self.sys_ctrl.as_deref() {
            if let Some(a20) = sink.a20_enabled() {
                if a20 {
                    value |= OUTPUT_PORT_A20;
                } else {
                    value &= !OUTPUT_PORT_A20;
                }
            }
        }
        value
    }

    fn translation_enabled(&self) -> bool {
        self.command_byte & 0x40 != 0
    }

    fn keyboard_port_enabled(&self) -> bool {
        self.command_byte & 0x10 == 0
    }

    fn mouse_port_enabled(&self) -> bool {
        self.command_byte & 0x20 == 0
    }

    fn push_pending_output(&mut self, out: OutputByte) {
        if self.pending_output.len() >= MAX_PENDING_OUTPUT {
            let _ = self.pending_output.pop_front();
            self.dropped_output_bytes = self.dropped_output_bytes.saturating_add(1);
        }
        self.pending_output.push_back(out);
    }

    fn push_controller_output(&mut self, value: u8) {
        self.push_pending_output(OutputByte {
            value,
            source: OutputSource::Controller,
        });
        self.service_output();
    }

    fn service_output(&mut self) {
        if self.output_buffer.is_some() {
            return;
        }

        // If we already have queued bytes (from translation or controller
        // responses), output them first.
        if let Some(out) = self.pending_output.pop_front() {
            self.load_output(out);
            return;
        }

        // Otherwise, pull from devices.
        loop {
            let take_mouse_first = self.prefer_mouse;
            let mut progressed = false;

            if take_mouse_first {
                progressed |= self.pull_from_mouse();
                progressed |= self.pull_from_keyboard();
            } else {
                progressed |= self.pull_from_keyboard();
                progressed |= self.pull_from_mouse();
            }

            if let Some(out) = self.pending_output.pop_front() {
                self.load_output(out);
                self.prefer_mouse = matches!(out.source, OutputSource::Keyboard);
                return;
            }

            if !progressed {
                return;
            }
        }
    }

    fn pull_from_keyboard(&mut self) -> bool {
        if !self.keyboard_port_enabled() {
            return false;
        }
        let Some(byte) = self.keyboard.pop_output() else {
            return false;
        };

        if self.translation_enabled() {
            if let Some(out) = self.translator.feed(byte) {
                self.push_pending_output(OutputByte {
                    value: out,
                    source: OutputSource::Keyboard,
                });
            }
        } else {
            self.push_pending_output(OutputByte {
                value: byte,
                source: OutputSource::Keyboard,
            });
        }
        true
    }

    fn pull_from_mouse(&mut self) -> bool {
        if !self.mouse_port_enabled() {
            return false;
        }
        let Some(byte) = self.mouse.pop_output() else {
            return false;
        };
        self.push_pending_output(OutputByte {
            value: byte,
            source: OutputSource::Mouse,
        });
        true
    }

    fn load_output(&mut self, out: OutputByte) {
        self.output_buffer = Some(out);
        self.status |= STATUS_OBF;

        match out.source {
            OutputSource::Mouse => {
                self.status |= STATUS_AUX_OBF;
                if self.command_byte & 0x02 != 0 {
                    if let Some(sink) = self.irq_sink.as_deref_mut() {
                        sink.raise_irq(12);
                    }
                }
            }
            OutputSource::Keyboard => {
                self.status &= !STATUS_AUX_OBF;
                if self.command_byte & 0x01 != 0 {
                    if let Some(sink) = self.irq_sink.as_deref_mut() {
                        sink.raise_irq(1);
                    }
                }
            }
            OutputSource::Controller => {
                self.status &= !STATUS_AUX_OBF;
            }
        }
    }

    /// Whether the guest-visible IRQ1 line should be considered asserted.
    ///
    /// The i8042 controller model emits IRQ pulses via [`IrqSink::raise_irq`] when output is
    /// loaded, but browser integrations often need an explicit level that can be translated into
    /// `irqRaise`/`irqLower` events. The level is high when:
    /// - the output buffer currently holds a keyboard-originated byte, and
    /// - the command byte has IRQ1 enabled (bit 0).
    pub fn irq1_level(&self) -> bool {
        (self.command_byte & 0x01) != 0
            && matches!(
                self.output_buffer,
                Some(OutputByte {
                    source: OutputSource::Keyboard,
                    ..
                })
            )
    }

    /// Whether the guest-visible IRQ12 line should be considered asserted.
    ///
    /// The level is high when:
    /// - the output buffer currently holds a mouse-originated byte, and
    /// - the command byte has IRQ12 enabled (bit 1).
    pub fn irq12_level(&self) -> bool {
        (self.command_byte & 0x02) != 0
            && matches!(
                self.output_buffer,
                Some(OutputByte {
                    source: OutputSource::Mouse,
                    ..
                })
            )
    }
}

impl Default for I8042Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for I8042Controller {
    const DEVICE_ID: [u8; 4] = *b"8042";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 3);

    fn save_state(&self) -> Vec<u8> {
        const TAG_REGS: u16 = 1;
        const TAG_OUTPUT_BUFFER: u16 = 2;
        const TAG_PENDING_OUTPUT: u16 = 3;
        const TAG_PENDING_WRITE: u16 = 4;
        const TAG_LAST_WRITE_WAS_CMD: u16 = 5;
        const TAG_KEYBOARD: u16 = 6;
        const TAG_MOUSE: u16 = 7;
        const TAG_TRANSLATOR: u16 = 8;
        const TAG_PREFER_MOUSE: u16 = 9;
        const TAG_OUTPUT_PORT: u16 = 10;
        const TAG_DROPPED_OUTPUT_BYTES: u16 = 11;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_bytes(
            TAG_REGS,
            Encoder::new()
                .u8(self.status)
                .u8(self.command_byte)
                .finish(),
        );
        // Store the guest-visible output port value. When the platform A20 line is observable via
        // `SystemControlSink::a20_enabled`, this keeps snapshots consistent even if the internal
        // latch is stale (e.g. A20 toggled via port 0x92).
        w.field_u8(TAG_OUTPUT_PORT, self.output_port_for_guest());
        w.field_u64(TAG_DROPPED_OUTPUT_BYTES, self.dropped_output_bytes);

        if let Some(out) = self.output_buffer {
            let source = match out.source {
                OutputSource::Keyboard => 1u8,
                OutputSource::Mouse => 2u8,
                OutputSource::Controller => 3u8,
            };
            w.field_bytes(
                TAG_OUTPUT_BUFFER,
                Encoder::new().u8(out.value).u8(source).finish(),
            );
        }

        let pending: Vec<(u8, u8)> = self
            .pending_output
            .iter()
            .map(|b| {
                let source = match b.source {
                    OutputSource::Keyboard => 1u8,
                    OutputSource::Mouse => 2u8,
                    OutputSource::Controller => 3u8,
                };
                (b.value, source)
            })
            .collect();
        let mut pending_enc = Encoder::new().u32(pending.len() as u32);
        for (value, source) in pending {
            pending_enc = pending_enc.u8(value).u8(source);
        }
        w.field_bytes(TAG_PENDING_OUTPUT, pending_enc.finish());

        let pending_write = match self.pending_write {
            None => 0u8,
            Some(PendingWrite::CommandByte) => 1,
            Some(PendingWrite::OutputPort) => 3,
            Some(PendingWrite::WriteToMouse) => 2,
            Some(PendingWrite::WriteToOutputBufferKeyboard) => 4,
            Some(PendingWrite::WriteToOutputBufferMouse) => 5,
        };
        w.field_u8(TAG_PENDING_WRITE, pending_write);

        w.field_bool(TAG_LAST_WRITE_WAS_CMD, self.last_write_was_command);

        // Nested snapshots for the PS/2 devices (versioned independently).
        w.field_bytes(TAG_KEYBOARD, self.keyboard.save_state());
        w.field_bytes(TAG_MOUSE, self.mouse.save_state());

        w.field_bytes(
            TAG_TRANSLATOR,
            Encoder::new()
                .bool(self.translator.saw_e0)
                .bool(self.translator.saw_f0)
                .finish(),
        );
        w.field_bool(TAG_PREFER_MOUSE, self.prefer_mouse);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_REGS: u16 = 1;
        const TAG_OUTPUT_BUFFER: u16 = 2;
        const TAG_PENDING_OUTPUT: u16 = 3;
        const TAG_PENDING_WRITE: u16 = 4;
        const TAG_LAST_WRITE_WAS_CMD: u16 = 5;
        const TAG_KEYBOARD: u16 = 6;
        const TAG_MOUSE: u16 = 7;
        const TAG_TRANSLATOR: u16 = 8;
        const TAG_PREFER_MOUSE: u16 = 9;
        const TAG_OUTPUT_PORT: u16 = 10;
        const TAG_DROPPED_OUTPUT_BYTES: u16 = 11;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Start from a deterministic baseline for forward-compatible snapshots that may omit fields.
        self.status = STATUS_SYS;
        self.command_byte = 0x45;
        self.output_port = OUTPUT_PORT_RESET;
        self.pending_output.clear();
        self.dropped_output_bytes = 0;
        self.pending_write = None;
        self.last_write_was_command = false;
        self.translator = Set2ToSet1::default();
        self.prefer_mouse = false;

        if let Some(buf) = r.bytes(TAG_REGS) {
            let mut d = Decoder::new(buf);
            self.status = d.u8()?;
            self.command_byte = d.u8()?;
            d.finish()?;
        }

        if let Some(port) = r.u8(TAG_OUTPUT_PORT)? {
            self.output_port = port;
        }

        self.dropped_output_bytes = r.u64(TAG_DROPPED_OUTPUT_BYTES)?.unwrap_or(0);

        self.output_buffer = if let Some(buf) = r.bytes(TAG_OUTPUT_BUFFER) {
            let mut d = Decoder::new(buf);
            let value = d.u8()?;
            let source = match d.u8()? {
                1 => OutputSource::Keyboard,
                2 => OutputSource::Mouse,
                _ => OutputSource::Controller,
            };
            d.finish()?;
            Some(OutputByte { value, source })
        } else {
            None
        };

        self.pending_output.clear();
        if let Some(buf) = r.bytes(TAG_PENDING_OUTPUT) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            // Pending output entries are fixed-width (value + source). If a snapshot contains more
            // entries than the runtime queue supports, skip the oldest entries in bulk so restore
            // time stays bounded.
            let drop = count.saturating_sub(MAX_PENDING_OUTPUT);
            if drop != 0 {
                let drop_bytes = drop
                    .checked_mul(2)
                    .ok_or(SnapshotError::InvalidFieldEncoding("pending output"))?;
                d.bytes(drop_bytes)?;
            }
            for _ in 0..count.min(MAX_PENDING_OUTPUT) {
                let value = d.u8()?;
                let source = match d.u8()? {
                    1 => OutputSource::Keyboard,
                    2 => OutputSource::Mouse,
                    _ => OutputSource::Controller,
                };
                self.push_pending_output(OutputByte { value, source });
            }
            d.finish()?;
        }

        self.pending_write = match r.u8(TAG_PENDING_WRITE)?.unwrap_or(0) {
            1 => Some(PendingWrite::CommandByte),
            2 => Some(PendingWrite::WriteToMouse),
            3 => Some(PendingWrite::OutputPort),
            4 => Some(PendingWrite::WriteToOutputBufferKeyboard),
            5 => Some(PendingWrite::WriteToOutputBufferMouse),
            _ => None,
        };

        self.last_write_was_command = r.bool(TAG_LAST_WRITE_WAS_CMD)?.unwrap_or(false);

        if let Some(buf) = r.bytes(TAG_KEYBOARD) {
            self.keyboard.load_state(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_MOUSE) {
            self.mouse.load_state(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_TRANSLATOR) {
            let mut d = Decoder::new(buf);
            self.translator.saw_e0 = d.bool()?;
            self.translator.saw_f0 = d.bool()?;
            d.finish()?;
        } else {
            self.translator = Set2ToSet1::default();
        }

        self.prefer_mouse = r.bool(TAG_PREFER_MOUSE)?.unwrap_or(false);

        // Snapshots may be untrusted/corrupt. Keep the i8042 status register coherent with the
        // restored buffers so guests do not observe an "output buffer full" state without any
        // readable data (which would otherwise cause busy-loops).
        //
        // For valid snapshots this is a no-op because the bits are already consistent.
        const STATUS_KNOWN_MASK: u8 =
            STATUS_OBF | STATUS_IBF | STATUS_SYS | STATUS_A2 | STATUS_AUX_OBF;
        self.status &= STATUS_KNOWN_MASK;
        // The input buffer fullness is transient in this device model (writes are processed
        // synchronously), so clear it to avoid a corrupted snapshot permanently wedging the guest.
        self.status &= !STATUS_IBF;
        // The system bit should always be set once the controller has initialized.
        self.status |= STATUS_SYS;
        // Derive OBF/AUX bits from the restored output buffer.
        self.status &= !(STATUS_OBF | STATUS_AUX_OBF | STATUS_A2);
        if let Some(out) = self.output_buffer {
            self.status |= STATUS_OBF;
            if matches!(out.source, OutputSource::Mouse) {
                self.status |= STATUS_AUX_OBF;
            }
        }

        // `irq_sink` and `sys_ctrl` are host integration points; they are expected to be
        // (re)attached by the coordinator. If `sys_ctrl` is already attached, resynchronize the
        // platform A20 line with the restored output-port image.
        if let Some(sink) = self.sys_ctrl.as_deref_mut() {
            let enabled = (self.output_port & OUTPUT_PORT_A20) != 0;
            if sink.a20_enabled() != Some(enabled) {
                sink.set_a20(enabled);
            }
        }

        // Ensure output-buffer bits are coherent with the restored buffer source.
        if let Some(out) = self.output_buffer {
            self.status |= STATUS_OBF;
            match out.source {
                OutputSource::Mouse => self.status |= STATUS_AUX_OBF,
                _ => self.status &= !STATUS_AUX_OBF,
            }
        } else {
            self.status &= !STATUS_OBF;
            self.status &= !STATUS_AUX_OBF;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn pending_output_queue_is_bounded_during_runtime() {
        let mut dev = I8042Controller::new();

        // Fill the output buffer so subsequent controller writes accumulate in the pending queue.
        dev.write_port(0x64, 0xD2);
        dev.write_port(0x60, 0xAA);
        assert!(dev.output_buffer.is_some());

        for i in 0..(MAX_PENDING_OUTPUT + 10) {
            dev.write_port(0x64, 0xD2);
            dev.write_port(0x60, i as u8);
        }

        assert_eq!(dev.pending_output.len(), MAX_PENDING_OUTPUT);
        assert_eq!(dev.dropped_output_bytes(), 10);
        assert_eq!(dev.pending_output.front().unwrap().value, 10);
        assert_eq!(
            dev.pending_output.back().unwrap().value,
            (MAX_PENDING_OUTPUT + 9) as u8
        );
    }

    #[test]
    fn snapshot_restore_truncates_oversized_pending_output_queue() {
        const TAG_PENDING_OUTPUT: u16 = 3;

        let count = MAX_PENDING_OUTPUT + 10;
        let mut enc = Encoder::new().u32(count as u32);
        for i in 0..count {
            enc = enc.u8(i as u8).u8(1);
        }

        let mut w =
            SnapshotWriter::new(I8042Controller::DEVICE_ID, I8042Controller::DEVICE_VERSION);
        w.field_bytes(TAG_PENDING_OUTPUT, enc.finish());

        let mut dev = I8042Controller::new();
        dev.load_state(&w.finish())
            .expect("snapshot restore should succeed");

        assert_eq!(dev.pending_output.len(), MAX_PENDING_OUTPUT);
        assert_eq!(dev.pending_output.front().unwrap().value, 10);
        assert_eq!(
            dev.pending_output.back().unwrap().value,
            (MAX_PENDING_OUTPUT + 9) as u8
        );
    }

    #[test]
    fn snapshot_roundtrip_preserves_dropped_output_bytes() {
        let mut dev = I8042Controller::new();

        // Fill the output buffer so subsequent controller writes accumulate in the pending queue.
        dev.write_port(0x64, 0xD2);
        dev.write_port(0x60, 0xAA);
        assert!(dev.output_buffer.is_some());

        for i in 0..(MAX_PENDING_OUTPUT + 10) {
            dev.write_port(0x64, 0xD2);
            dev.write_port(0x60, i as u8);
        }
        assert_eq!(dev.dropped_output_bytes(), 10);

        let snap = dev.save_state();
        let mut restored = I8042Controller::new();
        restored
            .load_state(&snap)
            .expect("snapshot restore should succeed");

        assert_eq!(restored.dropped_output_bytes(), 10);
    }

    #[test]
    fn irq_pulses_on_output_buffer_refill_after_port60_read() {
        #[derive(Clone)]
        struct TestIrqSink {
            raised: Rc<RefCell<Vec<u8>>>,
        }

        impl IrqSink for TestIrqSink {
            fn raise_irq(&mut self, irq: u8) {
                self.raised.borrow_mut().push(irq);
            }
        }

        let raised = Rc::new(RefCell::new(Vec::new()));
        let mut dev = I8042Controller::new();
        dev.set_irq_sink(Box::new(TestIrqSink {
            raised: raised.clone(),
        }));

        // Inject a single key scancode. The controller should load it into the output buffer and
        // emit an IRQ1 pulse immediately.
        dev.inject_key_scancode_bytes(&[0x1c]);
        assert_eq!(&*raised.borrow(), &[1]);
        raised.borrow_mut().clear();

        // Inject another key while the output buffer is still full. No new IRQ should be emitted
        // until the guest reads the first byte.
        dev.inject_key_scancode_bytes(&[0x32]);
        assert!(raised.borrow().is_empty());

        // Read port 0x60 to consume the first byte. The controller should immediately refill the
        // output buffer from the queued key and emit another IRQ1 pulse.
        let _ = dev.read_port(0x60);
        assert_eq!(&*raised.borrow(), &[1]);
        raised.borrow_mut().clear();

        // Reading the final byte should not generate any additional pulses.
        let _ = dev.read_port(0x60);
        assert!(raised.borrow().is_empty());
    }

    #[test]
    fn keyboard_injection_drops_scancodes_when_port_disabled() {
        let mut dev = I8042Controller::new();

        // Disable the keyboard port (0xAD).
        dev.write_port(0x64, 0xAD);

        // Inject a key while the port is disabled. The controller should not buffer it.
        dev.inject_key_scancode_bytes(&[0x1c]);

        assert!(
            !dev.keyboard.has_output(),
            "keyboard should not buffer output"
        );
        assert!(dev.output_buffer.is_none());
        assert_eq!(dev.status & STATUS_OBF, 0);

        // Re-enable the keyboard port (0xAE). No buffered key should appear.
        dev.write_port(0x64, 0xAE);
        assert!(dev.output_buffer.is_none());
        assert_eq!(dev.status & STATUS_OBF, 0);

        // Ensure injection works again when enabled.
        dev.inject_key_scancode_bytes(&[0x1c]);
        assert!(dev.output_buffer.is_some());
        // Translation is enabled by default (Set-2 -> Set-1), so 0x1c ("A" in Set-2) becomes 0x1e.
        assert_eq!(dev.read_port(0x60), 0x1e);
    }

    #[test]
    fn snapshot_restore_sanitizes_inconsistent_status_bits() {
        const TAG_REGS: u16 = 1;

        // Corrupt snapshot: claim the output buffer is full, but omit the output buffer field.
        let regs = Encoder::new()
            .u8(STATUS_OBF) // status
            .u8(0x45) // command_byte baseline
            .finish();

        let mut w =
            SnapshotWriter::new(I8042Controller::DEVICE_ID, I8042Controller::DEVICE_VERSION);
        w.field_bytes(TAG_REGS, regs);

        let mut dev = I8042Controller::new();
        dev.load_state(&w.finish())
            .expect("snapshot restore should succeed");

        let status = dev.read_port(0x64);
        assert_eq!(
            status & STATUS_OBF,
            0,
            "expected OBF to be cleared when no output buffer is present"
        );
        assert_ne!(
            status & STATUS_SYS,
            0,
            "expected SYS bit to remain set after restore"
        );
    }
}
