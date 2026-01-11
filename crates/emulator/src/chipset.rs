use crate::io::input::i8042::I8042Callbacks;

/// Minimal chipset/core glue state for i8042 output port side effects.
#[derive(Debug, Default)]
pub struct ChipsetControl {
    pub a20_enabled: bool,
    pub reset_requested: bool,
}

impl I8042Callbacks for ChipsetControl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn request_reset(&mut self) {
        self.reset_requested = true;
    }
}
