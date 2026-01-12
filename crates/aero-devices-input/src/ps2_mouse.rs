use std::collections::VecDeque;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

const MAX_OUTPUT_BYTES: usize = 4096;

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

    fn push_out(&mut self, byte: u8) {
        if self.out.len() >= MAX_OUTPUT_BYTES {
            let _ = self.out.pop_front();
        }
        self.out.push_back(byte);
    }

    pub fn device_id(&self) -> u8 {
        self.device_id
    }

    pub fn buttons_mask(&self) -> u8 {
        self.buttons
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
                self.push_out(0xFA);
            }
            0xE7 => {
                self.scaling = Scaling::TwoToOne;
                self.push_out(0xFA);
            }
            0xE8 => {
                self.push_out(0xFA);
                self.expecting_data = Some(ExpectingData::Resolution);
            }
            0xE9 => {
                self.push_out(0xFA);
                self.push_out(self.status_byte());
                self.push_out(self.resolution);
                self.push_out(self.sample_rate);
            }
            0xEA => {
                self.mode = Mode::Stream;
                self.push_out(0xFA);
            }
            0xEB => {
                // Read data (remote mode).
                self.push_out(0xFA);
                self.send_packet();
            }
            0xF0 => {
                self.mode = Mode::Remote;
                self.push_out(0xFA);
            }
            0xF2 => {
                self.push_out(0xFA);
                self.push_out(self.device_id);
            }
            0xF3 => {
                self.push_out(0xFA);
                self.expecting_data = Some(ExpectingData::SampleRate);
            }
            0xF4 => {
                self.reporting_enabled = true;
                self.push_out(0xFA);
            }
            0xF5 => {
                self.reporting_enabled = false;
                self.push_out(0xFA);
            }
            0xF6 => {
                self.reset_to_defaults();
                self.push_out(0xFA);
            }
            0xFF => {
                // Reset.
                self.reset_to_defaults();
                self.device_id = 0x00;
                self.sample_rate_seq = [0; 3];
                self.push_out(0xFA);
                self.push_out(0xAA);
                self.push_out(0x00);
            }
            _ => {
                // ACK unknown commands for compatibility.
                self.push_out(0xFA);
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
                self.push_out(0xFA);
            }
            ExpectingData::SampleRate => {
                self.sample_rate = byte;
                self.push_out(0xFA);

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

        self.push_out(b0);
        self.push_out((x_clamped as i16 & 0xFF) as u8);
        self.push_out((y_clamped as i16 & 0xFF) as u8);

        if self.device_id >= 0x03 {
            // IntelliMouse: signed 4-bit wheel delta (-8..7).
            let wheel = self.wheel.clamp(-8, 7) as i8;
            self.push_out((wheel as u8) & 0x0F);
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

impl IoSnapshot for Ps2Mouse {
    const DEVICE_ID: [u8; 4] = *b"MSE0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_CONFIG: u16 = 1;
        const TAG_MOTION: u16 = 2;
        const TAG_SEQ: u16 = 3;
        const TAG_EXPECTING: u16 = 4;
        const TAG_OUTPUT: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let config = Encoder::new()
            .u8(match self.mode {
                Mode::Stream => 1,
                Mode::Remote => 2,
            })
            .u8(match self.scaling {
                Scaling::OneToOne => 1,
                Scaling::TwoToOne => 2,
            })
            .u8(self.resolution)
            .u8(self.sample_rate)
            .bool(self.reporting_enabled)
            .u8(self.device_id)
            .u8(self.buttons)
            .finish();
        w.field_bytes(TAG_CONFIG, config);

        let motion = Encoder::new()
            .i32(self.dx)
            .i32(self.dy)
            .i32(self.wheel)
            .finish();
        w.field_bytes(TAG_MOTION, motion);

        w.field_bytes(TAG_SEQ, self.sample_rate_seq.to_vec());

        let expecting = match self.expecting_data {
            None => 0u8,
            Some(ExpectingData::Resolution) => 1,
            Some(ExpectingData::SampleRate) => 2,
        };
        w.field_u8(TAG_EXPECTING, expecting);

        let out: Vec<u8> = self.out.iter().copied().collect();
        w.field_bytes(TAG_OUTPUT, Encoder::new().vec_u8(&out).finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_CONFIG: u16 = 1;
        const TAG_MOTION: u16 = 2;
        const TAG_SEQ: u16 = 3;
        const TAG_EXPECTING: u16 = 4;
        const TAG_OUTPUT: u16 = 5;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_CONFIG) {
            let mut d = Decoder::new(buf);
            self.mode = match d.u8()? {
                2 => Mode::Remote,
                _ => Mode::Stream,
            };
            self.scaling = match d.u8()? {
                2 => Scaling::TwoToOne,
                _ => Scaling::OneToOne,
            };
            self.resolution = d.u8()?;
            self.sample_rate = d.u8()?;
            self.reporting_enabled = d.bool()?;
            self.device_id = d.u8()?;
            self.buttons = d.u8()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_MOTION) {
            let mut d = Decoder::new(buf);
            self.dx = d.i32()?;
            self.dy = d.i32()?;
            self.wheel = d.i32()?;
            d.finish()?;
        }

        if let Some(seq) = r.bytes(TAG_SEQ) {
            if seq.len() == 3 {
                self.sample_rate_seq.copy_from_slice(seq);
            }
        }

        self.expecting_data = match r.u8(TAG_EXPECTING)?.unwrap_or(0) {
            1 => Some(ExpectingData::Resolution),
            2 => Some(ExpectingData::SampleRate),
            _ => None,
        };

        self.out.clear();
        if let Some(buf) = r.bytes(TAG_OUTPUT) {
            let mut d = Decoder::new(buf);
            let len = d.u32()? as usize;
            let bytes = d.bytes(len)?;
            d.finish()?;

            let drop = len.saturating_sub(MAX_OUTPUT_BYTES);
            for &byte in bytes.iter().skip(drop) {
                self.push_out(byte);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_queue_is_bounded_during_runtime() {
        let mut m = Ps2Mouse::new();
        for _ in 0..(MAX_OUTPUT_BYTES + 10) {
            m.receive_byte(0xE6);
        }
        let drained: Vec<u8> = std::iter::from_fn(|| m.pop_output()).collect();
        assert_eq!(drained.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn snapshot_restore_truncates_oversized_output_queue() {
        const TAG_OUTPUT: u16 = 5;

        let bytes: Vec<u8> = (0..(MAX_OUTPUT_BYTES as u32 + 10))
            .map(|v| v as u8)
            .collect();

        let mut w = SnapshotWriter::new(Ps2Mouse::DEVICE_ID, Ps2Mouse::DEVICE_VERSION);
        w.field_bytes(TAG_OUTPUT, Encoder::new().vec_u8(&bytes).finish());

        let mut m = Ps2Mouse::new();
        m.load_state(&w.finish())
            .expect("snapshot restore should succeed");

        let drained: Vec<u8> = std::iter::from_fn(|| m.pop_output()).collect();
        let drop = bytes.len() - MAX_OUTPUT_BYTES;
        assert_eq!(drained, bytes[drop..]);
    }

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
