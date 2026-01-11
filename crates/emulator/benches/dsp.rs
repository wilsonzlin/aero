#[cfg(not(feature = "legacy-audio"))]
fn main() {}

#[cfg(feature = "legacy-audio")]
use emulator::io::audio::dsp::pcm::{PcmSampleFormat, PcmSpec};
#[cfg(feature = "legacy-audio")]
use emulator::io::audio::dsp::resample::ResamplerKind;
#[cfg(feature = "legacy-audio")]
use emulator::io::audio::dsp::StreamProcessor;
#[cfg(feature = "legacy-audio")]
use criterion::{black_box, criterion_group, criterion_main, Criterion};

#[cfg(feature = "legacy-audio")]
fn bench_convert_44k1_s16_stereo_to_48k_stereo(c: &mut Criterion) {
    let input_spec = PcmSpec {
        format: PcmSampleFormat::I16,
        channels: 2,
        sample_rate: 44_100,
    };

    let mut proc = StreamProcessor::new(input_spec, 48_000, 2, ResamplerKind::Linear).unwrap();

    // 10ms of audio at 44.1 kHz.
    let frames = 441;
    let mut bytes = Vec::with_capacity(frames * 2 * 2);
    for i in 0..frames {
        let l = ((i as i32 * 257 + 12345) % 65536 - 32768) as i16;
        let r = (-l) as i16;
        bytes.extend_from_slice(&l.to_le_bytes());
        bytes.extend_from_slice(&r.to_le_bytes());
    }

    let mut out = Vec::new();
    c.bench_function("dsp/convert/s16_44k1_stereo_to_48k_stereo", |b| {
        b.iter(|| {
            proc.process(black_box(&bytes), black_box(&mut out))
                .unwrap();
            black_box(&out);
        })
    });
}

#[cfg(feature = "legacy-audio")]
criterion_group!(benches, bench_convert_44k1_s16_stereo_to_48k_stereo);
#[cfg(feature = "legacy-audio")]
criterion_main!(benches);
