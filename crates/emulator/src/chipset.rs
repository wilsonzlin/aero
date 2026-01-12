use aero_devices_input::SystemControlSink;

/// Minimal chipset/core glue state for i8042 output port side effects.
#[derive(Debug, Default)]
pub struct ChipsetControl {
    pub a20_enabled: bool,
    pub reset_requested: bool,
}

impl SystemControlSink for ChipsetControl {
    fn set_a20(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn request_reset(&mut self) {
        self.reset_requested = true;
    }

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20_enabled)
    }
}
