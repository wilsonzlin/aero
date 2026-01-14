#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::fs;

#[cfg(not(target_arch = "wasm32"))]
use aero_d3d9::{dxbc, sm3};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

#[cfg(not(target_arch = "wasm32"))]
fn load_fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/dxbc/{name}", env!("CARGO_MANIFEST_DIR"));
    fs::read(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
struct ShaderTranslationFlags {
    d3d9_translator_version: u32,
    half_pixel_center: bool,
    caps_hash: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn compute_persistent_in_memory_key(dxbc: &[u8], flags: &ShaderTranslationFlags) -> [u8; 32] {
    // Mirrors `runtime::shader_cache::compute_in_memory_key` (wasm-only) so we can benchmark the
    // keying overhead on native CPU without involving JS/Web APIs.
    const VERSION: &[u8] = b"aero-d3d9 in-memory shader cache v1";

    let mut hasher = blake3::Hasher::new();
    hasher.update(VERSION);
    hasher.update(dxbc);
    hasher.update(&flags.d3d9_translator_version.to_le_bytes());
    hasher.update(&[flags.half_pixel_center as u8]);
    match &flags.caps_hash {
        Some(caps_hash) => {
            hasher.update(&[1]);
            hasher.update(&(caps_hash.len() as u32).to_le_bytes());
            hasher.update(caps_hash.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    *hasher.finalize().as_bytes()
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

    // Key computation alone for the persistent-cache in-memory key.
    let flags = ShaderTranslationFlags {
        // Keep in sync with `runtime::shader_cache::D3D9_TRANSLATOR_CACHE_VERSION`.
        d3d9_translator_version: 8,
        half_pixel_center: false,
        // Representative stable hash for GPU caps/limits; the real value is typically a short hex
        // string.
        caps_hash: Some("0123456789abcdef0123456789abcdef".to_owned()),
    };
    group.bench_function("key", |b| {
        b.iter(|| {
            let key = compute_persistent_in_memory_key(black_box(&ps_dxbc), black_box(&flags));
            black_box(key);
        })
    });

    // In-memory lookup overhead on the fast path (cache hit).
    let mut cache: HashMap<[u8; 32], ()> = HashMap::new();
    let key = compute_persistent_in_memory_key(&ps_dxbc, &flags);
    cache.insert(key, ());
    group.bench_function("lookup_hit", |b| {
        b.iter(|| {
            let value = cache.get(black_box(&key)).unwrap();
            black_box(value);
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_translation_stages, bench_shader_cache_keying);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
