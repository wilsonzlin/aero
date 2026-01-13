#![no_main]

use aero_devices_input::i8042::MAX_PENDING_OUTPUT;
use aero_devices_input::{I8042Controller, IrqSink, SystemControlSink};
use aero_io_snapshot::io::state::IoSnapshot;
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

/// Upper bound on the number of mouse bytes sent per testcase.
const MAX_BYTES: usize = 2048;

/// Drain a small amount of output after each write-to-mouse to keep internal queues small.
const DRAIN_READS_PER_WRITE: usize = 4;

/// Upper bound on `I8042Controller::save_state()` size.
const MAX_SNAPSHOT_BYTES: usize = 128 * 1024;

#[derive(Default)]
struct CountingIrqSink {
    count: u32,
}

impl IrqSink for CountingIrqSink {
    fn raise_irq(&mut self, _irq: u8) {
        self.count = self.count.saturating_add(1);
    }
}

#[derive(Default)]
struct CountingSystemControlSink {
    a20: bool,
}

impl SystemControlSink for CountingSystemControlSink {
    fn set_a20(&mut self, enabled: bool) {
        self.a20 = enabled;
    }

    fn request_reset(&mut self) {}

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20)
    }
}

#[derive(Debug)]
struct Input {
    bytes: Vec<u8>,
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let len = u.int_in_range(0..=MAX_BYTES)?;
        let bytes = u.bytes(len)?.to_vec();
        Ok(Self { bytes })
    }
}

fn run(input: &Input) -> Vec<u8> {
    let mut dev = I8042Controller::new();
    dev.set_irq_sink(Box::new(CountingIrqSink::default()));
    dev.set_system_control_sink(Box::new(CountingSystemControlSink::default()));

    // Enable IRQ12 to exercise the AUX IRQ path when the mouse produces output.
    dev.write_port(0x64, 0x60); // write command byte
    dev.write_port(0x60, 0x45 | 0x02);

    for &b in &input.bytes {
        // i8042 "write to mouse": command 0xD4 then data byte.
        dev.write_port(0x64, 0xD4);
        dev.write_port(0x60, b);

        // Drain a few bytes of output (ACKs, IDs, status, etc).
        for _ in 0..DRAIN_READS_PER_WRITE {
            let _ = dev.read_port(0x60);
        }

        assert!(dev.pending_output_len() <= MAX_PENDING_OUTPUT);
    }

    let snapshot = dev.save_state();
    assert!(snapshot.len() <= MAX_SNAPSHOT_BYTES);

    let mut restored = I8042Controller::new();
    restored
        .load_state(&snapshot)
        .expect("load_state(save_state()) should succeed");

    snapshot
}

fuzz_target!(|input: Input| {
    let a = run(&input);
    let b = run(&input);
    assert_eq!(a, b);
});

