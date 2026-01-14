use aero_devices::pic8259::{MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_machine::{Machine, MachineConfig, RunExit};

fn build_pit_wake_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut code: Vec<u8> = Vec::new();

    // Disable interrupts and set up a known real-mode environment.
    // cli
    code.push(0xFA);
    // xor ax, ax
    code.extend_from_slice(&[0x31, 0xC0]);
    // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD8]);
    // mov es, ax
    code.extend_from_slice(&[0x8E, 0xC0]);
    // mov ss, ax
    code.extend_from_slice(&[0x8E, 0xD0]);
    // mov sp, 0x7000
    code.extend_from_slice(&[0xBC, 0x00, 0x70]);

    // -----------------------------------------------------------------------------------------
    // Initialize and unmask the legacy PIC so the PIT can deliver IRQ0.
    // -----------------------------------------------------------------------------------------
    // ICW1: start init + ICW4
    code.extend_from_slice(&[0xB0, 0x11]); // mov al,0x11
    code.extend_from_slice(&[0xE6, MASTER_CMD as u8]); // out 0x20,al
    code.extend_from_slice(&[0xE6, SLAVE_CMD as u8]); // out 0xA0,al

    // ICW2: vector offsets (0x20/0x28)
    code.extend_from_slice(&[0xB0, 0x20]); // mov al,0x20
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0x28]); // mov al,0x28
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // ICW3: master has a slave on IRQ2; slave identity is 2.
    code.extend_from_slice(&[0xB0, 0x04]); // mov al,0x04
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0x02]); // mov al,0x02
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // ICW4: 8086 mode.
    code.extend_from_slice(&[0xB0, 0x01]); // mov al,0x01
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // Unmask only IRQ0 (timer) on the master PIC; mask all on the slave.
    code.extend_from_slice(&[0xB0, 0xFE]); // mov al,0xFE
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0xFF]); // mov al,0xFF
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // PIT channel 0, lobyte/hibyte, mode 2 (rate generator), binary.
    // mov al, 0x34
    code.extend_from_slice(&[0xB0, 0x34]);
    // out 0x43, al
    code.extend_from_slice(&[0xE6, PIT_CMD as u8]);

    // Divisor ~1193 -> ~1kHz.
    // mov al, 0xA9 (low byte)
    code.extend_from_slice(&[0xB0, 0xA9]);
    // out 0x40, al
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);
    // mov al, 0x04 (high byte)
    code.extend_from_slice(&[0xB0, 0x04]);
    // out 0x40, al
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);

    // Enable interrupts then halt until PIT IRQ0 arrives.
    // sti; hlt
    code.push(0xFB);
    code.push(0xF4);

    // After wake, write 'W' to COM1.
    // mov dx, 0x3f8
    code.extend_from_slice(&[0xBA, 0xF8, 0x03]);
    // mov al, 'W'
    code.extend_from_slice(&[0xB0, b'W']);
    // out dx, al
    code.push(0xEE);

    // Stop: cli; hlt
    code.push(0xFA);
    code.push(0xF4);

    assert!(
        code.len() <= 510,
        "boot sector too large: {} bytes",
        code.len()
    );

    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[..code.len()].copy_from_slice(&code);
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn pit_irq0_wakes_cpu_from_hlt() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();

    let boot = build_pit_wake_boot_sector();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    for _ in 0..1_000 {
        match m.run_slice(10_000) {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }

        if m.serial_output_bytes().contains(&b'W') {
            return;
        }
    }

    panic!("guest never woke from HLT and wrote to serial");
}
