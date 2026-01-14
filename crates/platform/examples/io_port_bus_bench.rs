//! Micro-benchmark harness for `IoPortBus` exact-port dispatch.
//!
//! Run with:
//! ```bash
//! cargo run -p aero-platform --example io_port_bus_bench --release
//! ```
//!
//! The benchmark measures a tight loop of exact-port `read_u8` operations.
//! Exact-port dispatch is hot in real-mode BIOS and legacy device workloads
//! (e.g. PIT/RTC/i8042/etc).
//!
//! ## Example measurements
//! Using `--release` on the same machine, switching from a `HashMap<u16, _>` to a
//! 64Ki-entry table for exact-port dispatch reduced this benchmark from ~24 ns/op
//! to ~3 ns/op (≈7-8× faster). Numbers will vary by CPU and compiler version.

use aero_platform::io::{IoPortBus, PortIoDevice};
use std::hint::black_box;
use std::time::Instant;

#[derive(Debug)]
struct Noop;

impl PortIoDevice for Noop {
    fn read(&mut self, _port: u16, _size: u8) -> u32 {
        0
    }

    fn write(&mut self, _port: u16, _size: u8, _value: u32) {}
}

fn main() {
    const PORT: u16 = 0x0080;
    const ITERS: u64 = 50_000_000;

    let mut bus = IoPortBus::new();
    bus.register(PORT, Box::new(Noop));

    // Warm up caches / branch predictor.
    for _ in 0..10_000 {
        black_box(bus.read_u8(PORT));
    }

    let start = Instant::now();
    let mut acc: u64 = 0;
    for _ in 0..ITERS {
        acc = acc.wrapping_add(u64::from(bus.read_u8(PORT)));
    }
    let elapsed = start.elapsed();
    black_box(acc);

    let ns_per_op = elapsed.as_secs_f64() * 1e9 / (ITERS as f64);
    println!("IoPortBus exact-port read_u8: {ITERS} iters in {elapsed:?} => {ns_per_op:.2} ns/op");
}
