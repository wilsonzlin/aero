use aero_machine::{PcMachine, RunExit};
use firmware::bios::{BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};
use memory::MemoryBus as _;
use pretty_assertions::assert_eq;

fn busy_loop_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    // cli
    sector[0] = 0xFA;
    // jmp short $-2 (infinite loop)
    sector[1] = 0xEB;
    sector[2] = 0xFE;
    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn read_bda_tick_count(pc: &mut PcMachine) -> u32 {
    pc.platform_mut().memory.read_u32(BDA_TICK_COUNT_ADDR)
}

#[test]
fn pc_machine_run_slice_advances_bios_bda_ticks_deterministically() {
    let mut pc = PcMachine::new(2 * 1024 * 1024);
    pc.set_disk_image(busy_loop_boot_sector().to_vec()).unwrap();
    pc.reset();

    // Use a small deterministic TSC frequency so we can advance 1 second worth of guest time with a
    // small number of instructions (Tier-0 currently uses 1 cycle per instruction).
    pc.cpu.time.set_tsc_hz(1000);
    pc.cpu.state.msr.tsc = pc.cpu.time.read_tsc();

    let start = read_bda_tick_count(&mut pc);

    let cycles_per_second = pc.cpu.time.tsc_hz();
    assert!(cycles_per_second > 0);
    for elapsed_secs in 1u64..=10 {
        match pc.run_slice(cycles_per_second) {
            RunExit::Completed { executed } => assert_eq!(executed, cycles_per_second),
            other => panic!("unexpected run exit: {other:?}"),
        }

        let expected_delta: u32 = (u64::from(TICKS_PER_DAY) * elapsed_secs / 86_400)
            .try_into()
            .unwrap();
        let expected = start.wrapping_add(expected_delta);
        assert_eq!(
            read_bda_tick_count(&mut pc),
            expected,
            "unexpected tick count after {elapsed_secs} seconds"
        );
    }
}
