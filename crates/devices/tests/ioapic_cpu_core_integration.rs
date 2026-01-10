use aero_cpu_core::bus::{Bus, RamBus};
use aero_cpu_core::interrupts::{CpuExit, InterruptController as CpuInterruptController};
use aero_cpu_core::system::{Cpu, CpuMode, DescriptorTableRegister};
use aero_platform::interrupts::{InterruptInput, InterruptController, PlatformInterruptMode, PlatformInterrupts};

fn write_idt_gate32(
    mem: &mut RamBus,
    idt_base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let entry_addr = idt_base + (vector as u64) * 8;
    mem.write_u16(entry_addr, (offset & 0xffff) as u16);
    mem.write_u16(entry_addr + 2, selector);
    mem.write_u8(entry_addr + 4, 0);
    mem.write_u8(entry_addr + 5, type_attr);
    mem.write_u16(entry_addr + 6, (offset >> 16) as u16);
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
fn ioapic_interrupt_delivers_to_cpu_core_idt() -> Result<(), CpuExit> {
    let gsi = 1u32;
    let vector = 0x46u8;

    let mut ints = PlatformInterrupts::new();
    ints.set_mode(PlatformInterruptMode::Apic);

    // Program IOAPIC redirection table entry for `gsi` to the chosen vector,
    // using the IOREGSEL/IOWIN programming model.
    let redir_low_index = 0x10u8 + (2 * gsi as u8);
    let redir_high_index = redir_low_index + 1;

    // Low dword: vector + level-triggered (bit15), unmasked (bit16=0).
    ints.ioapic_mmio_write(0x00, redir_low_index as u32);
    ints.ioapic_mmio_write(0x10, (vector as u32) | (1 << 15));

    // High dword: destination APIC ID 0.
    ints.ioapic_mmio_write(0x00, redir_high_index as u32);
    ints.ioapic_mmio_write(0x10, 0);

    ints.raise_irq(InterruptInput::Gsi(gsi));

    let mut mem = RamBus::new(0x20000);
    let idt_base = 0x1000;
    let handler = 0x6000;
    write_idt_gate32(&mut mem, idt_base, vector, 0x08, handler, 0x8e);

    let mut cpu = Cpu::default();
    cpu.mode = CpuMode::Protected32;
    cpu.idtr = DescriptorTableRegister {
        base: idt_base,
        limit: 0x7ff,
    };
    cpu.cs = 0x08;
    cpu.ss = 0x10;
    cpu.rsp = 0x9000;
    cpu.rip = 0x1111;
    cpu.rflags = 0x202;

    let mut ctrl = PlatformCtrl {
        ints: &mut ints,
        last_vector: None,
    };

    cpu.poll_and_deliver_external_interrupt(&mut mem, &mut ctrl)?;
    assert_eq!(cpu.rip, handler as u64);

    let delivered = ctrl.last_vector.expect("expected IOAPIC vector to be acknowledged");
    assert_eq!(delivered, vector);

    // Typical level-triggered behaviour: device deasserts the line before EOI.
    ctrl.ints.lower_irq(InterruptInput::Gsi(gsi));
    ctrl.ints.eoi(vector);
    assert_eq!(ctrl.ints.get_pending(), None);

    Ok(())
}

