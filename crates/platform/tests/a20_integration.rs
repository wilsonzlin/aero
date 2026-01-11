use aero_bios::firmware::{A20Gate as BiosA20Gate, BlockDevice, DiskError, Memory, NullKeyboard};
use aero_bios::{Bios, BiosConfig, RealModeCpu};
use aero_devices::a20_gate::A20Gate as Port92A20Gate;
use aero_devices::i8042::{I8042Ports, PlatformSystemControlSink};
use aero_platform::{A20GateHandle, Platform};

struct DummyMemory;

impl Memory for DummyMemory {
    fn read_u8(&self, _paddr: u32) -> u8 {
        0
    }

    fn read_u16(&self, _paddr: u32) -> u16 {
        0
    }

    fn read_u32(&self, _paddr: u32) -> u32 {
        0
    }

    fn write_u8(&mut self, _paddr: u32, _v: u8) {}

    fn write_u16(&mut self, _paddr: u32, _v: u16) {}

    fn write_u32(&mut self, _paddr: u32, _v: u32) {}
}

struct DummyDisk;

impl BlockDevice for DummyDisk {
    fn read_sector(&mut self, _lba: u64, _buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        Err(DiskError::OutOfRange)
    }

    fn write_sector(&mut self, _lba: u64, _buf512: &[u8; 512]) -> Result<(), DiskError> {
        Err(DiskError::OutOfRange)
    }

    fn sector_count(&self) -> u64 {
        0
    }
}

struct PlatformA20Gate(A20GateHandle);

impl BiosA20Gate for PlatformA20Gate {
    fn a20_enabled(&self) -> bool {
        self.0.enabled()
    }

    fn set_a20_enabled(&mut self, enabled: bool) {
        self.0.set_enabled(enabled);
    }
}

#[test]
fn a20_state_is_shared_between_devices_memory_and_bios() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    let a20 = platform.chipset.a20();

    // 1) Register the fast A20 gate latch (port 0x92).
    platform
        .io
        .register(0x92, Box::new(Port92A20Gate::new(a20.clone())));

    // 2) Register the i8042 controller, wiring the output port callbacks to the same A20 handle.
    let i8042 = I8042Ports::new();
    let controller = i8042.controller();
    controller
        .borrow_mut()
        .set_system_control_sink(Box::new(PlatformSystemControlSink::new(a20.clone())));
    platform.io.register(0x60, Box::new(i8042.port60()));
    platform.io.register(0x64, Box::new(i8042.port64()));

    // 3) Wire BIOS INT 15h A20 services to the platform A20 line.
    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    bios.set_a20_gate(Box::new(PlatformA20Gate(a20.clone())));

    let mut cpu = RealModeCpu::default();
    let mut mem = DummyMemory;
    let mut disk = DummyDisk;
    let mut kbd = NullKeyboard;

    // Write distinct bytes while A20 is enabled.
    platform.io.write_u8(0x92, 0x02);
    platform.memory.write_u8(0x0, 0x11);
    platform.memory.write_u8(0x1_00000, 0x22);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    // Disable A20 via port 0x92 and verify aliasing.
    platform.io.write_u8(0x92, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    // BIOS query should observe the same state.
    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 0);

    // Enable A20 via the i8042 output port path and verify separation again.
    platform.io.write_u8(0x64, 0xD1);
    platform.io.write_u8(0x60, 0x03);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    // BIOS query should now report enabled.
    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 1);

    // Disable A20 via BIOS INT 15h and verify aliasing again.
    cpu.set_ax(0x2400);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    // Enable A20 via BIOS INT 15h and verify separation.
    cpu.set_ax(0x2401);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 1);
}

