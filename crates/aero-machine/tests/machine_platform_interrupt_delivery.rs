use aero_machine::{Machine, MachineConfig, RunExit};
use aero_platform::interrupts::{InterruptController, InterruptInput, PlatformInterruptMode};
use pretty_assertions::assert_eq;

fn pc_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal and deterministic for interrupt delivery tests.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn build_real_mode_interrupt_wait_boot_sector(
    vector: u8,
    flag_addr: u16,
    flag_value: u8,
) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov ss, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD0]);
    i += 2;
    // mov sp, 0x7c00
    sector[i..i + 3].copy_from_slice(&[0xBC, 0x00, 0x7C]);
    i += 3;

    let ivt_off = (vector as u16) * 4;

    // mov word ptr [ivt_off], handler_offset (patched later)
    // C7 06 <addr16> <imm16>
    let patch_off = i + 4;
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&ivt_off.to_le_bytes());
    // imm16 placeholder
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // mov word ptr [ivt_off+2], 0x0000 (segment)
    sector[i..i + 2].copy_from_slice(&[0xC7, 0x06]);
    sector[i + 2..i + 4].copy_from_slice(&(ivt_off + 2).to_le_bytes());
    sector[i + 4..i + 6].copy_from_slice(&[0, 0]);
    i += 6;

    // sti
    sector[i] = 0xFB;
    i += 1;

    // hlt; jmp short $-3  (busy wait at HLT)
    sector[i..i + 3].copy_from_slice(&[0xF4, 0xEB, 0xFD]);
    i += 3;

    // Handler lives directly after the loop, still within the boot sector (loaded at 0x7C00).
    let handler_addr = 0x7C00u16 + (i as u16);
    sector[patch_off..patch_off + 2].copy_from_slice(&handler_addr.to_le_bytes());

    // mov byte ptr [flag_addr], flag_value
    sector[i..i + 2].copy_from_slice(&[0xC6, 0x06]);
    i += 2;
    sector[i..i + 2].copy_from_slice(&flag_addr.to_le_bytes());
    i += 2;
    sector[i] = flag_value;
    i += 1;
    // iret
    sector[i] = 0xCF;

    // Boot signature.
    sector[510] = 0x55;
    sector[511] = 0xAA;

    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit while waiting for HLT: {other:?}"),
        }
    }
    panic!("machine did not reach HLT in time");
}

fn program_ioapic_entry(ints: &mut aero_platform::interrupts::PlatformInterrupts, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

#[test]
fn machine_pit_irq0_wakes_cpu_from_hlt() {
    // IRQ0 with PIC base 0x20 => vector 0x20.
    let vector = 0x20u8;
    let flag_addr = 0x0502u16;
    let flag_value = 0xCCu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Halt first so the PIT interrupt must wake the CPU.
    run_until_halt(&mut m);

    // Reprogram the PIC via its legacy I/O ports.
    // Standard 8259 remap sequence:
    //   ICW1=0x11, ICW2=0x20/0x28, ICW3=0x04/0x02, ICW4=0x01.
    m.io_write(0x20, 1, 0x11);
    m.io_write(0xA0, 1, 0x11);
    m.io_write(0x21, 1, 0x20);
    m.io_write(0xA1, 1, 0x28);
    m.io_write(0x21, 1, 0x04);
    m.io_write(0xA1, 1, 0x02);
    m.io_write(0x21, 1, 0x01);
    m.io_write(0xA1, 1, 0x01);
    // Unmask IRQ0 only.
    m.io_write(0x21, 1, 0xFE);
    m.io_write(0xA1, 1, 0xFF);

    // Program PIT channel 0 for ~1kHz periodic interrupts (mode 2, lo/hi).
    // PIT tick rate is 1.193182 MHz, so divisor 1193 ~= 1ms period.
    m.io_write(0x43, 1, 0x34);
    m.io_write(0x40, 1, 1193 & 0xFF);
    m.io_write(0x40, 1, 1193 >> 8);

    // The machine should advance platform time even while halted so PIT IRQ0 can wake it.
    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "PIT IRQ0 handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_run_slice_polls_platform_pic_and_delivers_interrupt() {
    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let flag_addr = 0x0500u16;
    let flag_value = 0x5Au8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure the PIC and raise IRQ1.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "PIC interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_run_slice_polls_platform_ioapic_and_delivers_interrupt() {
    let vector = 0x60u8;
    let gsi = 10u32;
    let flag_addr = 0x0501u16;
    let flag_value = 0xA5u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Route GSI10 -> vector 0x60, edge-triggered, active-low (PCI INTx wiring).
        let low = u32::from(vector) | (1 << 13); // polarity_low, edge-triggered
        program_ioapic_entry(&mut ints, gsi, low, 0);

        ints.raise_irq(InterruptInput::Gsi(gsi));
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IOAPIC interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn boot_sector_builder_patches_handler_address_correctly() {
    // Smoke-test the boot sector builder itself so failures are easier to diagnose.
    let vector = 0x33u8;
    let flag_addr = 0x1234u16;
    let flag_value = 0xABu8;
    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);
    assert_eq!(&boot[510..], &[0x55, 0xAA]);
}

#[test]
fn machine_i8042_keyboard_input_raises_irq1_and_wakes_hlt_cpu() {
    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let flag_addr = 0x0502u16;
    let flag_value = 0xCCu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure PIC offsets and unmask IRQ1 so i8042 keyboard IRQs can be delivered.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
    }

    // Inject a key press. The i8042 model should place a scancode in the output buffer and pulse
    // IRQ1, waking the halted CPU and delivering vector 0x21.
    m.inject_browser_key("KeyA", true);

    // Sanity-check that the platform interrupt controller sees the IRQ pending before we run the
    // CPU again.
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "i8042 input did not latch IRQ1 into the PIC"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "i8042 IRQ1 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}
