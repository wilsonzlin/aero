#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_input::i8042::MAX_PENDING_OUTPUT;
use aero_devices_input::{I8042Controller, IrqSink, SystemControlSink};
use aero_devices_input::Ps2MouseButton;
use aero_io_snapshot::io::state::IoSnapshot;

const MAX_OPS: usize = 1024;

#[derive(Default)]
struct NullIrqSink;

impl IrqSink for NullIrqSink {
    fn raise_irq(&mut self, _irq: u8) {}
}

#[derive(Default)]
struct NullSystemControlSink {
    a20: bool,
}

impl SystemControlSink for NullSystemControlSink {
    fn set_a20(&mut self, enabled: bool) {
        self.a20 = enabled;
    }

    fn request_reset(&mut self) {}

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20)
    }
}

fn seed_mouse(ctl: &mut I8042Controller) {
    // Enable IRQ12 (mouse) in the i8042 command byte so AUX output exercises the IRQ path.
    ctl.write_port(0x64, 0x60);
    ctl.write_port(0x60, 0x47);

    // Helper: send one byte to the mouse via i8042 command 0xD4.
    let mut send_mouse = |b: u8| {
        ctl.write_port(0x64, 0xD4);
        ctl.write_port(0x60, b);
    };

    // Enable IntelliMouse Explorer (wheel + side/extra buttons): sample-rate sequence 200,200,80.
    for rate in [200u8, 200u8, 80u8] {
        send_mouse(0xF3); // set sample rate
        send_mouse(rate);
    }

    // Enable data reporting so injected motion produces packets.
    send_mouse(0xF4);

    // Drain any queued ACK bytes from the output buffer so subsequent injection isn't blocked by a
    // full buffer.
    for _ in 0..32 {
        let _ = ctl.read_port(0x60);
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    let mut ctl = I8042Controller::new();
    ctl.set_irq_sink(Box::new(NullIrqSink::default()));
    ctl.set_system_control_sink(Box::new(NullSystemControlSink::default()));
    seed_mouse(&mut ctl);

    // Ensure we always execute at least one operation so even empty inputs stress the state
    // machines and invariants.
    let ops: usize = u.int_in_range(1usize..=MAX_OPS).unwrap_or(1);
    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 8 {
            0 => {
                let _ = ctl.read_port(0x60);
            }
            1 => {
                let _ = ctl.read_port(0x64);
            }
            2 => {
                let v: u8 = u.arbitrary().unwrap_or(0);
                ctl.write_port(0x60, v);
            }
            3 => {
                let v: u8 = u.arbitrary().unwrap_or(0);
                ctl.write_port(0x64, v);
            }
            4 => {
                // Host injection: raw Set-2 scancode bytes. Keep bounded to avoid unbounded internal
                // queue growth.
                let len: usize = u.int_in_range(1usize..=8).unwrap_or(1);
                let mut buf = [0u8; 8];
                for b in &mut buf[..len] {
                    *b = u.arbitrary().unwrap_or(0);
                }
                ctl.inject_key_scancode_bytes(&buf[..len]);
            }
            5 => {
                // Host injection: mouse motion. Keep deltas bounded so each injection doesn't
                // dominate runtime (PS/2 splits large motion into multiple packets).
                let dx: i32 = i32::from(u.arbitrary::<i16>().unwrap_or(0)).clamp(-1024, 1024);
                let dy: i32 = i32::from(u.arbitrary::<i16>().unwrap_or(0)).clamp(-1024, 1024);
                let wheel: i32 = i32::from(u.arbitrary::<i16>().unwrap_or(0)).clamp(-256, 256);
                ctl.inject_mouse_motion(dx, dy, wheel);
            }
            6 => {
                // Host injection: mouse button state.
                let which: u8 = u.arbitrary().unwrap_or(0);
                let pressed: bool = u.arbitrary().unwrap_or(false);
                let button = match which % 5 {
                    0 => Ps2MouseButton::Left,
                    1 => Ps2MouseButton::Right,
                    2 => Ps2MouseButton::Middle,
                    3 => Ps2MouseButton::Side,
                    _ => Ps2MouseButton::Extra,
                };
                ctl.inject_mouse_button(button, pressed);
            }
            _ => {
                // Snapshot roundtrip: save_state -> load_state into a fresh controller, then keep
                // going from the restored state.
                let snap = ctl.save_state();
                let mut fresh = I8042Controller::new();
                let _ = fresh.load_state(&snap);
                fresh.set_irq_sink(Box::new(NullIrqSink::default()));
                fresh.set_system_control_sink(Box::new(NullSystemControlSink::default()));
                ctl = fresh;
            }
        }

        // Invariant: controller buffering must never grow without bound.
        assert!(
            ctl.pending_output_len() <= MAX_PENDING_OUTPUT,
            "pending_output_len={} exceeded MAX_PENDING_OUTPUT={}",
            ctl.pending_output_len(),
            MAX_PENDING_OUTPUT
        );
    }
});
