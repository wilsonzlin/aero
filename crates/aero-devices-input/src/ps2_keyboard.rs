use std::collections::VecDeque;

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
