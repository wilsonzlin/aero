use std::collections::VecDeque;

const KBD_ACK: u8 = 0xFA;
const KBD_SELF_TEST_OK: u8 = 0xAA;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCommand {
    SetLeds,
    SetScancodeSet,
    SetTypematic,
}

/// Minimal PS/2 keyboard device model.
///
/// This model focuses on the subset of behaviour required for BIOS and Windows
/// guests, notably scan code set handling and common command responses.
#[derive(Debug, Default)]
pub struct Ps2Keyboard {
    scancode_set: u8,
    leds: u8,
    typematic: u8,
    scanning_enabled: bool,
    pending: Option<PendingCommand>,
    output: VecDeque<u8>,
}

impl Ps2Keyboard {
    pub fn new() -> Self {
        Self {
            scancode_set: 2,
            leds: 0,
            typematic: 0,
            scanning_enabled: true,
            pending: None,
            output: VecDeque::new(),
        }
    }

    pub fn scancode_set(&self) -> u8 {
        self.scancode_set
    }

    pub fn leds(&self) -> u8 {
        self.leds
    }

    pub fn scanning_enabled(&self) -> bool {
        self.scanning_enabled
    }

    pub fn receive_byte(&mut self, byte: u8) {
        if let Some(pending) = self.pending.take() {
            self.handle_pending_data(pending, byte);
            return;
        }

        match byte {
            0xED => {
                // Set LEDs
                self.output.push_back(KBD_ACK);
                self.pending = Some(PendingCommand::SetLeds);
            }
            0xF2 => {
                // Identify (MF2 keyboard)
                self.output.push_back(KBD_ACK);
                self.output.push_back(0xAB);
                self.output.push_back(0x83);
            }
            0xF0 => {
                // Get/Set scan code set
                self.output.push_back(KBD_ACK);
                self.pending = Some(PendingCommand::SetScancodeSet);
            }
            0xF3 => {
                // Set typematic rate/delay
                self.output.push_back(KBD_ACK);
                self.pending = Some(PendingCommand::SetTypematic);
            }
            0xF4 => {
                // Enable scanning
                self.scanning_enabled = true;
                self.output.push_back(KBD_ACK);
            }
            0xF5 => {
                // Disable scanning
                self.scanning_enabled = false;
                self.output.push_back(KBD_ACK);
            }
            0xFF => {
                // Reset
                self.reset_to_defaults();
                self.output.push_back(KBD_ACK);
                self.output.push_back(KBD_SELF_TEST_OK);
            }
            // Unknown / unimplemented commands are ACKed to keep guests moving.
            _ => {
                self.output.push_back(KBD_ACK);
            }
        }
    }

    fn handle_pending_data(&mut self, pending: PendingCommand, byte: u8) {
        match pending {
            PendingCommand::SetLeds => {
                self.leds = byte & 0x07;
                self.output.push_back(KBD_ACK);
            }
            PendingCommand::SetScancodeSet => {
                self.output.push_back(KBD_ACK);
                match byte {
                    0 => self.output.push_back(self.scancode_set),
                    1 | 2 | 3 => self.scancode_set = byte,
                    _ => {}
                }
            }
            PendingCommand::SetTypematic => {
                self.typematic = byte;
                self.output.push_back(KBD_ACK);
            }
        }
    }

    fn reset_to_defaults(&mut self) {
        self.scancode_set = 2;
        self.leds = 0;
        self.typematic = 0;
        // After reset, scanning is disabled until the guest enables it.
        self.scanning_enabled = false;
        self.pending = None;
    }

    pub fn key_event(&mut self, scancode: u8, pressed: bool, extended: bool) {
        if !self.scanning_enabled || scancode == 0 {
            return;
        }

        match self.scancode_set {
            1 => self.enqueue_set1(scancode, pressed, extended),
            2 => self.enqueue_set2(scancode, pressed, extended),
            _ => {}
        }
    }

    fn enqueue_set1(&mut self, scancode: u8, pressed: bool, extended: bool) {
        if extended {
            self.output.push_back(0xE0);
        }

        if pressed {
            self.output.push_back(scancode);
        } else {
            self.output.push_back(scancode | 0x80);
        }
    }

    fn enqueue_set2(&mut self, scancode: u8, pressed: bool, extended: bool) {
        if extended {
            self.output.push_back(0xE0);
        }

        if pressed {
            self.output.push_back(scancode);
        } else {
            self.output.push_back(0xF0);
            self.output.push_back(scancode);
        }
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
    fn reset_returns_ack_then_self_test_ok() {
        let mut kbd = Ps2Keyboard::new();
        kbd.receive_byte(0xFF);
        assert_eq!(kbd.drain_output(), vec![KBD_ACK, KBD_SELF_TEST_OK]);
        assert_eq!(kbd.scancode_set(), 2);
        assert!(!kbd.scanning_enabled());
    }

    #[test]
    fn make_break_set2_and_extended() {
        let mut kbd = Ps2Keyboard::new();

        // Enable scanning (and discard ACK).
        kbd.receive_byte(0xF4);
        kbd.drain_output();

        // Normal key.
        kbd.key_event(0x1C, true, false);
        assert_eq!(kbd.drain_output(), vec![0x1C]);

        kbd.key_event(0x1C, false, false);
        assert_eq!(kbd.drain_output(), vec![0xF0, 0x1C]);

        // Extended key.
        kbd.key_event(0x74, true, true);
        assert_eq!(kbd.drain_output(), vec![0xE0, 0x74]);

        kbd.key_event(0x74, false, true);
        assert_eq!(kbd.drain_output(), vec![0xE0, 0xF0, 0x74]);
    }

    #[test]
    fn scanning_disabled_suppresses_key_events() {
        let mut kbd = Ps2Keyboard::new();
        kbd.receive_byte(0xF5);
        kbd.drain_output(); // discard ACK

        kbd.key_event(0x1C, true, false);
        assert_eq!(kbd.drain_output(), Vec::<u8>::new());
    }
}
