use aero_machine::{Machine, MachineConfig};
use firmware::bios::{BDA_TICK_COUNT_ADDR, TICKS_PER_DAY};
use pretty_assertions::assert_eq;

fn read_bda_tick_count(m: &mut Machine) -> u32 {
    let bytes = m.read_physical_bytes(BDA_TICK_COUNT_ADDR, 4);
    u32::from_le_bytes(bytes.try_into().expect("BDA tick count is 4 bytes"))
}

#[test]
fn machine_tick_advances_bda_tick_count_deterministically() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .unwrap();

    let start = read_bda_tick_count(&mut m);

    // Advance time by whole seconds and ensure the BDA tick count tracks the BIOS PIT-derived tick
    // rate (~18.2Hz), including fractional tick accumulation.
    for elapsed_secs in 1u64..=10 {
        m.tick(1_000_000_000);

        let expected_delta =
            (u64::from(TICKS_PER_DAY) * elapsed_secs / 86_400).try_into().unwrap();
        let expected = start.wrapping_add(expected_delta);

        assert_eq!(
            read_bda_tick_count(&mut m),
            expected,
            "unexpected tick count after {elapsed_secs} seconds"
        );
    }
}

