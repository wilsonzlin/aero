use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::chipset::A20GateHandle;
use aero_platform::io::PortIoDevice;
use aero_platform::reset::{PlatformResetSink, ResetKind};

/// I/O port for the "fast A20 gate" latch (System Control Port A).
///
/// On PC-compatible hardware, port `0x92` commonly exposes:
/// - bit 1: A20 enable
/// - bit 0: reset pulse (self-clearing in this emulation)
pub const A20_GATE_PORT: u16 = 0x92;

pub struct A20Gate {
    a20: A20GateHandle,
    reset: Option<Box<dyn PlatformResetSink>>,
    value: u8,
}

impl IoSnapshot for A20Gate {
    const DEVICE_ID: [u8; 4] = *b"A20G";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_VALUE: u16 = 1;
        const TAG_A20_ENABLED: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u8(TAG_VALUE, self.value);
        w.field_bool(TAG_A20_ENABLED, self.a20.enabled());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_VALUE: u16 = 1;
        const TAG_A20_ENABLED: u16 = 2;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Deterministic baseline.
        self.value = 0;
        self.a20.set_enabled(false);

        let value = r.u8(TAG_VALUE)?.unwrap_or(0) & !0x01;
        let enabled = r.bool(TAG_A20_ENABLED)?.unwrap_or((value & 0x02) != 0);

        self.value = value;
        self.a20.set_enabled(enabled);

        // `reset` is a host integration point; it is expected to be (re)attached by the coordinator.
        Ok(())
    }
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
    fn read(&mut self, _port: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        self.read_value() as u32
    }

    fn write(&mut self, _port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use aero_platform::chipset::ChipsetState;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn port_io_size0_is_noop() {
        let chipset = ChipsetState::new(false);
        let a20 = chipset.a20();

        let reset_called = Rc::new(Cell::new(false));
        let reset_called_clone = reset_called.clone();
        let mut dev = A20Gate::with_reset_sink(a20.clone(), move |_kind| {
            reset_called_clone.set(true);
        });

        // Size-0 writes must not trigger a reset pulse or update A20 state.
        dev.write(A20_GATE_PORT, 0, 0x03);
        assert!(!reset_called.get());
        assert!(!a20.enabled());
        assert_eq!(dev.read(A20_GATE_PORT, 1), 0);

        // Size-0 reads must return 0.
        assert_eq!(dev.read(A20_GATE_PORT, 0), 0);
    }
}
