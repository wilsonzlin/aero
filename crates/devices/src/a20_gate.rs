use aero_platform::chipset::A20GateHandle;
use aero_platform::io::PortIoDevice;

pub struct A20Gate {
    a20: A20GateHandle,
    reset: Option<Box<dyn FnMut()>>,
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

    pub fn with_reset_callback(a20: A20GateHandle, reset: Box<dyn FnMut()>) -> Self {
        let mut dev = Self::new(a20);
        dev.reset = Some(reset);
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
                reset();
            }
        }

        self.a20.set_enabled((value & 0x02) != 0);

        // Preserve other bits, but treat bit0 as a pulse (self-clearing).
        self.value = value & !0x01;
    }
}
