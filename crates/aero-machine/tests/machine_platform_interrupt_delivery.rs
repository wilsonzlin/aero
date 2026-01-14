use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::i8042::{I8042_DATA_PORT, I8042_STATUS_PORT};
use aero_devices::pic8259::{MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
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

fn pc_machine_mmio_config() -> MachineConfig {
    MachineConfig {
        // Low RAM is fine; the MMIO ranges are well above 1MiB.
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        // HPET/MMIO bases rely on bit20; tests that touch MMIO should enable the A20 gate.
        enable_a20_gate: true,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn build_real_mode_interrupt_wait_boot_sector(
    vector: u8,
    flag_addr: u16,
    flag_value: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
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
    m.io_write(MASTER_CMD, 1, 0x11);
    m.io_write(SLAVE_CMD, 1, 0x11);
    m.io_write(MASTER_DATA, 1, 0x20);
    m.io_write(SLAVE_DATA, 1, 0x28);
    m.io_write(MASTER_DATA, 1, 0x04);
    m.io_write(SLAVE_DATA, 1, 0x02);
    m.io_write(MASTER_DATA, 1, 0x01);
    m.io_write(SLAVE_DATA, 1, 0x01);
    // Unmask IRQ0 only.
    m.io_write(MASTER_DATA, 1, 0xFE);
    m.io_write(SLAVE_DATA, 1, 0xFF);

    // Program PIT channel 0 for ~1kHz periodic interrupts (mode 2, lo/hi).
    // PIT tick rate is 1.193182 MHz, so divisor 1193 ~= 1ms period.
    m.io_write(PIT_CMD, 1, 0x34);
    m.io_write(PIT_CH0, 1, 1193 & 0xFF);
    m.io_write(PIT_CH0, 1, 1193 >> 8);

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
fn machine_pit_irq0_wakes_cpu_from_hlt_in_apic_mode() {
    // In our ACPI tables, ISA IRQ0 is overridden to IOAPIC GSI2. Route it to a known vector.
    const PIT_GSI: u32 = 2;
    let vector = 0x40u8;
    let flag_addr = 0x0503u16;
    let flag_value = 0xDDu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Halt first so the PIT interrupt must wake the CPU.
    run_until_halt(&mut m);

    // Switch interrupt routing to APIC mode and route PIT GSI2 to `vector`.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(&mut ints, PIT_GSI, u32::from(vector), 0);
    }

    // Program PIT channel 0 for ~1kHz periodic interrupts (mode 2, lo/hi).
    m.io_write(PIT_CMD, 1, 0x34);
    m.io_write(PIT_CH0, 1, 1193 & 0xFF);
    m.io_write(PIT_CH0, 1, 1193 >> 8);

    // The machine should advance platform time even while halted so PIT IRQ0 can wake it.
    for _ in 0..200 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "PIT IRQ0 (IOAPIC) handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_hpet_timer0_wakes_cpu_from_hlt_in_apic_mode() {
    // HPET timer0 defaults to GSI2 (matching the ACPI MADT legacy timer interrupt source override).
    const HPET_GSI: u32 = 2;

    // Use a deterministic IOAPIC vector and a small real-mode handler that flips a flag.
    let vector = 0x61u8;
    let flag_addr = 0x0506u16;
    let flag_value = 0xF0u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_mmio_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Ensure the guest reaches `sti; hlt` so the HPET interrupt must wake it.
    run_until_halt(&mut m);

    // Enable A20 before touching MMIO (HPET base 0xFED0_0000 aliases to IOAPIC at 0xFEC0_0000 when
    // A20 is disabled).
    enable_a20(&mut m);

    // Switch interrupt routing to APIC mode and route HPET GSI2 to `vector`.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // HPET timer interrupts are typically level-triggered, active-high.
        let low = u32::from(vector) | (1 << 15);
        program_ioapic_entry(&mut ints, HPET_GSI, low, 0);
    }

    // Program HPET timer0 via guest-visible MMIO:
    // - route: 2 (GSI2)
    // - level-triggered
    // - interrupt enabled
    let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
    m.write_physical_u64(HPET_MMIO_BASE + 0x100, timer0_cfg);
    // Comparator: 10_000 ticks at 10MHz is 1ms (counter_clk_period_fs=100_000_000).
    m.write_physical_u64(HPET_MMIO_BASE + 0x108, 10_000);
    // Enable HPET (general config).
    m.write_physical_u64(HPET_MMIO_BASE + 0x010, 1);

    for _ in 0..50 {
        let _ = m.run_slice(10_000);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "HPET timer0 interrupt did not wake HLT CPU (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_hpet_timer0_wakes_cpu_from_hlt_after_dirty_snapshot_restore() {
    // HPET timer0 defaults to GSI2 (matching the ACPI MADT legacy timer interrupt source override).
    const HPET_GSI: u32 = 2;

    // Use a deterministic IOAPIC vector and a small real-mode handler that flips a flag.
    let vector = 0x62u8;
    let flag_addr = 0x0507u16;
    let flag_value = 0xA5u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut m = Machine::new(pc_machine_mmio_config()).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // Ensure the guest reaches `sti; hlt` first so the HPET interrupt must wake it.
    run_until_halt(&mut m);

    // Capture a base snapshot before programming HPET so we exercise dirty-snapshot restore.
    let base = m.take_snapshot_full().unwrap();

    // Enable A20 before touching MMIO (HPET base 0xFED0_0000 aliases to IOAPIC at 0xFEC0_0000 when
    // A20 is disabled).
    enable_a20(&mut m);

    // Switch interrupt routing to APIC mode and route HPET GSI2 to `vector`.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // HPET timer interrupts are typically level-triggered, active-high.
        let low = u32::from(vector) | (1 << 15);
        program_ioapic_entry(&mut ints, HPET_GSI, low, 0);
    }

    // Program HPET timer0 via guest-visible MMIO:
    // - route: 2 (GSI2)
    // - level-triggered
    // - interrupt enabled
    let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
    m.write_physical_u64(HPET_MMIO_BASE + 0x100, timer0_cfg);
    // Comparator: 10_000 ticks at 10MHz is 1ms (counter_clk_period_fs=100_000_000).
    m.write_physical_u64(HPET_MMIO_BASE + 0x108, 10_000);
    // Enable HPET (general config).
    m.write_physical_u64(HPET_MMIO_BASE + 0x010, 1);

    let diff = m.take_snapshot_dirty().unwrap();

    let mut restored = Machine::new(pc_machine_mmio_config()).unwrap();
    restored.set_disk_image(boot.to_vec()).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&base).unwrap();
    restored.restore_snapshot_bytes(&diff).unwrap();

    for _ in 0..50 {
        let _ = restored.run_slice(10_000);
        if restored.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "HPET timer0 interrupt did not wake HLT CPU after snapshot restore (flag=0x{:02x})",
        restored.read_physical_u8(u64::from(flag_addr))
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

#[test]
fn machine_i8042_mouse_motion_raises_irq12_and_wakes_hlt_cpu() {
    // IRQ12 with PIC base 0x28 => vector 0x2C.
    let vector = 0x2Cu8;
    let flag_addr = 0x0503u16;
    let flag_value = 0xDDu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Enable PS/2 mouse data reporting *without* enabling IRQ12 yet. This avoids waking the CPU
    // due to the mouse ACK byte (0xFA) produced by the enable command itself.
    m.io_write(I8042_STATUS_PORT, 1, 0xD4); // i8042: next data write goes to mouse
    m.io_write(I8042_DATA_PORT, 1, 0xF4); // mouse: enable data reporting (ACK 0xFA)
    let ack = m.io_read(I8042_DATA_PORT, 1) as u8;
    assert_eq!(
        ack, 0xFA,
        "expected mouse ACK after enabling data reporting"
    );

    // Enable i8042 IRQ12 generation (command byte bit 1) while keeping the default settings
    // (IRQ1 enabled + translation enabled).
    m.io_write(I8042_STATUS_PORT, 1, 0x60); // i8042: write command byte
    m.io_write(I8042_DATA_PORT, 1, 0x47); // default 0x45 | IRQ12 enable (bit 1)

    // Configure PIC offsets and unmask cascade (IRQ2) + IRQ12 so the mouse IRQ can be delivered.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(12, false);
    }

    // Inject relative mouse motion. The i8042 model should enqueue a PS/2 packet and pulse IRQ12,
    // waking the halted CPU and delivering vector 0x2C.
    m.inject_mouse_motion(1, 1, 0);

    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "i8042 mouse motion did not latch IRQ12 into the PIC"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "i8042 IRQ12 interrupt handler did not run (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_i8042_keyboard_input_delivers_via_ioapic_in_apic_mode() {
    let vector = 0x61u8;
    let flag_addr = 0x0504u16;
    let flag_value = 0xEEu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Route ISA IRQ1 through the IOAPIC/LAPIC path.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);

        // ISA IRQ1 maps to GSI1 by default.
        program_ioapic_entry(&mut ints, 1, u32::from(vector), 0);
    }

    m.inject_browser_key("KeyA", true);

    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "i8042 keyboard input did not latch IRQ1 into the LAPIC in APIC mode"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "i8042 IRQ1 handler did not run in APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_i8042_mouse_motion_delivers_via_ioapic_in_apic_mode() {
    let vector = 0x62u8;
    let flag_addr = 0x0505u16;
    let flag_value = 0xEFu8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Switch to APIC mode and program GSI12 (ISA IRQ12) to a deterministic vector.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_entry(&mut ints, 12, u32::from(vector), 0);
    }

    // Enable PS/2 mouse reporting while IRQ12 is still disabled (avoid waking on the ACK).
    m.io_write(I8042_STATUS_PORT, 1, 0xD4);
    m.io_write(I8042_DATA_PORT, 1, 0xF4);
    let ack = m.io_read(I8042_DATA_PORT, 1) as u8;
    assert_eq!(
        ack, 0xFA,
        "expected mouse ACK after enabling data reporting"
    );

    // Enable i8042 IRQ12 generation.
    m.io_write(I8042_STATUS_PORT, 1, 0x60);
    m.io_write(I8042_DATA_PORT, 1, 0x47);

    m.inject_mouse_motion(1, 1, 0);

    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "i8042 mouse motion did not latch IRQ12 into the LAPIC in APIC mode"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "i8042 IRQ12 handler did not run in APIC mode (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_i8042_keyboard_irq1_is_gated_by_i8042_command_byte() {
    let vector = 0x21u8; // IRQ1 with PIC base 0x20.
    let flag_addr = 0x0506u16;
    let flag_value = 0x11u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure PIC offsets and unmask IRQ1 so delivery would be possible if i8042 asserted it.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
    }

    // Disable i8042 IRQ1 generation (command byte bit 0).
    m.io_write(I8042_STATUS_PORT, 1, 0x60);
    m.io_write(I8042_DATA_PORT, 1, 0x44); // 0x45 default with IRQ1 cleared.

    m.inject_browser_key("KeyA", true);

    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ1 should not be latched when i8042 command byte has IRQ1 disabled"
        );
    }

    // The scancode should still be present in the output buffer even if no interrupt is generated.
    assert_eq!(m.io_read(I8042_DATA_PORT, 1) as u8, 0x1E); // Set-1 'A' make code.

    for _ in 0..10 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "interrupt handler ran unexpectedly while IRQ1 was disabled in i8042 command byte"
        );
    }
}

#[test]
fn machine_i8042_mouse_irq12_is_gated_by_i8042_command_byte() {
    // IRQ12 with PIC base 0x28 => vector 0x2C.
    let vector = 0x2Cu8;
    let flag_addr = 0x0507u16;
    let flag_value = 0x22u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure PIC offsets and unmask cascade + IRQ12.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(12, false);
    }

    // Enable PS/2 mouse reporting; this produces an ACK byte (0xFA) but should not generate IRQ12
    // because the i8042 command byte has IRQ12 disabled by default (bit 1 = 0).
    m.io_write(I8042_STATUS_PORT, 1, 0xD4);
    m.io_write(I8042_DATA_PORT, 1, 0xF4);
    let ack = m.io_read(I8042_DATA_PORT, 1) as u8;
    assert_eq!(ack, 0xFA);

    m.inject_mouse_motion(1, 1, 0);

    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ12 should not be latched when i8042 command byte has IRQ12 disabled"
        );
    }

    // A mouse packet should still be available in the output buffer.
    let _ = m.io_read(I8042_DATA_PORT, 1);

    for _ in 0..10 {
        let _ = m.run_slice(256);
        assert_ne!(
            m.read_physical_u8(u64::from(flag_addr)),
            flag_value,
            "interrupt handler ran unexpectedly while IRQ12 was disabled in i8042 command byte"
        );
    }
}

#[test]
fn machine_i8042_keyboard_port_disable_drops_keys_until_reenabled() {
    let vector = 0x21u8; // IRQ1 with PIC base 0x20
    let flag_addr = 0x0508u16;
    let flag_value = 0x33u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure PIC offsets and unmask IRQ1 so we can observe any keyboard IRQ pulses.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
    }

    // Disable the keyboard port via i8042 command 0xAD.
    m.io_write(I8042_STATUS_PORT, 1, 0xAD);

    m.inject_browser_key("KeyA", true);

    assert_eq!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should remain empty while keyboard port is disabled"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ1 should not be latched while keyboard port is disabled"
        );
    }

    // Re-enable the keyboard port via i8042 command 0xAE.
    //
    // Host-side key injection is intentionally dropped while the port is disabled (the controller
    // is holding the clock line low). Re-enabling the port should *not* suddenly flush stale key
    // events into the output buffer.
    m.io_write(I8042_STATUS_PORT, 1, 0xAE);

    assert_eq!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should remain empty after re-enabling keyboard port (disabled-port key injection is dropped)"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ1 should not be latched after re-enabling keyboard port (disabled-port key injection is dropped)"
        );
    }

    // Inject a key with the port enabled; now we should observe output + IRQ1 delivery.
    m.inject_browser_key("KeyA", true);
    assert_ne!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should become full once keyboard port is enabled and a key is injected"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "IRQ1 should be latched once keyboard port is enabled and a key is injected"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IRQ1 handler did not run after re-enabling keyboard port (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_i8042_mouse_port_disable_drops_motion_until_reenabled() {
    // IRQ12 with PIC base 0x28 => vector 0x2C.
    let vector = 0x2Cu8;
    let flag_addr = 0x0509u16;
    let flag_value = 0x44u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure PIC offsets and unmask cascade + IRQ12.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(12, false);
    }

    // Enable PS/2 mouse reporting while IRQ12 is still disabled, drain ACK.
    m.io_write(I8042_STATUS_PORT, 1, 0xD4);
    m.io_write(I8042_DATA_PORT, 1, 0xF4);
    let ack = m.io_read(I8042_DATA_PORT, 1) as u8;
    assert_eq!(ack, 0xFA);

    // Enable i8042 IRQ12 generation.
    m.io_write(I8042_STATUS_PORT, 1, 0x60);
    m.io_write(I8042_DATA_PORT, 1, 0x47);

    // Disable mouse port via i8042 command 0xA7.
    m.io_write(I8042_STATUS_PORT, 1, 0xA7);

    m.inject_mouse_motion(1, 1, 0);

    assert_eq!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should remain empty while mouse port is disabled"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ12 should not be latched while mouse port is disabled"
        );
    }

    // Re-enable mouse port via i8042 command 0xA8. Host-side injected mouse motion should be
    // dropped while the port is disabled (to avoid buffering large cursor jumps), so re-enabling
    // the port should *not* suddenly produce an output byte or IRQ12.
    m.io_write(I8042_STATUS_PORT, 1, 0xA8);

    assert_eq!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should remain empty after re-enabling mouse port (disabled-port motion is dropped)"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            None,
            "IRQ12 should not be latched after re-enabling mouse port (disabled-port motion is dropped)"
        );
    }

    // Inject motion again with the port enabled; now we should observe output + IRQ12 delivery.
    m.inject_mouse_motion(1, 1, 0);
    assert_ne!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should become full once mouse port is enabled and motion is injected"
    );
    {
        let interrupts = m.platform_interrupts().unwrap();
        assert_eq!(
            interrupts.borrow().get_pending(),
            Some(vector),
            "IRQ12 should be latched once mouse port is enabled and motion is injected"
        );
    }

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IRQ12 handler did not run after re-enabling mouse port (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}

#[test]
fn machine_i8042_translation_disable_emits_set2_scancodes() {
    let vector = 0x21u8; // IRQ1 with PIC base 0x20.
    let flag_addr = 0x050Au16;
    let flag_value = 0x55u8;

    let boot = build_real_mode_interrupt_wait_boot_sector(vector, flag_addr, flag_value);

    let mut cfg = pc_machine_config();
    cfg.enable_i8042 = true;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    // Configure the PIC and unmask IRQ1 so the i8042 keyboard IRQ can be delivered.
    {
        let interrupts = m.platform_interrupts().unwrap();
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
    }

    // Disable Set-2 -> Set-1 translation (command byte bit 6).
    //
    // Default command byte is 0x45; clearing bit 6 yields 0x05 (IRQ1 still enabled).
    m.io_write(I8042_STATUS_PORT, 1, 0x60);
    m.io_write(I8042_DATA_PORT, 1, 0x05);

    m.inject_browser_key("KeyA", true);

    // With translation disabled, the Set-2 make code for 'A' is 0x1C.
    assert_ne!(
        m.io_read(I8042_STATUS_PORT, 1) as u8 & 0x01,
        0,
        "output buffer should contain a scancode byte after key injection"
    );
    assert_eq!(
        m.io_read(I8042_DATA_PORT, 1) as u8,
        0x1C,
        "expected Set-2 scancode when i8042 translation is disabled"
    );

    for _ in 0..10 {
        let _ = m.run_slice(256);
        if m.read_physical_u8(u64::from(flag_addr)) == flag_value {
            return;
        }
    }

    panic!(
        "IRQ1 handler did not run with translation disabled (flag=0x{:02x})",
        m.read_physical_u8(u64::from(flag_addr))
    );
}
