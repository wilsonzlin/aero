#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_devices_input::I8042Controller;
use aero_io_snapshot::io::state::IoSnapshot;

const MAX_OPS: usize = 1024;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    let mut ctl = I8042Controller::new();

    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 6 {
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
            _ => {
                // Snapshot roundtrip: save_state -> load_state into a fresh controller, then keep
                // going from the restored state.
                let snap = ctl.save_state();
                let mut fresh = I8042Controller::new();
                let _ = fresh.load_state(&snap);
                ctl = fresh;
            }
        }
    }
});

