use std::collections::VecDeque;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};

use crate::scancode::{push_set2_sequence, Set2Scancode};

#[derive(Debug, Clone, Copy)]
enum ExpectingData {
    LedState,
    Typematic,
    ScancodeSet,
}

/// Minimal PS/2 keyboard device model.
#[derive(Debug)]
pub struct Ps2Keyboard {
    scancode_set: u8,
    leds: u8,
    typematic: u8,
    scanning_enabled: bool,
    expecting_data: Option<ExpectingData>,
    out: VecDeque<u8>,
}

impl Ps2Keyboard {
    pub fn new() -> Self {
        Self {
            scancode_set: 2,
            leds: 0,
            typematic: 0x0B,
            scanning_enabled: true,
            expecting_data: None,
            out: VecDeque::new(),
        }
    }

    pub fn scancode_set(&self) -> u8 {
        self.scancode_set
    }

    pub fn has_output(&self) -> bool {
        !self.out.is_empty()
    }

    pub fn pop_output(&mut self) -> Option<u8> {
        self.out.pop_front()
    }

    pub fn inject_key(&mut self, scancode: Set2Scancode, pressed: bool) {
        if !self.scanning_enabled {
            return;
        }
        if self.scancode_set != 2 {
            // We only generate Set-2 sequences for now; other sets are still
            // accepted via commands so the guest can probe capabilities.
            return;
        }

        let mut seq = Vec::new();
        push_set2_sequence(&mut seq, scancode, pressed);
        self.out.extend(seq);
    }

    /// Receives a byte from the host (guest) over the PS/2 data port.
    pub fn receive_byte(&mut self, byte: u8) {
        if let Some(expecting) = self.expecting_data.take() {
            self.handle_data_byte(expecting, byte);
            return;
        }

        match byte {
            0xED => {
                // Set LEDs (next byte contains LED state).
                self.out.push_back(0xFA);
                self.expecting_data = Some(ExpectingData::LedState);
            }
            0xEE => {
                // Echo.
                self.out.push_back(0xEE);
            }
            0xF0 => {
                // Get/Set scancode set (next byte selects).
                self.out.push_back(0xFA);
                self.expecting_data = Some(ExpectingData::ScancodeSet);
            }
            0xF2 => {
                // Identify.
                self.out.push_back(0xFA);
                // MF2 keyboard ID.
                self.out.push_back(0xAB);
                self.out.push_back(0x83);
            }
            0xF3 => {
                // Set typematic rate/delay.
                self.out.push_back(0xFA);
                self.expecting_data = Some(ExpectingData::Typematic);
            }
            0xF4 => {
                // Enable scanning.
                self.scanning_enabled = true;
                self.out.push_back(0xFA);
            }
            0xF5 => {
                // Disable scanning.
                self.scanning_enabled = false;
                self.out.push_back(0xFA);
            }
            0xF6 => {
                // Set defaults.
                self.scancode_set = 2;
                self.typematic = 0x0B;
                self.leds = 0;
                self.scanning_enabled = true;
                self.out.push_back(0xFA);
            }
            0xFF => {
                // Reset.
                self.scancode_set = 2;
                self.typematic = 0x0B;
                self.leds = 0;
                self.scanning_enabled = true;
                self.out.push_back(0xFA);
                self.out.push_back(0xAA);
            }
            _ => {
                // Most commands are ACKed even if unsupported.
                self.out.push_back(0xFA);
            }
        }
    }

    fn handle_data_byte(&mut self, expecting: ExpectingData, byte: u8) {
        match expecting {
            ExpectingData::LedState => {
                self.leds = byte & 0x07;
                self.out.push_back(0xFA);
            }
            ExpectingData::Typematic => {
                self.typematic = byte;
                self.out.push_back(0xFA);
            }
            ExpectingData::ScancodeSet => {
                if byte == 0 {
                    self.out.push_back(0xFA);
                    self.out.push_back(self.scancode_set);
                    return;
                }
                if (1..=3).contains(&byte) {
                    self.scancode_set = byte;
                }
                self.out.push_back(0xFA);
            }
        }
    }
}

impl Default for Ps2Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for Ps2Keyboard {
    const DEVICE_ID: [u8; 4] = *b"KBD0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_CONFIG: u16 = 1;
        const TAG_EXPECTING: u16 = 2;
        const TAG_OUTPUT: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let config = Encoder::new()
            .u8(self.scancode_set)
            .u8(self.leds)
            .u8(self.typematic)
            .bool(self.scanning_enabled)
            .finish();
        w.field_bytes(TAG_CONFIG, config);

        let expecting = match self.expecting_data {
            None => 0u8,
            Some(ExpectingData::LedState) => 1,
            Some(ExpectingData::Typematic) => 2,
            Some(ExpectingData::ScancodeSet) => 3,
        };
        w.field_u8(TAG_EXPECTING, expecting);

        let out: Vec<u8> = self.out.iter().copied().collect();
        w.field_bytes(TAG_OUTPUT, Encoder::new().vec_u8(&out).finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_CONFIG: u16 = 1;
        const TAG_EXPECTING: u16 = 2;
        const TAG_OUTPUT: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_CONFIG) {
            let mut d = Decoder::new(buf);
            self.scancode_set = d.u8()?;
            self.leds = d.u8()?;
            self.typematic = d.u8()?;
            self.scanning_enabled = d.bool()?;
            d.finish()?;
        }

        self.expecting_data = match r.u8(TAG_EXPECTING)?.unwrap_or(0) {
            0 => None,
            1 => Some(ExpectingData::LedState),
            2 => Some(ExpectingData::Typematic),
            3 => Some(ExpectingData::ScancodeSet),
            _ => None,
        };

        self.out.clear();
        if let Some(buf) = r.bytes(TAG_OUTPUT) {
            let mut d = Decoder::new(buf);
            for byte in d.vec_u8()? {
                self.out.push_back(byte);
            }
            d.finish()?;
        }

        Ok(())
    }
}
