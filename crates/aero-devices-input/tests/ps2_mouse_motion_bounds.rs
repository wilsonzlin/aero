use aero_devices_input::Ps2Mouse;

fn enable_reporting(mouse: &mut Ps2Mouse) {
    mouse.receive_byte(0xF4);
    assert_eq!(
        mouse.pop_output(),
        Some(0xFA),
        "mouse should ACK ENABLE_DATA_REPORTING"
    );
    assert!(
        !mouse.has_output(),
        "unexpected extra output bytes after ENABLE_DATA_REPORTING"
    );
}

#[test]
fn ps2_mouse_bounds_packet_generation_for_extreme_motion() {
    let mut mouse = Ps2Mouse::new();
    enable_reporting(&mut mouse);

    // Host input is untrusted; absurd deltas must not translate into unbounded packet generation.
    mouse.inject_motion(i32::MAX, 0, 0);

    let mut bytes = 0usize;
    while mouse.has_output() {
        let _ = mouse.pop_output();
        bytes += 1;
        if bytes > 10_000 {
            panic!("unexpectedly large PS/2 mouse output queue (possible unbounded motion splitting)");
        }
    }

    // The output queue is bounded, but we should keep headroom by limiting packets per injection.
    assert!(
        bytes < 4096,
        "expected bounded output for extreme motion, got {bytes} bytes"
    );
}

