#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_dxbc::{DxbcFile, FourCC};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

// D3D11 SM4 fixtures.
#[cfg(not(target_arch = "wasm32"))]
const VS_PASSTHROUGH_D3D11: &[u8] =
    include_bytes!("../../aero-d3d11/tests/fixtures/vs_passthrough.dxbc");
#[cfg(not(target_arch = "wasm32"))]
const VS_MATRIX_D3D11: &[u8] = include_bytes!("../../aero-d3d11/tests/fixtures/vs_matrix.dxbc");
#[cfg(not(target_arch = "wasm32"))]
const PS_SAMPLE_D3D11: &[u8] = include_bytes!("../../aero-d3d11/tests/fixtures/ps_sample.dxbc");

// D3D9 SM2/SM3 fixtures.
#[cfg(not(target_arch = "wasm32"))]
const VS_3_0_BRANCH_D3D9: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/vs_3_0_branch.dxbc");
#[cfg(not(target_arch = "wasm32"))]
const PS_3_0_MATH_D3D9: &[u8] =
    include_bytes!("../../aero-d3d9/tests/fixtures/dxbc/ps_3_0_math.dxbc");

#[cfg(not(target_arch = "wasm32"))]
const PARSE_FIXTURES: &[(&str, &[u8])] = &[
    ("d3d11/vs_passthrough", VS_PASSTHROUGH_D3D11),
    ("d3d11/vs_matrix", VS_MATRIX_D3D11),
    ("d3d11/ps_sample", PS_SAMPLE_D3D11),
    ("d3d9/vs_3_0_branch", VS_3_0_BRANCH_D3D9),
    ("d3d9/ps_3_0_math", PS_3_0_MATH_D3D9),
];

#[cfg(not(target_arch = "wasm32"))]
fn bench_dxbc_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("dxbc_parse");
    for (name, bytes) in PARSE_FIXTURES {
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("DxbcFile::parse", name),
            bytes,
            |b, bytes| {
                b.iter(|| {
                    let file = DxbcFile::parse(black_box(bytes)).expect("fixture must parse");
                    black_box(file);
                })
            },
        );
    }
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_chunk_lookup(c: &mut Criterion) {
    let dxbc = DxbcFile::parse(VS_MATRIX_D3D11).expect("fixture must parse");

    // Choose a few representative lookups:
    // - `RDEF` is usually near the start of the chunk list.
    // - `SHEX` is usually near the end of the chunk list.
    // - a missing chunk exercises the full scan + failure path.
    let rdef = FourCC(*b"RDEF");
    let shex = FourCC(*b"SHEX");
    let nope = FourCC(*b"NOPE");

    let mut group = c.benchmark_group("dxbc_chunk_lookup");
    group.throughput(Throughput::Bytes(dxbc.bytes().len() as u64));

    group.bench_function("get_chunk/RDEF", |b| {
        b.iter(|| black_box(dxbc.get_chunk(black_box(rdef))))
    });

    group.bench_function("get_chunk/SHEX", |b| {
        b.iter(|| black_box(dxbc.get_chunk(black_box(shex))))
    });

    group.bench_function("get_chunk/miss", |b| {
        b.iter(|| black_box(dxbc.get_chunk(black_box(nope))))
    });

    group.bench_function("find_first_shader_chunk", |b| {
        b.iter(|| black_box(dxbc.find_first_shader_chunk()))
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_dxbc_parse, bench_chunk_lookup);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
