use aero_virtio::devices::input::{BTN_LEFT, KEY_A};
use aero_wasm::Machine;

#[test]
fn virtio_input_injection_advances_device_state_when_enabled() {
    // Keep the RAM size modest for a fast smoke test while still leaving room for the canonical PC
    // platform topology.
    let cfg = aero_machine::MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep the test minimal: no NIC/USB required.
        enable_e1000: false,
        enable_virtio_net: false,
        enable_uhci: false,
        ..Default::default()
    };

    let mut m =
        Machine::new_with_machine_config(cfg).expect("Machine::new_with_machine_config should succeed");
    assert_eq!(m.virtio_input_keyboard_pending_events(), 0);
    assert_eq!(m.virtio_input_mouse_pending_events(), 0);
    assert!(!m.virtio_input_keyboard_driver_ok());
    assert!(!m.virtio_input_mouse_driver_ok());

    // Injecting a key should enqueue at least one pending event on the virtio-input keyboard.
    m.inject_virtio_key(u32::from(KEY_A), true);
    assert!(
        m.virtio_input_keyboard_pending_events() > 0,
        "inject_virtio_key should enqueue events when virtio-input is enabled"
    );

    // Mouse events should enqueue on the mouse function.
    m.inject_virtio_rel(10, -5);
    m.inject_virtio_button(u32::from(BTN_LEFT), true);
    let before = m.virtio_input_mouse_pending_events();
    m.inject_virtio_wheel(1);
    assert!(
        m.virtio_input_mouse_pending_events() > before,
        "inject_virtio_wheel should enqueue events when virtio-input is enabled"
    );

    let before = m.virtio_input_mouse_pending_events();
    m.inject_virtio_hwheel(-2);
    assert!(
        m.virtio_input_mouse_pending_events() > before,
        "inject_virtio_hwheel should enqueue events when virtio-input is enabled"
    );

    let before = m.virtio_input_mouse_pending_events();
    m.inject_virtio_wheel2(3, -3);
    assert!(
        m.virtio_input_mouse_pending_events() > before,
        "inject_virtio_wheel2 should enqueue events when virtio-input is enabled"
    );
    assert!(
        m.virtio_input_mouse_pending_events() > 0,
        "mouse injections should enqueue events when virtio-input is enabled"
    );
}
