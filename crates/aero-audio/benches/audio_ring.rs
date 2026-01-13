#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_audio::ring::AudioRingBuffer;
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
fn bench_audio_ring(c: &mut Criterion) {
    // A small micro-benchmark for the pure-Rust `AudioRingBuffer` used in native
    // builds and unit tests.
    //
    // This specifically exercises the wrap-around copy logic in
    // `push_interleaved_stereo`/`pop_interleaved_stereo` which should run in O(1)
    // slice copies (contiguous + optional wrap segment), rather than copying
    // frame-by-frame in a loop.
    let frames_per_call = 128usize;
    let samples: Vec<f32> = (0..frames_per_call * 2)
        .map(|i| (i as f32) * 0.001)
        .collect();

    let bytes_per_call = (samples.len() * core::mem::size_of::<f32>()) as u64;

    let mut group = c.benchmark_group("audio_ring");
    group.throughput(Throughput::Bytes(bytes_per_call));

    for &(name, capacity_frames) in &[("contiguous", 1024usize), ("wrap", 1000usize)] {
        group.bench_function(BenchmarkId::new("push_pop", name), |b| {
            let mut ring = AudioRingBuffer::new_stereo(capacity_frames);
            b.iter(|| {
                ring.push_interleaved_stereo(black_box(&samples));
                let out = ring.pop_interleaved_stereo(frames_per_call);
                black_box(out);
            })
        });
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_audio_ring);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
