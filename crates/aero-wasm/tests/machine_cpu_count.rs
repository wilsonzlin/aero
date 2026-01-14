use aero_wasm::Machine;

#[test]
fn machine_constructor_defaults_to_one_cpu() {
    let m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");
    assert_eq!(m.cpu_count(), 1);
}

#[test]
fn machine_new_with_cpu_count_sets_configured_cpu_count() {
    let m = Machine::new_with_cpu_count(16 * 1024 * 1024, 2)
        .expect("Machine::new_with_cpu_count should succeed");
    assert_eq!(m.cpu_count(), 2);
}

#[test]
fn machine_new_with_config_can_override_cpu_count() {
    let m = Machine::new_with_config(16 * 1024 * 1024, false, None, Some(4))
        .expect("Machine::new_with_config(cpu_count=4) should succeed");
    assert_eq!(m.cpu_count(), 4);
}
