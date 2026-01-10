#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;
use memory::{Bus, MmioHandler};

#[derive(Default)]
struct RecordingMmio {
    log: Vec<(bool, u64, usize, u64)>,
    storage: std::collections::BTreeMap<u64, u8>,
}

impl RecordingMmio {
    fn read_byte(&self, offset: u64) -> u8 {
        *self.storage.get(&offset).unwrap_or(&0)
    }
}

impl MmioHandler for RecordingMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut out = 0u64;
        for i in 0..size.min(8) {
            out |= (self.read_byte(offset.wrapping_add(i as u64)) as u64) << (i * 8);
        }
        self.log.push((false, offset, size, out));
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        for i in 0..size.min(8) {
            let byte = ((value >> (i * 8)) & 0xff) as u8;
            self.storage.insert(offset.wrapping_add(i as u64), byte);
        }
        self.log.push((true, offset, size, value));
    }
}

#[derive(Clone, Copy)]
enum Op {
    Read { addr: u64, size: usize },
    Write { addr: u64, size: usize, value: u64 },
}

fn run(data: &[u8]) -> Vec<u64> {
    let mut u = Unstructured::new(data);

    let ram_size: usize = u.int_in_range(0usize..=64 * 1024).unwrap_or(0);
    let mut bus = Bus::new(ram_size);

    let map_ops: usize = u.int_in_range(0usize..=16).unwrap_or(0);
    for _ in 0..map_ops {
        let kind: u8 = u.int_in_range(0u8..=1).unwrap_or(0);
        let start: u64 = u.arbitrary().unwrap_or(0);
        let len: u16 = u.arbitrary().unwrap_or(0);
        let len = len as u64;
        if len == 0 {
            continue;
        }

        match kind {
            0 => {
                let mut rom = vec![0u8; len.min(256) as usize];
                for b in &mut rom {
                    *b = u.arbitrary().unwrap_or(0);
                }
                bus.map_rom(start, rom);
            }
            _ => {
                bus.map_mmio(start, len, Box::new(RecordingMmio::default()));
            }
        }
    }

    let rw_ops: usize = u.int_in_range(0usize..=64).unwrap_or(0);
    let mut ops = Vec::with_capacity(rw_ops);
    for _ in 0..rw_ops {
        let is_write: bool = u.arbitrary().unwrap_or(false);
        let addr: u64 = u.arbitrary().unwrap_or(0);
        let size_pow: u8 = u.int_in_range(0u8..=3).unwrap_or(0);
        let size = 1usize << size_pow;
        if is_write {
            let value: u64 = u.arbitrary().unwrap_or(0);
            ops.push(Op::Write { addr, size, value });
        } else {
            ops.push(Op::Read { addr, size });
        }
    }

    let mut reads = Vec::new();
    for op in ops {
        match op {
            Op::Read { addr, size } => reads.push(bus.read(addr, size)),
            Op::Write { addr, size, value } => bus.write(addr, size, value),
        }
    }
    reads
}

fuzz_target!(|data: &[u8]| {
    let a = run(data);
    let b = run(data);
    assert_eq!(a, b);
});
