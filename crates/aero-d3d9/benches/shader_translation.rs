#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::fs;

#[cfg(not(target_arch = "wasm32"))]
use aero_d3d9::{dxbc, shader, sm3};
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
    // Representative real-world-ish SM3 fixtures (compiled with fxc / d3dcompiler).
    let vs_dxbc = load_fixture("vs_3_0_branch.dxbc");
    let ps_dxbc = load_fixture("ps_3_0_math.dxbc");

    // Cache the extracted token streams so we can benchmark container parsing vs token parsing
    // separately.
    let vs_tokens = dxbc::extract_shader_bytecode(&vs_dxbc)
        .expect("fixture should contain SHDR/SHEX")
        .to_vec();
    let ps_tokens = dxbc::extract_shader_bytecode(&ps_dxbc)
        .expect("fixture should contain SHDR/SHEX")
        .to_vec();

    // Pre-build SM3 IR for WGSL-only benchmarks (so we don't accidentally time parsing/building in
    // the WGSL stage).
    let vs_decoded = sm3::decode_u8_le_bytes(&vs_tokens).expect("VS fixture should decode");
    let ps_decoded = sm3::decode_u8_le_bytes(&ps_tokens).expect("PS fixture should decode");
    let vs_ir = sm3::build_ir(&vs_decoded).expect("VS fixture should build IR");
    let ps_ir = sm3::build_ir(&ps_decoded).expect("PS fixture should build IR");

    let mut group = c.benchmark_group("d3d9_shader_translation");

    // --- parse: DXBC container parsing + SHDR extraction ---
    for (name, dxbc_bytes) in [("vs_3_0_branch", &vs_dxbc), ("ps_3_0_math", &ps_dxbc)] {
        group.bench_with_input(BenchmarkId::new("parse", name), dxbc_bytes, |b, bytes| {
            b.iter(|| {
                let shdr = dxbc::extract_shader_bytecode(black_box(bytes)).unwrap();
                black_box(shdr.len());
            })
        });
    }

    // --- IR: decode the raw SM2/SM3 token stream ---
    for (name, token_bytes) in [("vs_3_0_branch", &vs_tokens), ("ps_3_0_math", &ps_tokens)] {
        group.bench_with_input(BenchmarkId::new("IR", name), token_bytes, |b, bytes| {
            b.iter(|| {
                let decoded = sm3::decode_u8_le_bytes(black_box(bytes)).unwrap();
                black_box(decoded.instructions.len());
            })
        });
    }

    // --- build: build the SM3 IR from decoded instructions ---
    for (name, decoded) in [("vs_3_0_branch", &vs_decoded), ("ps_3_0_math", &ps_decoded)] {
        group.bench_with_input(BenchmarkId::new("build", name), decoded, |b, decoded| {
            b.iter(|| {
                let ir = sm3::build_ir(black_box(decoded)).unwrap();
                black_box(ir.body.stmts.len());
            })
        });
    }

    // --- WGSL: generate WGSL for a VS+PS pair ---
    group.bench_function("WGSL/vs+ps", |b| {
        b.iter(|| {
            let vs = sm3::generate_wgsl(black_box(&vs_ir)).unwrap();
            let ps = sm3::generate_wgsl(black_box(&ps_ir)).unwrap();
            black_box(vs.wgsl.len());
            black_box(ps.wgsl.len());
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_shader_cache_keying(c: &mut Criterion) {
    // Use a representative shader blob for key computation and lookups.
    let ps_dxbc = load_fixture("ps_3_0_math.dxbc");

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
