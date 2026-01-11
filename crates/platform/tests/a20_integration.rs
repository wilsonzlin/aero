use aero_devices::a20_gate::A20Gate as Port92A20Gate;
use aero_devices::i8042::{I8042Ports, PlatformSystemControlSink};
use aero_platform::Platform;
use firmware::bus::Bus;
use firmware::legacy_bios::{BiosConfig as LegacyBiosConfig, LegacyBios};
use firmware::realmode::RealModeCpu;

struct PlatformBus<'a> {
    platform: &'a mut Platform,
    serial: Vec<u8>,
}

impl<'a> PlatformBus<'a> {
    fn new(platform: &'a mut Platform) -> Self {
        Self {
            platform,
            serial: Vec::new(),
        }
    }
}

impl Bus for PlatformBus<'_> {
    fn read_u8(&mut self, paddr: u32) -> u8 {
        self.platform.memory.read_u8(paddr as u64)
    }

    fn write_u8(&mut self, paddr: u32, val: u8) {
        self.platform.memory.write_u8(paddr as u64, val);
    }

    fn a20_enabled(&self) -> bool {
        self.platform.chipset.a20().enabled()
    }

    fn set_a20_enabled(&mut self, enabled: bool) {
        self.platform.chipset.a20().set_enabled(enabled);
    }

    fn io_read_u8(&mut self, port: u16) -> u8 {
        self.platform.io.read_u8(port)
    }

    fn io_write_u8(&mut self, port: u16, val: u8) {
        self.platform.io.write_u8(port, val)
    }

    fn serial_write(&mut self, byte: u8) {
        self.serial.push(byte);
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

    // 3) Wire BIOS INT15 A20 services through the platform A20 handle by exposing a BIOS bus
    // implementation that reads/writes the same A20 gate state.
    let mut bus = PlatformBus::new(&mut platform);
    let mut bios = LegacyBios::new(LegacyBiosConfig {
        ram_size: 2 * 1024 * 1024,
        // Not used by this test, but required for BIOS construction.
        acpi_base: 0xE0000,
        hpet_base: 0xFED0_0000,
    });
    let mut cpu = RealModeCpu::default();

    // BIOS query should observe the reset default: A20 disabled.
    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.al(), 0);

    // i8042 output port reads should also reflect disabled.
    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x00);

    // Enable A20 via port 0x92 and verify memory separation.
    bus.io_write_u8(0x92, 0x02);
    assert!(bus.a20_enabled());

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.al(), 1);

    bus.write_u8(0x0, 0x11);
    bus.write_u8(0x1_00000, 0x22);
    assert_eq!(bus.read_u8(0x0), 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x22);

    // i8042 output port reads should report the same A20 line state even though we did not
    // write the i8042 output port latch.
    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x02);

    // Disable A20 via port 0x92 and verify aliasing.
    bus.io_write_u8(0x92, 0x00);
    assert!(!bus.a20_enabled());
    assert_eq!(bus.read_u8(0x1_00000), 0x11);
    assert_eq!(bus.io_read_u8(0x92) & 0x02, 0x00);

    // i8042 output port reads should observe the same line state.
    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x00);

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.al(), 0);

    // Enable A20 via the i8042 output port path and verify separation again.
    bus.io_write_u8(0x64, 0xD1);
    bus.io_write_u8(0x60, 0x03);
    assert!(bus.a20_enabled());
    assert_eq!(bus.read_u8(0x0), 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x22);
    assert_eq!(bus.io_read_u8(0x92) & 0x02, 0x02);

    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x02);

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.al(), 1);

    // Disable A20 via BIOS INT 15h and verify aliasing.
    cpu.set_ax(0x2400);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert!(!bus.a20_enabled());
    assert_eq!(bus.read_u8(0x1_00000), 0x11);

    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x00);
    assert_eq!(bus.io_read_u8(0x92) & 0x02, 0x00);

    // Enable A20 via BIOS INT 15h and verify separation.
    cpu.set_ax(0x2401);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert!(bus.a20_enabled());
    assert_eq!(bus.read_u8(0x0), 0x11);
    assert_eq!(bus.read_u8(0x1_00000), 0x22);

    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x02);
    assert_eq!(bus.io_read_u8(0x92) & 0x02, 0x02);

    cpu.set_ax(0x2403);
    bios.handle_interrupt(0x15, &mut bus, &mut cpu);
    assert!(!cpu.carry());
    assert_eq!(cpu.bx(), 0x0007);

    // Disable A20 again and verify aliasing plus coherent device-visible state.
    bus.io_write_u8(0x92, 0x00);
    assert_eq!(bus.read_u8(0x1_00000), 0x11);

    bus.io_write_u8(0x64, 0xD0);
    assert_eq!(bus.io_read_u8(0x60) & 0x02, 0x00);
}
