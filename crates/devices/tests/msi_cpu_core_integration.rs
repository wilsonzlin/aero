use aero_cpu_core::interrupts::{CpuCore, CpuExit, InterruptController as CpuInterruptController};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode};
use aero_devices::pci::{MsiCapability, PciConfigSpace};
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};

fn write_idt_gate32(
    mem: &mut impl CpuBus,
    idt_base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let entry_addr = idt_base + (vector as u64) * 8;
    mem.write_u16(entry_addr, (offset & 0xffff) as u16).unwrap();
    mem.write_u16(entry_addr + 2, selector).unwrap();
    mem.write_u8(entry_addr + 4, 0).unwrap();
    mem.write_u8(entry_addr + 5, type_attr).unwrap();
    mem.write_u16(entry_addr + 6, (offset >> 16) as u16).unwrap();
}

struct PlatformCtrl<'a> {
    ints: &'a mut PlatformInterrupts,
    last_vector: Option<u8>,
}

impl CpuInterruptController for PlatformCtrl<'_> {
    fn poll_interrupt(&mut self) -> Option<u8> {
        let vector = self.ints.get_pending()?;
        self.ints.acknowledge(vector);
        self.last_vector = Some(vector);
        Some(vector)
    }
}

#[test]
fn msi_interrupt_delivers_to_cpu_core_idt() -> Result<(), CpuExit> {
    let mut config = PciConfigSpace::new(0x1234, 0x5678);
    config.add_capability(Box::new(MsiCapability::new()));
    let cap_offset = config
        .find_capability(aero_devices::pci::msi::PCI_CAP_ID_MSI)
        .unwrap() as u16;

    config.write(cap_offset + 0x04, 4, 0xfee0_0000);
    config.write(cap_offset + 0x08, 4, 0);
    config.write(cap_offset + 0x0c, 2, 0x0045);
    let ctrl = config.read(cap_offset + 0x02, 2) as u16;
    config.write(cap_offset + 0x02, 2, (ctrl | 0x0001) as u32);

    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    let msi = config.capability_mut::<MsiCapability>().unwrap();
    assert!(msi.trigger(&mut ints));

    let mut mem = FlatTestBus::new(0x20000);
    let idt_base = 0x1000;
    let handler = 0x6000;
    write_idt_gate32(&mut mem, idt_base, 0x45, 0x08, handler, 0x8e);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7ff;
    cpu.state.segments.cs.selector = 0x08;
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x9000);
    cpu.state.set_rip(0x1111);
    cpu.state.set_rflags(0x202);

    let mut ctrl = PlatformCtrl {
        ints: &mut ints,
        last_vector: None,
    };
    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;
    assert_eq!(cpu.state.rip(), handler as u64);

    let vector = ctrl
        .last_vector
        .expect("expected MSI vector to be acknowledged");
    ctrl.ints.eoi(vector);

    Ok(())
}
