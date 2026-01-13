#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::fs;

#[cfg(not(target_arch = "wasm32"))]
use aero_d3d9::{dxbc, shader};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

#[cfg(not(target_arch = "wasm32"))]
fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/dxbc/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_translation_stages(c: &mut Criterion) {
    // Representative real-world-ish fixtures (compiled with fxc / d3dcompiler).
    let vs_dxbc = load_fixture("vs_2_0_simple.dxbc");
    let ps_dxbc = load_fixture("ps_2_0_sample.dxbc");

    // Cache the extracted token streams so we can benchmark container parsing vs token parsing
    // separately.
    let vs_tokens = dxbc::extract_shader_bytecode(&vs_dxbc)
        .expect("fixture should contain SHDR/SHEX")
        .to_vec();
    let ps_tokens = dxbc::extract_shader_bytecode(&ps_dxbc)
        .expect("fixture should contain SHDR/SHEX")
        .to_vec();

    // Pre-build IR for WGSL-only benchmarks (so we don't accidentally time parsing in the
    // WGSL stage).
    let vs_program = shader::parse(&vs_tokens).expect("VS fixture should parse as raw tokens");
    let ps_program = shader::parse(&ps_tokens).expect("PS fixture should parse as raw tokens");
    let vs_ir = shader::to_ir(&vs_program);
    let ps_ir = shader::to_ir(&ps_program);

    let mut group = c.benchmark_group("d3d9_shader_translation");

    // --- parse: DXBC container parsing + SHDR extraction ---
    for (name, dxbc_bytes) in [("vs_2_0_simple", &vs_dxbc), ("ps_2_0_sample", &ps_dxbc)] {
        group.bench_with_input(BenchmarkId::new("parse", name), dxbc_bytes, |b, bytes| {
            b.iter(|| {
                let shdr = dxbc::extract_shader_bytecode(black_box(bytes)).unwrap();
                black_box(shdr.len());
            })
        });
    }

    // --- IR: token stream parsing into the intermediate ShaderProgram ---
    for (name, token_bytes) in [("vs_2_0_simple", &vs_tokens), ("ps_2_0_sample", &ps_tokens)] {
        group.bench_with_input(BenchmarkId::new("IR", name), token_bytes, |b, bytes| {
            b.iter(|| {
                let program = shader::parse(black_box(bytes)).unwrap();
                black_box(program.instructions.len());
            })
        });
    }

    // --- build: build the translator IR (ShaderIr) from parsed programs ---
    for (name, program) in [("vs_2_0_simple", &vs_program), ("ps_2_0_sample", &ps_program)] {
        group.bench_with_input(BenchmarkId::new("build", name), program, |b, program| {
            b.iter(|| {
                let ir = shader::to_ir(black_box(program));
                black_box(ir.ops.len());
            })
        });
    }

    // --- WGSL: generate WGSL for a VS+PS pair ---
    group.bench_function("WGSL/vs+ps", |b| {
        b.iter(|| {
            let vs = shader::generate_wgsl(black_box(&vs_ir)).unwrap();
            let ps = shader::generate_wgsl(black_box(&ps_ir)).unwrap();
            black_box(vs.wgsl.len());
            black_box(ps.wgsl.len());
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_shader_cache_keying(c: &mut Criterion) {
    // Use a representative shader blob for key computation and lookups.
    let ps_dxbc = load_fixture("ps_2_0_sample.dxbc");

    let mut group = c.benchmark_group("d3d9_shader_cache");

    // Key computation alone (blake3 over the raw DXBC bytes).
    group.bench_function("key", |b| {
        b.iter(|| {
            let hash = blake3::hash(black_box(&ps_dxbc));
            black_box(hash);
        })
    });

    // In-memory lookup overhead on the fast path (cache hit).
    let mut cache = shader::ShaderCache::default();
    cache
        .get_or_translate(&ps_dxbc)
        .expect("fixture should translate");
    group.bench_function("lookup_hit", |b| {
        b.iter(|| {
            let lookup = cache.get_or_translate(black_box(&ps_dxbc)).unwrap();
            black_box(lookup.source);
            black_box(lookup.wgsl.wgsl.len());
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_translation_stages, bench_shader_cache_keying);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
