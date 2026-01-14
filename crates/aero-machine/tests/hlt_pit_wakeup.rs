use aero_devices::pic8259::{MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_machine::{Machine, MachineConfig, RunExit};

fn build_hlt_pit_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut code: Vec<u8> = Vec::new();

    // cli
    code.push(0xFA);
    // xor ax, ax
    code.extend_from_slice(&[0x31, 0xC0]);
    // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xD8]);
    // mov es, ax
    code.extend_from_slice(&[0x8E, 0xC0]);

    // -----------------------------------------------------------------------------------------
    // Remap PIC offsets to 0x20/0x28 (legacy BIOS convention).
    // -----------------------------------------------------------------------------------------
    // ICW1: start init + ICW4
    code.extend_from_slice(&[0xB0, 0x11]); // mov al,0x11
    code.extend_from_slice(&[0xE6, MASTER_CMD as u8]); // out 0x20,al
    code.extend_from_slice(&[0xE6, SLAVE_CMD as u8]); // out 0xA0,al

    // ICW2: vector offsets
    code.extend_from_slice(&[0xB0, 0x20]); // mov al,0x20
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0x28]); // mov al,0x28
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // ICW3: master has a slave on IRQ2; slave identity is 2.
    code.extend_from_slice(&[0xB0, 0x04]); // mov al,0x04
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0x02]); // mov al,0x02
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // ICW4: 8086 mode
    code.extend_from_slice(&[0xB0, 0x01]); // mov al,0x01
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // Unmask only IRQ0 (timer) on the master PIC; mask all on the slave.
    code.extend_from_slice(&[0xB0, 0xFE]); // mov al,0xFE
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0xFF]); // mov al,0xFF
    code.extend_from_slice(&[0xE6, SLAVE_DATA as u8]); // out 0xA1,al

    // -----------------------------------------------------------------------------------------
    // Install a real-mode IVT handler for vector 0x20 at IVT[0x20] = 0x0000:handler.
    // -----------------------------------------------------------------------------------------
    // mov word [0x0080], imm16   ; IVT offset
    code.extend_from_slice(&[0xC7, 0x06, 0x80, 0x00, 0x00, 0x00]); // imm16 patched later
    let handler_imm_off = code.len() - 2;
    // mov word [0x0082], 0x0000 ; IVT segment
    code.extend_from_slice(&[0xC7, 0x06, 0x82, 0x00, 0x00, 0x00]);

    // Clear the flag at physical 0x0500.
    // mov byte [0x0500], 0
    code.extend_from_slice(&[0xC6, 0x06, 0x00, 0x05, 0x00]);

    // -----------------------------------------------------------------------------------------
    // Program PIT channel 0 for periodic interrupts (mode 2).
    // -----------------------------------------------------------------------------------------
    // mov al, 0x34  ; ch0, lobyte/hibyte, mode2, binary
    code.extend_from_slice(&[0xB0, 0x34]);
    // out 0x43, al
    code.extend_from_slice(&[0xE6, PIT_CMD as u8]);

    // Reload divisor (small period so tests complete quickly).
    // mov ax, 0x0020
    code.extend_from_slice(&[0xB8, 0x20, 0x00]);
    // out 0x40, al (low byte)
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);
    // mov al, ah
    code.extend_from_slice(&[0x8A, 0xC4]);
    // out 0x40, al (high byte)
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);

    // sti; hlt
    code.push(0xFB);
    code.push(0xF4);

    // After waking, print "OK" to COM1 and halt.
    // mov dx, 0x3f8
    code.extend_from_slice(&[0xBA, 0xF8, 0x03]);
    // mov al, 'O'; out dx, al
    code.extend_from_slice(&[0xB0, b'O', 0xEE]);
    // mov al, 'K'; out dx, al
    code.extend_from_slice(&[0xB0, b'K', 0xEE]);
    // hlt
    code.push(0xF4);

    // -----------------------------------------------------------------------------------------
    // IRQ0 handler (vector 0x20).
    // Sets a RAM flag at 0x0500, masks further timer interrupts, sends PIC EOI, and IRET.
    // -----------------------------------------------------------------------------------------
    let handler_offset = code.len();

    // push ax
    code.push(0x50);
    // mov byte [0x0500], 1
    code.extend_from_slice(&[0xC6, 0x06, 0x00, 0x05, 0x01]);
    // mov al, 0xFF ; mask all on master PIC (stop further timer interrupts)
    code.extend_from_slice(&[0xB0, 0xFF]);
    // out 0x21, al
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]);
    // mov al, 0x20 ; EOI
    code.extend_from_slice(&[0xB0, 0x20]);
    // out 0x20, al
    code.extend_from_slice(&[0xE6, MASTER_CMD as u8]);
    // pop ax
    code.push(0x58);
    // iret
    code.push(0xCF);

    // Patch IVT entry offset to point at the handler.
    let handler_addr = 0x7C00u16.wrapping_add(handler_offset as u16);
    let [lo, hi] = handler_addr.to_le_bytes();
    code[handler_imm_off] = lo;
    code[handler_imm_off + 1] = hi;

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
fn pit_irq0_wakes_hlt_and_writes_ok_to_serial() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();

    let boot = build_hlt_pit_boot_sector();
    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // If `Machine::run_slice` does not tick platform timers while halted, the VM will never wake
    // from the first HLT and this test will spin until the loop bound is reached.
    let mut saw_ok = false;
    for _ in 0..1_000 {
        let exit = m.run_slice(10_000);
        match exit {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }

        let out = m.serial_output_bytes();
        if out == b"OK" && matches!(exit, RunExit::Halted { .. }) {
            saw_ok = true;
            break;
        }
    }

    assert!(
        saw_ok,
        "guest never printed OK (timers likely not advancing during HLT)"
    );
    assert_eq!(m.take_serial_output(), b"OK");
}
