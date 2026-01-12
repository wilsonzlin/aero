#![allow(deprecated)]
use aero_cpu_core::state::{gpr, CpuMode, CpuState, FLAG_CF, RFLAGS_IF};
use aero_devices::a20_gate::{A20Gate as Port92A20Gate, A20_GATE_PORT};
use aero_devices::i8042::{
    I8042Ports, PlatformSystemControlSink, I8042_DATA_PORT, I8042_STATUS_PORT,
};
use aero_platform::{A20GateHandle, Platform};
use firmware::bios::{Bios, BiosBus, BiosConfig, FirmwareMemory, InMemoryDisk};
use std::sync::Arc;

struct BiosA20Bus {
    a20: A20GateHandle,
    mem: Vec<u8>,
}

impl BiosA20Bus {
    fn new(a20: A20GateHandle, mem_size: usize) -> Self {
        Self {
            a20,
            mem: vec![0; mem_size],
        }
    }
}

impl firmware::bios::A20Gate for BiosA20Bus {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.a20.enabled()
    }
}

impl FirmwareMemory for BiosA20Bus {
    fn map_rom(&mut self, _base: u64, _rom: Arc<[u8]>) {
        // This integration test only exercises INT 15h A20 services, which do not require ROM
        // mapping. A full VM would map the BIOS ROM separately.
    }
}

impl memory::MemoryBus for BiosA20Bus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            *slot = self.mem.get(addr as usize).copied().unwrap_or(0xFF);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = paddr.wrapping_add(i as u64);
            if let Some(slot) = self.mem.get_mut(addr as usize) {
                *slot = byte;
            }
        }
    }
}

fn bios_int15(bios: &mut Bios, bus: &mut dyn BiosBus, cpu: &mut CpuState, ax: u16) -> u16 {
    // `Bios::dispatch_interrupt` expects the CPU to have executed `INT` already. Provide a
    // minimal interrupt frame so the dispatcher can merge the handler flags into the IRET image.
    *cpu = CpuState::new(CpuMode::Real);
    cpu.segments.ss.selector = 0;
    cpu.segments.ss.base = 0;
    cpu.segments.ss.limit = 0xFFFF;
    cpu.segments.ss.access = 0;
    cpu.set_stack_ptr(0x0100);

    bus.write_u16(0x0100, 0); // return IP
    bus.write_u16(0x0102, 0); // return CS
                              // Return FLAGS from the interrupt frame. Real-mode BIOS callers typically have IF=1, and the
                              // dispatcher should preserve IF from this saved image (the CPU clears IF before entering the
                              // handler stub).
    bus.write_u16(0x0104, 0x0202); // return FLAGS (IF=1, bit1 always set)

    cpu.gpr[gpr::RAX] = ax as u64;
    let mut disk = InMemoryDisk::new(vec![0; 512]);
    bios.dispatch_interrupt(0x15, cpu, bus, &mut disk);

    bus.read_u16(0x0104)
}

fn assert_int15_success(flags: u16) {
    assert_eq!(flags & (FLAG_CF as u16), 0);
    assert_ne!(flags & (RFLAGS_IF as u16), 0);
}

#[test]
fn a20_state_is_shared_between_devices_memory_and_bios() {
    let mut platform = Platform::new(2 * 1024 * 1024);
    let a20 = platform.chipset.a20();

    // 1) Register the fast A20 gate latch (port 0x92).
    platform
        .io
        .register(A20_GATE_PORT, Box::new(Port92A20Gate::new(a20.clone())));

    // 2) Register the i8042 controller, wiring the output port callbacks to the same A20 handle.
    let i8042 = I8042Ports::new();
    let controller = i8042.controller();
    controller
        .borrow_mut()
        .set_system_control_sink(Box::new(PlatformSystemControlSink::new(a20.clone())));
    platform
        .io
        .register(I8042_DATA_PORT, Box::new(i8042.port60()));
    platform
        .io
        .register(I8042_STATUS_PORT, Box::new(i8042.port64()));

    assert!(!a20.enabled());

    // 3) Wire BIOS INT 15h A20 services to the platform A20 line.
    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut cpu = CpuState::new(CpuMode::Real);
    let mut bios_bus = BiosA20Bus::new(a20.clone(), 0x2000);

    // BIOS query should observe the reset default: A20 disabled.
    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2402);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RAX] as u8, 0);

    // Reset default: A20 disabled (0x1_00000 aliases 0x0).
    platform.memory.write_u8(0x0, 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    // i8042 output port reads should reflect disabled.
    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x00);

    // Enable A20 via port 0x92 and verify memory separation.
    platform.io.write_u8(A20_GATE_PORT, 0x02);
    assert!(a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x02);

    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2402);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RAX] as u8, 1);

    platform.memory.write_u8(0x1_00000, 0x22);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    // i8042 output port reads should report the same A20 line state even though we did not
    // write the i8042 output port latch.
    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x02);

    // Disable A20 via port 0x92 and verify aliasing.
    platform.io.write_u8(A20_GATE_PORT, 0x00);
    assert!(!a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    // i8042 output port reads should observe the same line state.
    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x00);

    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2402);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RAX] as u8, 0);

    // Enable A20 via the i8042 output port path and verify separation again.
    platform.io.write_u8(I8042_STATUS_PORT, 0xD1);
    platform.io.write_u8(I8042_DATA_PORT, 0x03);
    assert!(a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x02);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x02);

    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2402);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RAX] as u8, 1);

    // Disable A20 via the i8042 output port and verify aliasing.
    platform.io.write_u8(I8042_STATUS_PORT, 0xD1);
    // Keep reset deasserted (bit 0) but clear A20 (bit 1).
    platform.io.write_u8(I8042_DATA_PORT, 0x01);
    assert!(!a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x00);

    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2402);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RAX] as u8, 0);

    // Enable A20 via BIOS INT 15h and verify separation.
    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2401);
    assert_int15_success(flags);
    assert!(a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x02);
    assert_eq!(platform.memory.read_u8(0x0), 0x11);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x22);

    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x02);

    // Disable A20 via BIOS INT 15h and verify aliasing.
    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2400);
    assert_int15_success(flags);
    assert!(!a20.enabled());
    assert_eq!(platform.io.read_u8(A20_GATE_PORT) & 0x02, 0x00);
    assert_eq!(platform.memory.read_u8(0x1_00000), 0x11);

    platform.io.write_u8(I8042_STATUS_PORT, 0xD0);
    assert_eq!(platform.io.read_u8(I8042_DATA_PORT) & 0x02, 0x00);

    // BIOS should advertise that it supports the keyboard controller, port 0x92, and INT 15h.
    cpu.gpr[gpr::RBX] = 0;
    let flags = bios_int15(&mut bios, &mut bios_bus, &mut cpu, 0x2403);
    assert_int15_success(flags);
    assert_eq!(cpu.gpr[gpr::RBX] as u16, 0x0007);
}
