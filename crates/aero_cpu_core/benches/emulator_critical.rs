use std::time::Duration;

use aero_cpu_core::bus::{Bus, RamBus};
use aero_cpu_core::cpu::{Cpu, CpuMode};
use aero_cpu_core::interp;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep PR runtime low.
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

fn make_stringop_stream() -> Vec<u8> {
    // A deterministic stream of string instructions to exercise:
    // - legacy prefixes (F2/F3/66/67/segment override)
    // - REX in long mode
    // - opcode dispatch for different string ops
    const PATTERNS: &[&[u8]] = &[
        // MOVSB / MOVSQ (w/ REX.W)
        &[0xA4],
        &[0x48, 0xA5],
        // REP MOVSB
        &[0xF3, 0xA4],
        // REPNE CMPSB (F2 + A6)
        &[0xF2, 0xA6],
        // REPE SCASB (F3 + AE)
        &[0xF3, 0xAE],
        // Segment override + MOVSB
        &[0x2E, 0xA4],
        // Address size override + STOSB
        &[0x67, 0xAA],
        // Operand size override + STOSD (A5 semantics)
        &[0x66, 0xAB],
    ];

    let mut out = Vec::with_capacity(64 * 1024);
    while out.len() < 64 * 1024 {
        for pat in PATTERNS {
            out.extend_from_slice(pat);
        }
    }
    out
}

fn bench_decoder_throughput(c: &mut Criterion) {
    let code = make_stringop_stream();
    let mut pos = 0usize;

    c.bench_function("decoder_throughput", |b| {
        b.iter(|| {
            let mut checksum = 0u64;
            for _ in 0..8_192 {
                if pos >= code.len() {
                    pos = 0;
                }
                let inst =
                    aero_cpu_core::interp::decode::decode(CpuMode::Long64, black_box(&code[pos..]))
                        .unwrap();
                checksum = checksum.wrapping_add((inst.len as u64) ^ (pos as u64));
                pos += inst.len;
            }
            black_box(checksum);
        });
    });
}

fn bench_interpreter_hot_loop(c: &mut Criterion) {
    let decoded = aero_cpu_core::interp::decode::decode(CpuMode::Protected32, &[0xAA]).unwrap();
    let mut cpu = Cpu::new(CpuMode::Protected32);
    let mut bus = RamBus::new(256 * 1024);

    c.bench_function("interpreter_hot_loop", |b| {
        b.iter(|| {
            cpu.regs.set_eax(0xA5, cpu.mode);
            cpu.regs.set_edi(0, cpu.mode);

            let mut checksum = 0u64;
            for _ in 0..50_000 {
                interp::exec(&mut cpu, &mut bus, black_box(&decoded)).unwrap();
                checksum ^= cpu.regs.rdi;
            }
            black_box(checksum);
        });
    });
}

fn bench_memory_bulk_copy(c: &mut Criterion) {
    let len = 1024 * 1024;
    let mut bus = RamBus::new(2 * len);

    let mut group = c.benchmark_group("memory");
    group.throughput(Throughput::Bytes(len as u64));
    group.bench_function("bulk_copy_1mib", |b| {
        b.iter(|| {
            let ok = bus.bulk_copy(len as u64, 0, len);
            black_box(ok);
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_decoder_throughput, bench_interpreter_hot_loop, bench_memory_bulk_copy
}
criterion_main!(benches);
