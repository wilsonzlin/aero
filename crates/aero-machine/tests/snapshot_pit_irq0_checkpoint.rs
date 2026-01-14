use aero_devices::pic8259::{MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

const COUNTER_ADDR: u64 = 0x2000;
const TARGET_TICKS: u16 = 5;
const SNAPSHOT_TICKS: u16 = 2;

fn build_pit_irq0_counter_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut code: Vec<u8> = Vec::new();

    // -----------------------------------------------------------------------------------------
    // Real-mode init: set segments, stack, and configure PIC+PIT.
    // -----------------------------------------------------------------------------------------
    //
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
    // mov sp, 0x7C00
    code.extend_from_slice(&[0xBC, 0x00, 0x7C]);

    // -----------------------------------------------------------------------------------------
    // Program PIC offsets to:
    // - master: 0x08 (IRQ0 => vector 0x08)
    // - slave:  0x70
    // -----------------------------------------------------------------------------------------
    // ICW1: start init + ICW4
    code.extend_from_slice(&[0xB0, 0x11]); // mov al,0x11
    code.extend_from_slice(&[0xE6, MASTER_CMD as u8]); // out 0x20,al
    code.extend_from_slice(&[0xE6, SLAVE_CMD as u8]); // out 0xA0,al
                                                      // ICW2: vector offsets
    code.extend_from_slice(&[0xB0, 0x08]); // mov al,0x08
    code.extend_from_slice(&[0xE6, MASTER_DATA as u8]); // out 0x21,al
    code.extend_from_slice(&[0xB0, 0x70]); // mov al,0x70
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
    // Install a real-mode IVT handler for vector 0x08 at IVT[0x08] = 0x0000:handler.
    //
    // We write the handler address using an absolute segment (0x0000) so we don't depend on the
    // BIOS-provided CS value (some BIOSes jump to 07C0:0000 instead of 0000:7C00).
    // -----------------------------------------------------------------------------------------
    // mov word [0x0020], imm16   ; IVT offset
    code.extend_from_slice(&[0xC7, 0x06, 0x20, 0x00, 0x00, 0x00]); // imm16 patched later
    let handler_imm_off = code.len() - 2;
    // mov word [0x0022], 0x0000 ; IVT segment
    code.extend_from_slice(&[0xC7, 0x06, 0x22, 0x00, 0x00, 0x00]);

    // Clear the tick counter at COUNTER_ADDR.
    // mov word [0x2000], 0
    code.extend_from_slice(&[0xC7, 0x06, 0x00, 0x20, 0x00, 0x00]);

    // -----------------------------------------------------------------------------------------
    // Program PIT channel 0 for periodic interrupts (mode 2).
    // Use a relatively slow divisor so the CPU will remain halted between ticks, letting the
    // host take a snapshot while waiting for the next wakeup interrupt.
    // -----------------------------------------------------------------------------------------
    // mov al, 0x34  ; ch0, lobyte/hibyte, mode2, binary
    code.extend_from_slice(&[0xB0, 0x34]);
    // out 0x43, al
    code.extend_from_slice(&[0xE6, PIT_CMD as u8]);

    // Reload divisor for ~100Hz: 1_193_182 / 100 ≈ 11_932 (0x2E9C).
    // mov ax, 0x2E9C
    code.extend_from_slice(&[0xB8, 0x9C, 0x2E]);
    // out 0x40, al (low byte)
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);
    // mov al, ah
    code.extend_from_slice(&[0x8A, 0xC4]);
    // out 0x40, al (high byte)
    code.extend_from_slice(&[0xE6, PIT_CH0 as u8]);

    // sti
    code.push(0xFB);

    // -----------------------------------------------------------------------------------------
    // Main loop: sleep until IRQ0 increments the counter to TARGET_TICKS.
    // -----------------------------------------------------------------------------------------
    let loop_start = code.len();
    // hlt
    code.push(0xF4);
    // cmp word [0x2000], TARGET_TICKS
    let [ticks_lo, ticks_hi] = TARGET_TICKS.to_le_bytes();
    code.extend_from_slice(&[0x81, 0x3E, 0x00, 0x20, ticks_lo, ticks_hi]);
    // jb loop_start
    code.extend_from_slice(&[0x72, 0x00]); // rel8 patched later
    let jb_disp_off = code.len() - 1;

    // done:
    // cli
    code.push(0xFA);

    // Write "DONE\n" to COM1.
    // mov dx, 0x3f8
    code.extend_from_slice(&[0xBA, 0xF8, 0x03]);
    for &b in b"DONE\n" {
        // mov al, imm8; out dx, al
        code.extend_from_slice(&[0xB0, b, 0xEE]);
    }

    // hlt
    code.push(0xF4);

    // -----------------------------------------------------------------------------------------
    // IRQ0 handler (vector 0x08): increment counter, PIC EOI, IRET.
    // -----------------------------------------------------------------------------------------
    let handler_offset = code.len();
    // push ax
    code.push(0x50);
    // inc word [0x2000]
    code.extend_from_slice(&[0xFF, 0x06, 0x00, 0x20]);
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
    let [hlo, hhi] = handler_addr.to_le_bytes();
    code[handler_imm_off] = hlo;
    code[handler_imm_off + 1] = hhi;

    // Patch the `jb` rel8 displacement.
    let jb_next = jb_disp_off + 1;
    let rel = (loop_start as isize).wrapping_sub(jb_next as isize);
    let rel8: i8 = rel
        .try_into()
        .expect("loop jump displacement should fit in i8");
    code[jb_disp_off] = rel8 as u8;

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

fn checkpoint_machine_cfg() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal and deterministic for this test.
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn run_until_final_marker(m: &mut Machine) -> (Vec<u8>, u16) {
    for _ in 0..5_000 {
        let exit = m.run_slice(10_000);
        match exit {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }

        let ticks = m.read_physical_u16(COUNTER_ADDR);
        if ticks == TARGET_TICKS && m.serial_output_len() == 5 {
            let out = m.serial_output_bytes();
            return (out, ticks);
        }
    }
    panic!("timeout waiting for guest to reach TARGET_TICKS and write serial marker");
}

#[test]
fn snapshot_restore_preserves_pit_irq0_hlt_checkpoint_continuity() {
    let cfg = checkpoint_machine_cfg();
    let boot = build_pit_irq0_counter_boot_sector();

    // Baseline run to completion.
    let mut baseline = Machine::new(cfg.clone()).unwrap();
    baseline.set_disk_image(boot.to_vec()).unwrap();
    baseline.reset();
    let (baseline_out, baseline_ticks) = run_until_final_marker(&mut baseline);
    assert_eq!(baseline_out, b"DONE\n");
    assert_eq!(baseline_ticks, TARGET_TICKS);

    // Run a second machine, checkpoint mid-way, and resume in a fresh instance.
    let mut vm = Machine::new(cfg.clone()).unwrap();
    vm.set_disk_image(boot.to_vec()).unwrap();
    vm.reset();

    // Run until we've seen SNAPSHOT_TICKS timer interrupts and the CPU is halted waiting for the
    // next one.
    for _ in 0..5_000 {
        let exit = vm.run_slice(10_000);
        match exit {
            RunExit::Completed { .. } | RunExit::Halted { .. } => {}
            other => panic!("unexpected exit: {other:?}"),
        }

        if matches!(exit, RunExit::Halted { .. })
            && vm.read_physical_u16(COUNTER_ADDR) == SNAPSHOT_TICKS
            && vm.serial_output_len() == 0
        {
            break;
        }
    }
    assert_eq!(vm.read_physical_u16(COUNTER_ADDR), SNAPSHOT_TICKS);
    assert_eq!(vm.serial_output_len(), 0);

    // Advance a few more milliseconds (still waiting for the next PIT tick) so the snapshot
    // captures a non-trivial PIT phase.
    for _ in 0..3 {
        let exit = vm.run_slice(10_000);
        assert!(matches!(exit, RunExit::Halted { .. }));
        assert_eq!(vm.read_physical_u16(COUNTER_ADDR), SNAPSHOT_TICKS);
        assert_eq!(vm.serial_output_len(), 0);
    }

    let snap = vm.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.set_disk_image(boot.to_vec()).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Resume to completion and compare with the baseline run.
    let (resumed_out, resumed_ticks) = run_until_final_marker(&mut restored);
    assert_eq!(resumed_out, baseline_out);
    assert_eq!(resumed_ticks, baseline_ticks);
}
