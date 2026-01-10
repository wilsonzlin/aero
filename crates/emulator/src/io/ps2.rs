use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ps2MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Default)]
pub struct Ps2Controller {
    pub keyboard: Ps2Keyboard,
    pub mouse: Ps2Mouse,
}

#[derive(Debug, Default)]
pub struct Ps2Keyboard {
    pub scancodes: VecDeque<u8>,
}

impl Ps2Keyboard {
    pub fn inject_scancode_set2(&mut self, scancode: u8, pressed: bool) {
        if pressed {
            self.scancodes.push_back(scancode);
        } else {
            self.scancodes.push_back(0xF0);
            self.scancodes.push_back(scancode);
        }
    }
}

#[derive(Debug, Default)]
pub struct Ps2Mouse {
    pub moves: VecDeque<(i32, i32)>,
    pub button_events: VecDeque<(Ps2MouseButton, bool)>,
    pub wheel_events: VecDeque<i32>,
}

impl Ps2Mouse {
    pub fn inject_move(&mut self, dx: i32, dy: i32) {
        self.moves.push_back((dx, dy));
    }

    pub fn inject_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        self.button_events.push_back((button, pressed));
    }

    pub fn inject_wheel(&mut self, delta: i32) {
        self.wheel_events.push_back(delta);
    }
}

