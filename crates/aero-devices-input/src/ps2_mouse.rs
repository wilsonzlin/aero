use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ps2MouseButton {
    Left,
    Right,
    Middle,
}

impl Ps2MouseButton {
    fn bit(self) -> u8 {
        match self {
            Ps2MouseButton::Left => 0x01,
            Ps2MouseButton::Right => 0x02,
            Ps2MouseButton::Middle => 0x04,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stream,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scaling {
    OneToOne,
    TwoToOne,
}

#[derive(Debug, Clone, Copy)]
enum ExpectingData {
    Resolution,
    SampleRate,
}

/// Minimal PS/2 mouse supporting stream mode packets and the IntelliMouse wheel
/// extension.
#[derive(Debug)]
pub struct Ps2Mouse {
    mode: Mode,
    scaling: Scaling,
    resolution: u8,
    sample_rate: u8,
    reporting_enabled: bool,
    device_id: u8,

    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,

    sample_rate_seq: [u8; 3],
    expecting_data: Option<ExpectingData>,
    out: VecDeque<u8>,
}

impl Ps2Mouse {
    pub fn new() -> Self {
        Self {
            mode: Mode::Stream,
            scaling: Scaling::OneToOne,
            resolution: 4,
            sample_rate: 100,
            reporting_enabled: false,
            device_id: 0x00,
            buttons: 0,
            dx: 0,
            dy: 0,
            wheel: 0,
            sample_rate_seq: [0; 3],
            expecting_data: None,
            out: VecDeque::new(),
        }
    }

    pub fn has_output(&self) -> bool {
        !self.out.is_empty()
    }

    pub fn pop_output(&mut self) -> Option<u8> {
        self.out.pop_front()
    }

    pub fn device_id(&self) -> u8 {
        self.device_id
    }

    pub fn inject_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        self.dx += dx;
        self.dy += dy;
        self.wheel += wheel;

        if self.mode == Mode::Stream && self.reporting_enabled {
            self.send_packet();
        }
    }

    pub fn inject_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        if pressed {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }

        if self.mode == Mode::Stream && self.reporting_enabled {
            self.send_packet();
        }
    }

    pub fn receive_byte(&mut self, byte: u8) {
        if let Some(expecting) = self.expecting_data.take() {
            self.handle_data_byte(expecting, byte);
            return;
        }

        match byte {
            0xE6 => {
                self.scaling = Scaling::OneToOne;
                self.out.push_back(0xFA);
            }
            0xE7 => {
                self.scaling = Scaling::TwoToOne;
                self.out.push_back(0xFA);
            }
            0xE8 => {
                self.out.push_back(0xFA);
                self.expecting_data = Some(ExpectingData::Resolution);
            }
            0xE9 => {
                self.out.push_back(0xFA);
                self.out.push_back(self.status_byte());
                self.out.push_back(self.resolution);
                self.out.push_back(self.sample_rate);
            }
            0xEA => {
                self.mode = Mode::Stream;
                self.out.push_back(0xFA);
            }
            0xEB => {
                // Read data (remote mode).
                self.out.push_back(0xFA);
                self.send_packet();
            }
            0xF0 => {
                self.mode = Mode::Remote;
                self.out.push_back(0xFA);
            }
            0xF2 => {
                self.out.push_back(0xFA);
                self.out.push_back(self.device_id);
            }
            0xF3 => {
                self.out.push_back(0xFA);
                self.expecting_data = Some(ExpectingData::SampleRate);
            }
            0xF4 => {
                self.reporting_enabled = true;
                self.out.push_back(0xFA);
            }
            0xF5 => {
                self.reporting_enabled = false;
                self.out.push_back(0xFA);
            }
            0xF6 => {
                self.reset_to_defaults();
                self.out.push_back(0xFA);
            }
            0xFF => {
                // Reset.
                self.reset_to_defaults();
                self.device_id = 0x00;
                self.sample_rate_seq = [0; 3];
                self.out.push_back(0xFA);
                self.out.push_back(0xAA);
                self.out.push_back(0x00);
            }
            _ => {
                // ACK unknown commands for compatibility.
                self.out.push_back(0xFA);
            }
        }
    }

    fn reset_to_defaults(&mut self) {
        self.mode = Mode::Stream;
        self.scaling = Scaling::OneToOne;
        self.resolution = 4;
        self.sample_rate = 100;
        self.reporting_enabled = false;
        self.buttons = 0;
        self.dx = 0;
        self.dy = 0;
        self.wheel = 0;
        self.expecting_data = None;
    }

    fn handle_data_byte(&mut self, expecting: ExpectingData, byte: u8) {
        match expecting {
            ExpectingData::Resolution => {
                self.resolution = byte;
                self.out.push_back(0xFA);
            }
            ExpectingData::SampleRate => {
                self.sample_rate = byte;
                self.out.push_back(0xFA);

                self.sample_rate_seq[0] = self.sample_rate_seq[1];
                self.sample_rate_seq[1] = self.sample_rate_seq[2];
                self.sample_rate_seq[2] = byte;

                // IntelliMouse wheel extension sequence: 200, 100, 80.
                if self.sample_rate_seq == [200, 100, 80] {
                    self.device_id = 0x03;
                }
            }
        }
    }

    fn status_byte(&self) -> u8 {
        let mut b = 0x08; // Always 1.
        b |= self.buttons & 0x07;
        if self.scaling == Scaling::TwoToOne {
            b |= 0x10;
        }
        if self.reporting_enabled {
            b |= 0x20;
        }
        if self.mode == Mode::Remote {
            b |= 0x40;
        }
        b
    }

    fn send_packet(&mut self) {
        // PS/2 uses +Y as "up", while browser mousemove uses +Y as "down".
        let x = self.dx;
        let y = -self.dy;

        let mut x_overflow = false;
        let mut y_overflow = false;

        let x_clamped = if x < -256 {
            x_overflow = true;
            -256
        } else if x > 255 {
            x_overflow = true;
            255
        } else {
            x
        };

        let y_clamped = if y < -256 {
            y_overflow = true;
            -256
        } else if y > 255 {
            y_overflow = true;
            255
        } else {
            y
        };

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

        self.out.push_back(b0);
        self.out.push_back((x_clamped as i16 & 0xFF) as u8);
        self.out.push_back((y_clamped as i16 & 0xFF) as u8);

        if self.device_id >= 0x03 {
            // IntelliMouse: signed 4-bit wheel delta (-8..7).
            let wheel = self.wheel.clamp(-8, 7) as i8;
            self.out.push_back((wheel as u8) & 0x0F);
        }

        self.dx = 0;
        self.dy = 0;
        self.wheel = 0;
    }
}

impl Default for Ps2Mouse {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_packet_inverts_y() {
        let mut m = Ps2Mouse::new();
        m.receive_byte(0xF4); // enable reporting
        assert_eq!(m.pop_output(), Some(0xFA));

        m.inject_motion(10, 5, 0);
        let b0 = m.pop_output().unwrap();
        let b1 = m.pop_output().unwrap();
        let b2 = m.pop_output().unwrap();

        assert_eq!(b0, 0x28); // bit3=1, y sign set.
        assert_eq!(b1, 10);
        assert_eq!(b2, 0xFB); // -5
    }

    #[test]
    fn wheel_extension_adds_fourth_byte() {
        let mut m = Ps2Mouse::new();

        // Enable IntelliMouse wheel.
        m.receive_byte(0xF3);
        m.receive_byte(200);
        m.receive_byte(0xF3);
        m.receive_byte(100);
        m.receive_byte(0xF3);
        m.receive_byte(80);

        while m.pop_output().is_some() {}
        assert_eq!(m.device_id(), 0x03);

        m.receive_byte(0xF4);
        assert_eq!(m.pop_output(), Some(0xFA));

        m.inject_motion(0, 0, 1);
        let packet: Vec<u8> = std::iter::from_fn(|| m.pop_output()).take(4).collect();
        assert_eq!(packet.len(), 4);
        assert_eq!(packet[0] & 0x08, 0x08);
        assert_eq!(packet[3], 0x01);
    }
}
