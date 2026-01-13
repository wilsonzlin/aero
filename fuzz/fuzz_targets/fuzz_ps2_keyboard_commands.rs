#![no_main]

use aero_devices_input::i8042::MAX_PENDING_OUTPUT;
use aero_devices_input::{I8042Controller, IrqSink, SystemControlSink};
use aero_io_snapshot::io::state::IoSnapshot;
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

/// Upper bound on the number of bytes written to the keyboard per testcase.
const MAX_BYTES: usize = 2048;

/// Drain a small amount of output after each write to avoid building up large queues and to
/// exercise the read path.
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

    for &b in &input.bytes {
        // Send a byte to the keyboard through the i8042 data port.
        dev.write_port(0x60, b);

        // Opportunistically drain a few bytes of output to keep state small.
        for _ in 0..DRAIN_READS_PER_WRITE {
            let _ = dev.read_port(0x60);
        }

        assert!(dev.pending_output_len() <= MAX_PENDING_OUTPUT);
    }

    let snapshot = dev.save_state();
    assert!(snapshot.len() <= MAX_SNAPSHOT_BYTES);

    // Snapshot/restore should always succeed on a self-generated snapshot.
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

