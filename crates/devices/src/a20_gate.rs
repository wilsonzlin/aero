use aero_platform::chipset::A20GateHandle;
use aero_platform::io::PortIoDevice;
use aero_platform::reset::{PlatformResetSink, ResetKind};

pub struct A20Gate {
    a20: A20GateHandle,
    reset: Option<Box<dyn PlatformResetSink>>,
    value: u8,
}

impl A20Gate {
    pub fn new(a20: A20GateHandle) -> Self {
        let value = if a20.enabled() { 0x02 } else { 0x00 };
        Self {
            a20,
            reset: None,
            value,
        }
    }

    pub fn with_reset_sink(a20: A20GateHandle, reset: impl PlatformResetSink + 'static) -> Self {
        let mut dev = Self::new(a20);
        dev.reset = Some(Box::new(reset));
        dev
    }

    fn read_value(&self) -> u8 {
        let a20_bit = if self.a20.enabled() { 0x02 } else { 0x00 };
        (self.value & !0x02) | a20_bit
    }
}

impl PortIoDevice for A20Gate {
    fn read(&mut self, _port: u16, _size: u8) -> u32 {
        self.read_value() as u32
    }

    fn write(&mut self, _port: u16, _size: u8, value: u32) {
        let value = value as u8;
        if (value & 0x01) != 0 {
            if let Some(reset) = self.reset.as_mut() {
                reset.request_reset(ResetKind::System);
            }
        }

        self.a20.set_enabled((value & 0x02) != 0);

        // Preserve other bits, but treat bit0 as a pulse (self-clearing).
        self.value = value & !0x01;
    }

    fn reset(&mut self) {
        // Power-on value: A20 gate follows the chipset state; reset bit is cleared.
        self.value = if self.a20.enabled() { 0x02 } else { 0x00 };
    }
}
