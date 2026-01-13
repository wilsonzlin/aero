#![no_main]

use aero_devices_input::i8042::MAX_PENDING_OUTPUT;
use aero_devices_input::{I8042Controller, IrqSink, SystemControlSink};
use aero_io_snapshot::io::state::IoSnapshot;
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

/// Keep each fuzz input bounded so we don't end up doing millions of I/O ops per testcase.
const MAX_OPS: usize = 256;

/// Upper bound on `I8042Controller::save_state()` size.
///
/// With all internal queues bounded to 4096 bytes, the snapshot should stay well under this.
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
    resets: u32,
}

impl SystemControlSink for CountingSystemControlSink {
    fn set_a20(&mut self, enabled: bool) {
        self.a20 = enabled;
    }

    fn request_reset(&mut self) {
        self.resets = self.resets.saturating_add(1);
    }

    fn a20_enabled(&self) -> Option<bool> {
        Some(self.a20)
    }
}

#[derive(Debug, Clone, Copy, Arbitrary)]
struct IoOp {
    is_write: bool,
    port: u16,
    size: u8,
    value: u32,
}

#[derive(Debug)]
struct Input {
    ops: Vec<IoOp>,
}

impl<'a> Arbitrary<'a> for Input {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let len = u.int_in_range(0..=MAX_OPS)?;
        let mut ops = Vec::with_capacity(len);
        for _ in 0..len {
            ops.push(IoOp::arbitrary(u)?);
        }
        Ok(Self { ops })
    }
}

fn map_port(raw: u16) -> u16 {
    // Bias towards the interesting i8042 ports while keeping some "random port" coverage.
    match raw % 10 {
        0 => raw,
        1..=5 => 0x60,
        _ => 0x64,
    }
}

fn map_size(raw: u8) -> usize {
    match raw % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn write_sized(dev: &mut I8042Controller, port: u16, size: usize, value: u32) {
    // Model a little-endian multi-byte port I/O transaction as a sequence of byte I/Os. This is
    // simple and still exercises the internal command/data state machines.
    for i in 0..size {
        let byte = (value >> (i * 8)) as u8;
        dev.write_port(port, byte);
    }
}

fn read_sized(dev: &mut I8042Controller, port: u16, size: usize) -> u32 {
    let mut out = 0u32;
    for i in 0..size {
        out |= (dev.read_port(port) as u32) << (i * 8);
    }
    out
}

fn run(input: &Input) -> (Vec<u32>, Vec<u8>) {
    let mut dev = I8042Controller::new();
    dev.set_irq_sink(Box::new(CountingIrqSink::default()));
    dev.set_system_control_sink(Box::new(CountingSystemControlSink::default()));

    let mut reads = Vec::new();

    for op in &input.ops {
        let port = map_port(op.port);
        let size = map_size(op.size);

        if op.is_write {
            write_sized(&mut dev, port, size, op.value);
        } else {
            reads.push(read_sized(&mut dev, port, size));
        }

        // Invariant: controller buffering must never grow without bound.
        assert!(
            dev.pending_output_len() <= MAX_PENDING_OUTPUT,
            "pending_output_len={} exceeded MAX_PENDING_OUTPUT={}",
            dev.pending_output_len(),
            MAX_PENDING_OUTPUT
        );
    }

    // Snapshot/restore should always succeed on a self-generated snapshot.
    let snapshot = dev.save_state();
    assert!(
        snapshot.len() <= MAX_SNAPSHOT_BYTES,
        "snapshot grew unexpectedly: {} bytes",
        snapshot.len()
    );

    let mut restored = I8042Controller::new();
    restored
        .load_state(&snapshot)
        .expect("load_state(save_state()) should succeed");
    assert!(restored.pending_output_len() <= MAX_PENDING_OUTPUT);

    (reads, snapshot)
}

fuzz_target!(|input: Input| {
    // Run twice to assert determinism (helps libFuzzer triage and catches accidental use of
    // randomness/time).
    let a = run(&input);
    let b = run(&input);
    assert_eq!(a, b);
});

