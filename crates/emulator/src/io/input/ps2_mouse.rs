use std::collections::VecDeque;

const MOUSE_ACK: u8 = 0xFA;
const MOUSE_SELF_TEST_OK: u8 = 0xAA;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCommand {
    SetSampleRate,
    SetResolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stream,
    Remote,
}

/// Minimal PS/2 mouse device model.
///
/// Produces 3-byte movement packets (plus an optional 4th wheel byte when in
/// IntelliMouse mode).
#[derive(Debug)]
pub struct Ps2Mouse {
    mode: Mode,
    reporting_enabled: bool,
    resolution: u8,
    sample_rate: u8,
    device_id: u8,
    buttons: u8,
    dx: i32,
    dy: i32,
    dz: i32,
    pending: Option<PendingCommand>,
    output: VecDeque<u8>,
    recent_sample_rates: VecDeque<u8>,
}

impl Default for Ps2Mouse {
    fn default() -> Self {
        Self::new()
    }
}

impl Ps2Mouse {
    pub fn new() -> Self {
        Self {
            mode: Mode::Stream,
            reporting_enabled: false,
            resolution: 4,
            sample_rate: 100,
            device_id: 0x00,
            buttons: 0,
            dx: 0,
            dy: 0,
            dz: 0,
            pending: None,
            output: VecDeque::new(),
            recent_sample_rates: VecDeque::new(),
        }
    }

    pub fn device_id(&self) -> u8 {
        self.device_id
    }

    pub fn reporting_enabled(&self) -> bool {
        self.reporting_enabled
    }

    pub fn receive_byte(&mut self, byte: u8) {
        if let Some(pending) = self.pending.take() {
            self.handle_pending_data(pending, byte);
            return;
        }

        match byte {
            0xE8 => {
                // Set resolution (next byte)
                self.output.push_back(MOUSE_ACK);
                self.pending = Some(PendingCommand::SetResolution);
            }
            0xE9 => {
                // Status request
                self.output.push_back(MOUSE_ACK);
                self.output.push_back(self.status_byte());
                self.output.push_back(self.resolution);
                self.output.push_back(self.sample_rate);
            }
            0xEA => {
                // Set stream mode
                self.mode = Mode::Stream;
                self.output.push_back(MOUSE_ACK);
            }
            0xEB => {
                // Read data (remote mode); spec returns ACK then packet.
                self.output.push_back(MOUSE_ACK);
                self.enqueue_movement_packet();
            }
            0xF0 => {
                // Set remote mode
                self.mode = Mode::Remote;
                self.output.push_back(MOUSE_ACK);
            }
            0xF2 => {
                // Get device ID
                self.output.push_back(MOUSE_ACK);
                self.output.push_back(self.device_id);
            }
            0xF3 => {
                // Set sample rate (next byte)
                self.output.push_back(MOUSE_ACK);
                self.pending = Some(PendingCommand::SetSampleRate);
            }
            0xF4 => {
                // Enable data reporting
                self.reporting_enabled = true;
                self.output.push_back(MOUSE_ACK);
            }
            0xF5 => {
                // Disable data reporting
                self.reporting_enabled = false;
                self.output.push_back(MOUSE_ACK);
            }
            0xFF => {
                // Reset
                self.reset_to_defaults();
                self.output.push_back(MOUSE_ACK);
                self.output.push_back(MOUSE_SELF_TEST_OK);
                self.output.push_back(self.device_id);
            }
            _ => {
                // Keep guests moving by ACKing unknown commands.
                self.output.push_back(MOUSE_ACK);
            }
        }
    }

    fn handle_pending_data(&mut self, pending: PendingCommand, byte: u8) {
        match pending {
            PendingCommand::SetSampleRate => {
                self.sample_rate = byte;
                self.record_sample_rate_for_intellimouse(byte);
                self.output.push_back(MOUSE_ACK);
            }
            PendingCommand::SetResolution => {
                self.resolution = byte & 0x03;
                self.output.push_back(MOUSE_ACK);
            }
        }
    }

    fn record_sample_rate_for_intellimouse(&mut self, rate: u8) {
        self.recent_sample_rates.push_back(rate);
        while self.recent_sample_rates.len() > 3 {
            self.recent_sample_rates.pop_front();
        }
        if self.recent_sample_rates.len() == 3
            && self.recent_sample_rates[0] == 200
            && self.recent_sample_rates[1] == 100
            && self.recent_sample_rates[2] == 80
        {
            self.device_id = 0x03;
        }
    }

    fn reset_to_defaults(&mut self) {
        self.mode = Mode::Stream;
        self.reporting_enabled = false;
        self.resolution = 4;
        self.sample_rate = 100;
        self.device_id = 0x00;
        self.buttons = 0;
        self.dx = 0;
        self.dy = 0;
        self.dz = 0;
        self.pending = None;
        self.recent_sample_rates.clear();
    }

    fn status_byte(&self) -> u8 {
        // Bit 5 is always set for a standard mouse.
        let mut st = 0x00;
        if self.buttons & 0x01 != 0 {
            st |= 0x01;
        }
        if self.buttons & 0x02 != 0 {
            st |= 0x02;
        }
        if self.buttons & 0x04 != 0 {
            st |= 0x04;
        }
        if self.reporting_enabled {
            st |= 0x20;
        }
        st
    }

    pub fn movement(&mut self, dx: i32, dy: i32, dz: i32) {
        self.dx += dx;
        self.dy += dy;
        self.dz += dz;

        if self.mode == Mode::Stream && self.reporting_enabled {
            self.enqueue_movement_packet();
        }
    }

    pub fn button_event(&mut self, button_mask: u8, pressed: bool) {
        if pressed {
            self.buttons |= button_mask & 0x07;
        } else {
            self.buttons &= !(button_mask & 0x07);
        }

        if self.mode == Mode::Stream && self.reporting_enabled {
            self.enqueue_movement_packet();
        }
    }

    fn enqueue_movement_packet(&mut self) {
        // Convert from "screen" coordinates (positive Y down) to PS/2 (positive Y up).
        let x = self.dx;
        let y = -self.dy;

        let x_overflow = x < -256 || x > 255;
        let y_overflow = y < -256 || y > 255;

        let x_clamped = x.clamp(-256, 255);
        let y_clamped = y.clamp(-256, 255);

        let mut b0 = 0x08 | (self.buttons & 0x07);
        if x_clamped < 0 {
            b0 |= 0x10;
        }
        if y_clamped < 0 {
            b0 |= 0x20;
        }
        if x_overflow {
            b0 |= 0x40;
        }
        if y_overflow {
            b0 |= 0x80;
        }

        let b1 = (x_clamped as i16 & 0xFF) as u8;
        let b2 = (y_clamped as i16 & 0xFF) as u8;

        self.output.push_back(b0);
        self.output.push_back(b1);
        self.output.push_back(b2);

        if self.device_id == 0x03 {
            let w = self.dz.clamp(-8, 7);
            self.output.push_back((w as i16 & 0x0F) as u8);
        }

        self.dx = 0;
        self.dy = 0;
        self.dz = 0;
    }

    pub fn pop_output_byte(&mut self) -> Option<u8> {
        self.output.pop_front()
    }

    pub fn drain_output(&mut self) -> Vec<u8> {
        self.output.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_returns_ack_self_test_ok_and_device_id() {
        let mut mouse = Ps2Mouse::new();
        mouse.receive_byte(0xFF);
        assert_eq!(
            mouse.drain_output(),
            vec![MOUSE_ACK, MOUSE_SELF_TEST_OK, 0x00]
        );
        assert!(!mouse.reporting_enabled());
    }

    #[test]
    fn movement_packet_sign_and_overflow() {
        let mut mouse = Ps2Mouse::new();
        mouse.receive_byte(0xF4); // enable reporting
        mouse.drain_output(); // discard ACK

        mouse.movement(5, 7, 0);
        assert_eq!(mouse.drain_output(), vec![0x28, 0x05, 0xF9]);

        mouse.movement(-1, 0, 0);
        assert_eq!(mouse.drain_output(), vec![0x18, 0xFF, 0x00]);

        mouse.movement(300, 0, 0);
        assert_eq!(mouse.drain_output(), vec![0x48, 0xFF, 0x00]);
    }

    #[test]
    fn intellimouse_enable_via_sample_rate_sequence() {
        let mut mouse = Ps2Mouse::new();
        // F3 + rate, repeated.
        for &rate in &[200u8, 100, 80] {
            mouse.receive_byte(0xF3);
            assert_eq!(mouse.pop_output_byte(), Some(MOUSE_ACK));
            mouse.receive_byte(rate);
            assert_eq!(mouse.pop_output_byte(), Some(MOUSE_ACK));
        }
        assert_eq!(mouse.device_id(), 0x03);
    }
}
