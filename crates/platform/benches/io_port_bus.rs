use std::collections::HashMap;
use std::time::Duration;

use aero_platform::io::{IoPortBus, PortIoDevice};
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep CI runtime low.
            .warm_up_time(Duration::from_millis(150))
            .measurement_time(Duration::from_millis(400))
            .sample_size(20)
            .noise_threshold(0.05),
        _ => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(50)
            .noise_threshold(0.03),
    }
}

#[derive(Debug)]
struct NoopDevice;

impl PortIoDevice for NoopDevice {
    fn read(&mut self, _port: u16, size: u8) -> u32 {
        // Return a size-shaped value so the call isn't trivially predictable.
        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0,
        }
    }

    fn write(&mut self, _port: u16, _size: u8, _value: u32) {}
}

/// A minimal baseline bus that uses a `HashMap` for exact-port dispatch.
///
/// This is used only for benchmarking the lookup overhead that `IoPortBus` eliminates by
/// switching to a fixed 64K table. It intentionally mirrors the access-size semantics of
/// `IoPortBus::read` for apples-to-apples behavior.
struct HashMapIoPortBus {
    devices: HashMap<u16, Box<dyn PortIoDevice>>,
}

impl HashMapIoPortBus {
    fn new() -> Self {
        Self {
            devices: HashMap::new(),
        }
    }

    fn register(&mut self, port: u16, dev: Box<dyn PortIoDevice>) {
        self.devices.insert(port, dev);
    }

    fn read(&mut self, port: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        if !matches!(size, 1 | 2 | 4) {
            return 0xFFFF_FFFF;
        }

        if let Some(dev) = self.devices.get_mut(&port) {
            return dev.read(port, size);
        }

        match size {
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }
}

fn bench_exact_port_hit(c: &mut Criterion) {
    let port: u16 = 0x03f8;

    let mut map_bus = HashMapIoPortBus::new();
    map_bus.register(port, Box::new(NoopDevice));

    let mut table_bus = IoPortBus::new();
    table_bus.register(port, Box::new(NoopDevice));

    c.bench_function("io_port_bus_exact_hit/hashmap", |b| {
        b.iter(|| black_box(map_bus.read(black_box(port), 4)))
    });
    c.bench_function("io_port_bus_exact_hit/table", |b| {
        b.iter(|| black_box(table_bus.read(black_box(port), 4)))
    });
}

fn bench_exact_port_miss(c: &mut Criterion) {
    let port: u16 = 0x03f8;

    let mut map_bus = HashMapIoPortBus::new();
    let mut table_bus = IoPortBus::new();

    c.bench_function("io_port_bus_exact_miss/hashmap", |b| {
        b.iter(|| black_box(map_bus.read(black_box(port), 4)))
    });
    c.bench_function("io_port_bus_exact_miss/table", |b| {
        b.iter(|| black_box(table_bus.read(black_box(port), 4)))
    });
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_exact_port_hit, bench_exact_port_miss
}
criterion_main!(benches);
