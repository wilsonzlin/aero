use aero_cpu_core::interrupts::{CpuCore, CpuExit, InterruptController as CpuInterruptController};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode};
use aero_devices::pci::{msix::PCI_CAP_ID_MSIX, MsixCapability, PciConfigSpace};
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
    mem.write_u16(entry_addr + 6, (offset >> 16) as u16)
        .unwrap();
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
fn msix_interrupt_delivers_to_cpu_core_idt() -> Result<(), CpuExit> {
    let mut config = PciConfigSpace::new(0x1234, 0x5678);
    config.add_capability(Box::new(MsixCapability::new(1, 0, 0x1000, 0, 0x2000)));
    let cap_offset = config.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;

    // Enable MSI-X via the Message Control enable bit (bit 15).
    let ctrl = config.read(cap_offset + 0x02, 2) as u16;
    config.write(cap_offset + 0x02, 2, u32::from(ctrl | (1 << 15)));

    let table_index: u16 = 0;
    let vector: u8 = 0x45;
    // Use broadcast destination to avoid depending on the platform LAPIC APIC ID.
    let msi_address: u64 = 0xFEE0_0000u64 | (0xFFu64 << 12);

    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        let base = u64::from(table_index) * 16;
        msix.table_write(base, &(msi_address as u32).to_le_bytes());
        msix.table_write(base + 0x4, &((msi_address >> 32) as u32).to_le_bytes());
        msix.table_write(base + 0x8, &(u32::from(vector)).to_le_bytes());
        msix.table_write(base + 0xc, &0u32.to_le_bytes()); // unmasked
    }

    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    {
        let msix = config.capability_mut::<MsixCapability>().unwrap();
        assert!(msix.trigger_into(table_index, &mut ints));
    }

    let mut mem = FlatTestBus::new(0x20000);
    let idt_base = 0x1000;
    let handler = 0x6000;
    write_idt_gate32(&mut mem, idt_base, vector, 0x08, handler, 0x8e);

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

    let acknowledged = ctrl
        .last_vector
        .expect("expected MSI-X vector to be acknowledged");
    ctrl.ints.eoi(acknowledged);

    Ok(())
}

