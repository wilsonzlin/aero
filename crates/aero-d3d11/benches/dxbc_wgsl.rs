//! Criterion benchmarks for DXBC → WGSL translation hot paths.
//!
//! These benchmarks exercise the most performance-sensitive parts of the D3D11
//! shader pipeline:
//! - `aero_dxbc::DxbcFile::parse`
//! - SM4 token parsing + IR decode (`Sm4Program::parse_from_dxbc` + `sm4::decode_program`)
//! - WGSL translation (signature-driven and the legacy bootstrap path)
//!
//! ## Running
//!
//! From the repo root:
//! ```bash
//! cargo bench -p aero-d3d11 --bench dxbc_wgsl -- --noplot
//! ```
//! (Omit `-- --noplot` to generate Criterion plots/reports.)
//!
//! Note: Criterion's cargo integration also supports a fast "smoke test" mode
//! (no timings) when the benchmark binary is executed without `--bench`. Using
//! `cargo bench` as shown above will run the full benchmark measurements.
//! (Avoid passing `--bench` yourself after `--`; Cargo already supplies it, and
//! duplicating it can cause argument parsing errors.)
//!
//! Shader fixture blobs are loaded from `crates/aero-d3d11/tests/fixtures/*.dxbc`.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_d3d11::{
    parse_signatures, sm4::decode_program, translate_sm4_module_to_wgsl,
    translate_sm4_to_wgsl_bootstrap, Sm4Program,
};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
fn load_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_dxbc_parse(c: &mut Criterion) {
    let fixtures = [
        "vs_passthrough.dxbc",
        "ps_passthrough.dxbc",
        "vs_matrix.dxbc",
        "ps_sample.dxbc",
    ];

    let mut group = c.benchmark_group("dxbc_parse");
    for name in fixtures {
        let bytes = load_fixture(name);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, bytes| {
            b.iter(|| {
                let dxbc = aero_dxbc::DxbcFile::parse(black_box(bytes.as_slice()))
                    .expect("fixture should parse as DXBC");
                black_box(dxbc);
            })
        });
    }
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_sm4_decode(c: &mut Criterion) {
    let fixtures = ["vs_matrix.dxbc", "ps_sample.dxbc"];

    let mut group = c.benchmark_group("sm4_decode");
    for name in fixtures {
        let bytes = load_fixture(name);
        let dxbc =
            aero_dxbc::DxbcFile::parse(&bytes).expect("fixture should parse as DXBC container");
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                let program =
                    Sm4Program::parse_from_dxbc(black_box(&dxbc)).expect("SM4 parse failed");
                let module = decode_program(&program).expect("SM4 decode failed");
                black_box(module);
            })
        });
    }
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_wgsl_translate(c: &mut Criterion) {
    // Signature-driven translator (the "real" path).
    {
        let fixtures = ["vs_matrix.dxbc", "ps_sample.dxbc"];
        let mut group = c.benchmark_group("wgsl_translate_signature_driven");
        for name in fixtures {
            let bytes = load_fixture(name);
            let dxbc = aero_dxbc::DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");
            let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
            let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse failed");
            let module = decode_program(&program).expect("SM4 decode failed");

            group.bench_function(BenchmarkId::from_parameter(name), |b| {
                b.iter(|| {
                    let translated = translate_sm4_module_to_wgsl(
                        black_box(&dxbc),
                        black_box(&module),
                        black_box(&signatures),
                    )
                    .expect("signature-driven WGSL translation failed");
                    black_box(translated.wgsl);
                })
            });
        }
        group.finish();
    }

    // Legacy bootstrap translator (MOV/RET-only).
    {
        let fixtures = ["vs_passthrough.dxbc", "ps_passthrough.dxbc"];
        let mut group = c.benchmark_group("wgsl_translate_bootstrap");
        for name in fixtures {
            let bytes = load_fixture(name);
            let dxbc = aero_dxbc::DxbcFile::parse(&bytes).expect("fixture should parse as DXBC");
            let program = Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 parse failed");

            group.bench_function(BenchmarkId::from_parameter(name), |b| {
                b.iter(|| {
                    let translated = translate_sm4_to_wgsl_bootstrap(black_box(&program))
                        .expect("bootstrap WGSL translation failed");
                    black_box(translated.wgsl);
                })
            });
        }
        group.finish();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_end_to_end(c: &mut Criterion) {
    // End-to-end signature-driven path (DXBC bytes → WGSL).
    {
        let fixtures = ["vs_matrix.dxbc", "ps_sample.dxbc"];
        let mut group = c.benchmark_group("dxbc_to_wgsl_end_to_end");
        for name in fixtures {
            let bytes = load_fixture(name);
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, bytes| {
                b.iter(|| {
                    let dxbc = aero_dxbc::DxbcFile::parse(black_box(bytes.as_slice()))
                        .expect("fixture should parse as DXBC");
                    let signatures = parse_signatures(&dxbc).expect("signature parsing failed");
                    let program =
                        Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 token parse failed");
                    let module = decode_program(&program).expect("SM4 decode failed");
                    let translated = translate_sm4_module_to_wgsl(
                        &dxbc,
                        black_box(&module),
                        black_box(&signatures),
                    )
                    .expect("WGSL translation failed");
                    black_box(translated.wgsl);
                })
            });
        }
        group.finish();
    }

    // End-to-end bootstrap path (DXBC bytes → WGSL) for MOV/RET-only shaders.
    {
        let fixtures = ["vs_passthrough.dxbc", "ps_passthrough.dxbc"];
        let mut group = c.benchmark_group("dxbc_to_wgsl_end_to_end_bootstrap");
        for name in fixtures {
            let bytes = load_fixture(name);
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, bytes| {
                b.iter(|| {
                    let dxbc = aero_dxbc::DxbcFile::parse(black_box(bytes.as_slice()))
                        .expect("fixture should parse as DXBC");
                    let program =
                        Sm4Program::parse_from_dxbc(&dxbc).expect("SM4 token parse failed");
                    let translated = translate_sm4_to_wgsl_bootstrap(black_box(&program))
                        .expect("bootstrap WGSL translation failed");
                    black_box(translated.wgsl);
                })
            });
        }
        group.finish();
    }
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(
    benches,
    bench_dxbc_parse,
    bench_sm4_decode,
    bench_wgsl_translate,
    bench_end_to_end
);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
