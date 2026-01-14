#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::io::Cursor;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_gpu::cmd::{
    BindGroupId, BufferId, CommandOptimizer, GpuCmd, IndexFormat, LoadOp, Operations, PipelineId,
    RenderPassColorAttachmentDesc, RenderPassDesc, StoreOp, TextureViewId,
};
#[cfg(not(target_arch = "wasm32"))]
use aero_gpu::{parse_cmd_stream, AeroGpuCmd};
#[cfg(not(target_arch = "wasm32"))]
use aero_gpu_trace::{BlobKind, TraceReader, TraceRecord};
#[cfg(not(target_arch = "wasm32"))]
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuIndexFormat, AerogpuPrimitiveTopology, AerogpuShaderStage, AerogpuVertexBufferBinding,
    AEROGPU_CLEAR_COLOR, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
    AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
#[cfg(not(target_arch = "wasm32"))]
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
#[cfg(not(target_arch = "wasm32"))]
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

#[cfg(not(target_arch = "wasm32"))]
const TRACE_CLEAR_RED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_a3a0_clear_red.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_TRIANGLE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_triangle.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_INDEXED_TRIANGLE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_indexed_triangle.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_TEXTURED_RGBA8_SAMPLER_TRIANGLE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_textured_rgba8_sampler_triangle.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_COPY_TEXTURE2D: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_copy_texture2d.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_COPY_BUFFER: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_copy_buffer.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_BLEND_ADD: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_blend_add.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_DEPTH_TEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_depth_test.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_SCISSOR_TEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_scissor_test.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_TEXTURED_BC1_TRIANGLE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_textured_bc1_triangle.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_CLEAR_B8G8R8X8: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_clear_b8g8r8x8.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_CLEAR_B8G8R8A8: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_clear_b8g8r8a8.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_COPY_TEXTURE2D_SUBRECT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_copy_texture2d_subrect.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_CULL_FRONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_cull_front.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_TEXTURE_LD_B5G5R5A1: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_texture_ld_b5g5r5a1.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_TEXTURE_LD_B5G6R5: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_texture_ld_b5g6r5.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_INDEXED_TRIANGLE_BASE_VERTEX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_indexed_triangle_base_vertex.aerogputrace"
));
#[cfg(not(target_arch = "wasm32"))]
const TRACE_PRESENT_EX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/aerogpu_cmd_present_ex.aerogputrace"
));

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
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

#[cfg(not(target_arch = "wasm32"))]
fn extract_cmd_stream_from_trace(trace_bytes: &[u8]) -> Vec<u8> {
    let mut reader =
        TraceReader::open(Cursor::new(trace_bytes)).expect("failed to open .aerogputrace fixture");

    let entry = *reader
        .frame_entries()
        .first()
        .expect("trace fixture contained no frames");

    let records = reader
        .read_records_in_range(entry.start_offset, entry.end_offset)
        .expect("failed to read trace records");

    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut cmd_stream_blob_id: Option<u64> = None;
    let mut cmd_stream_blobs: Vec<(u64, Vec<u8>)> = Vec::new();

    for record in records {
        match record {
            TraceRecord::Packet { bytes } => packets.push(bytes),
            TraceRecord::AerogpuSubmission {
                cmd_stream_blob_id: id,
                ..
            } => cmd_stream_blob_id = Some(id),
            TraceRecord::Blob {
                blob_id,
                kind: BlobKind::AerogpuCmdStream,
                bytes,
            } => cmd_stream_blobs.push((blob_id, bytes)),
            _ => {}
        }
    }

    // Container v1 uses `Packet` records directly, while v2 uses `AerogpuSubmission` + blob IDs.
    // Support both so our benchmarks can reuse fixtures from `aero-gpu-trace`.
    if cmd_stream_blobs.is_empty() {
        return packets
            .into_iter()
            .next()
            .expect("trace fixture had no packets or AerogpuCmdStream blobs");
    }

    let wanted_id = cmd_stream_blob_id.unwrap_or_else(|| {
        cmd_stream_blobs
            .first()
            .expect("trace fixture had no AerogpuCmdStream blobs")
            .0
    });

    cmd_stream_blobs
        .into_iter()
        .find(|(blob_id, _)| *blob_id == wanted_id)
        .unwrap_or_else(|| panic!("missing AerogpuCmdStream blob id {wanted_id}"))
        .1
}

#[cfg(not(target_arch = "wasm32"))]
fn build_synthetic_triangle_stream(draws: u32) -> Vec<u8> {
    let mut w = AerogpuCmdWriter::new();

    // Resource creation (lightweight; included so the stream resembles real submissions).
    w.create_buffer(
        1,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        256, // must be 4-byte aligned
        0,
        0,
    );
    w.create_buffer(
        3,
        AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
        256, // index data (still requires 4-byte alignment)
        0,
        0,
    );
    w.create_texture2d(
        2,
        AEROGPU_RESOURCE_USAGE_RENDER_TARGET,
        AerogpuFormat::R8G8B8A8Unorm as u32,
        64,
        64,
        1,
        1,
        0,
        0,
        0,
    );

    w.set_render_targets(&[2], 0);
    w.set_viewport(0.0, 0.0, 64.0, 64.0, 0.0, 1.0);
    w.clear(AEROGPU_CLEAR_COLOR, [0.0, 0.0, 0.0, 1.0], 1.0, 0);

    let vb = AerogpuVertexBufferBinding {
        buffer: 1,
        stride_bytes: 24,
        offset_bytes: 0,
        reserved0: 0,
    };

    for i in 0..draws {
        // Intentionally repeat state to give the optimizer something to remove.
        w.set_vertex_buffers(0, &[vb]);
        w.set_primitive_topology(AerogpuPrimitiveTopology::TriangleList);
        w.set_index_buffer(3, AerogpuIndexFormat::Uint16, 0);
        // Contiguous index ranges so optional draw coalescing can trigger.
        w.draw_indexed(3, 1, i.saturating_mul(3), 0, 0);
    }

    // Present terminates the submission for most real workloads.
    w.present(0, 0);
    w.finish()
}

#[cfg(not(target_arch = "wasm32"))]
fn build_synthetic_payload_stream() -> Vec<u8> {
    // A deterministic stream that exercises variable-length packets.
    let mut w = AerogpuCmdWriter::new();

    let dxbc = vec![0xA5u8; 16 * 1024];
    w.create_shader_dxbc(0x10, AerogpuShaderStage::Vertex, &dxbc);
    w.create_shader_dxbc(0x11, AerogpuShaderStage::Pixel, &dxbc);
    w.bind_shaders(0x10, 0x11, 0);

    let constants: Vec<f32> = (0..256).map(|i| (i as f32) * 0.001).collect();
    w.set_shader_constants_f(AerogpuShaderStage::Vertex, 0, &constants);

    // Include an UPLOAD_RESOURCE payload as well; parsing should remain safe regardless of size.
    w.create_buffer(
        1,
        AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
        16 * 1024, // must be 4-byte aligned
        0,
        0,
    );
    let upload = vec![0x3Cu8; 16 * 1024];
    w.upload_resource(1, 0, &upload);

    w.present(0, 0);
    w.finish()
}

#[cfg(not(target_arch = "wasm32"))]
fn dummy_render_pass_desc() -> RenderPassDesc {
    RenderPassDesc {
        label: None,
        color_attachments: vec![RenderPassColorAttachmentDesc {
            view: TextureViewId(1),
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Load,
                store: StoreOp::Store,
            },
        }],
        depth_stencil_attachment: None,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn translate_to_internal(cmds: &[AeroGpuCmd<'_>]) -> Vec<GpuCmd> {
    let mut out = Vec::with_capacity(cmds.len().saturating_add(2));
    out.push(GpuCmd::BeginRenderPass(dummy_render_pass_desc()));

    for cmd in cmds {
        match cmd {
            AeroGpuCmd::SetPrimitiveTopology { topology } => {
                // This is not a real translation layer; it exists purely to feed the optimizer a
                // realistic shape of command stream (lots of redundant state sets).
                out.push(GpuCmd::SetPipeline(PipelineId(*topology)));
            }
            AeroGpuCmd::SetVertexBuffers {
                start_slot,
                bindings_bytes,
                ..
            } => {
                // `bindings_bytes` is a raw `aerogpu_vertex_buffer_binding[]` array.
                for (i, binding) in bindings_bytes
                    .chunks_exact(AerogpuVertexBufferBinding::SIZE_BYTES)
                    .enumerate()
                {
                    let buffer = u32::from_le_bytes(binding[0..4].try_into().unwrap());
                    let offset_bytes = u32::from_le_bytes(binding[8..12].try_into().unwrap());
                    out.push(GpuCmd::SetVertexBuffer {
                        slot: start_slot.saturating_add(i as u32),
                        buffer: BufferId(buffer),
                        offset: u64::from(offset_bytes),
                        size: None,
                    });
                }
            }
            AeroGpuCmd::SetIndexBuffer {
                buffer,
                format,
                offset_bytes,
            } => {
                let format = match *format {
                    x if x == AerogpuIndexFormat::Uint16 as u32 => IndexFormat::Uint16,
                    x if x == AerogpuIndexFormat::Uint32 as u32 => IndexFormat::Uint32,
                    _ => IndexFormat::Uint16,
                };
                out.push(GpuCmd::SetIndexBuffer {
                    buffer: BufferId(*buffer),
                    format,
                    offset: u64::from(*offset_bytes),
                    size: None,
                });
            }
            AeroGpuCmd::Draw {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => out.push(GpuCmd::Draw {
                vertex_count: *vertex_count,
                instance_count: *instance_count,
                first_vertex: *first_vertex,
                first_instance: *first_instance,
            }),
            AeroGpuCmd::DrawIndexed {
                index_count,
                instance_count,
                first_index,
                base_vertex,
                first_instance,
            } => out.push(GpuCmd::DrawIndexed {
                index_count: *index_count,
                instance_count: *instance_count,
                first_index: *first_index,
                base_vertex: *base_vertex,
                first_instance: *first_instance,
            }),
            _ => {}
        }
    }

    out.push(GpuCmd::EndRenderPass);
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn build_internal_bind_group_draw_stream(draws: u32) -> Vec<GpuCmd> {
    let mut out = Vec::with_capacity(draws as usize * 4 + 2);
    out.push(GpuCmd::BeginRenderPass(dummy_render_pass_desc()));

    for i in 0..draws {
        // These are intentionally redundant across draws; the optimizer should collapse them.
        out.push(GpuCmd::SetPipeline(PipelineId(1)));
        out.push(GpuCmd::SetBindGroup {
            slot: 0,
            bind_group: BindGroupId(1),
            // Keep this empty so we benchmark optimizer behavior rather than per-iteration
            // allocation overhead from cloning thousands of tiny Vecs.
            dynamic_offsets: Vec::new(),
        });
        out.push(GpuCmd::SetVertexBuffer {
            slot: 0,
            buffer: BufferId(1),
            offset: 0,
            size: None,
        });
        out.push(GpuCmd::Draw {
            vertex_count: 3,
            instance_count: 1,
            first_vertex: i.saturating_mul(3),
            first_instance: 0,
        });
    }

    out.push(GpuCmd::EndRenderPass);
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_cmd_stream_parse(c: &mut Criterion) {
    let clear_red = extract_cmd_stream_from_trace(TRACE_CLEAR_RED);
    let triangle = extract_cmd_stream_from_trace(TRACE_TRIANGLE);
    let indexed_triangle = extract_cmd_stream_from_trace(TRACE_INDEXED_TRIANGLE);
    let textured_rgba8_sampler_triangle =
        extract_cmd_stream_from_trace(TRACE_TEXTURED_RGBA8_SAMPLER_TRIANGLE);
    let copy_texture2d = extract_cmd_stream_from_trace(TRACE_COPY_TEXTURE2D);
    let copy_buffer = extract_cmd_stream_from_trace(TRACE_COPY_BUFFER);
    let blend_add = extract_cmd_stream_from_trace(TRACE_BLEND_ADD);
    let depth_test = extract_cmd_stream_from_trace(TRACE_DEPTH_TEST);
    let scissor_test = extract_cmd_stream_from_trace(TRACE_SCISSOR_TEST);
    let textured_bc1_triangle = extract_cmd_stream_from_trace(TRACE_TEXTURED_BC1_TRIANGLE);
    let clear_b8g8r8x8 = extract_cmd_stream_from_trace(TRACE_CLEAR_B8G8R8X8);
    let clear_b8g8r8a8 = extract_cmd_stream_from_trace(TRACE_CLEAR_B8G8R8A8);
    let copy_texture2d_subrect = extract_cmd_stream_from_trace(TRACE_COPY_TEXTURE2D_SUBRECT);
    let cull_front = extract_cmd_stream_from_trace(TRACE_CULL_FRONT);
    let texture_ld_b5g5r5a1 = extract_cmd_stream_from_trace(TRACE_TEXTURE_LD_B5G5R5A1);
    let texture_ld_b5g6r5 = extract_cmd_stream_from_trace(TRACE_TEXTURE_LD_B5G6R5);
    let indexed_triangle_base_vertex =
        extract_cmd_stream_from_trace(TRACE_INDEXED_TRIANGLE_BASE_VERTEX);
    let present_ex = extract_cmd_stream_from_trace(TRACE_PRESENT_EX);
    let synthetic = build_synthetic_triangle_stream(1024);
    let synthetic_payloads = build_synthetic_payload_stream();

    let mut group = c.benchmark_group("cmd_stream_parse");
    for (name, bytes) in [
        ("fixture_clear_red", clear_red),
        ("fixture_triangle", triangle),
        ("fixture_indexed_triangle", indexed_triangle),
        (
            "fixture_textured_rgba8_sampler_triangle",
            textured_rgba8_sampler_triangle,
        ),
        ("fixture_copy_texture2d", copy_texture2d),
        ("fixture_copy_buffer", copy_buffer),
        ("fixture_blend_add", blend_add),
        ("fixture_depth_test", depth_test),
        ("fixture_scissor_test", scissor_test),
        ("fixture_textured_bc1_triangle", textured_bc1_triangle),
        ("fixture_clear_b8g8r8x8", clear_b8g8r8x8),
        ("fixture_clear_b8g8r8a8", clear_b8g8r8a8),
        ("fixture_copy_texture2d_subrect", copy_texture2d_subrect),
        ("fixture_cull_front", cull_front),
        ("fixture_texture_ld_b5g5r5a1", texture_ld_b5g5r5a1),
        ("fixture_texture_ld_b5g6r5", texture_ld_b5g6r5),
        (
            "fixture_indexed_triangle_base_vertex",
            indexed_triangle_base_vertex,
        ),
        ("fixture_present_ex", present_ex),
        ("synthetic_triangle_1024", synthetic),
        ("synthetic_payloads", synthetic_payloads),
    ] {
        group.throughput(criterion::Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(BenchmarkId::new(name, bytes.len()), &bytes, |b, bytes| {
            b.iter(|| {
                let view = parse_cmd_stream(black_box(bytes)).unwrap();
                black_box(view.cmds.len());
            });
        });
    }
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_cmd_optimize(c: &mut Criterion) {
    // Larger, deterministic stream to get stable optimizer timings.
    let bytes = build_synthetic_triangle_stream(1024);
    let parsed = parse_cmd_stream(&bytes).unwrap();
    let cmds = translate_to_internal(&parsed.cmds);
    let bind_group_cmds = build_internal_bind_group_draw_stream(1024);

    // "Default" optimizer is conservative (no draw coalescing).
    let opt_default = CommandOptimizer::new();
    let mut opt_coalesce = CommandOptimizer::new();
    opt_coalesce.enable_draw_coalescing = true;

    let mut group = c.benchmark_group("cmd_optimize");
    for (name, cmds) in [
        ("from_parsed_synthetic_triangle_1024", cmds),
        ("synthetic_internal_bind_groups_1024", bind_group_cmds),
    ] {
        group.throughput(criterion::Throughput::Elements(cmds.len() as u64));
        group.bench_with_input(BenchmarkId::new(name, "state_only"), &cmds, |b, cmds| {
            b.iter_batched(
                || cmds.clone(),
                |cmds| black_box(opt_default.optimize(cmds)),
                BatchSize::LargeInput,
            );
        });

        group.bench_with_input(
            BenchmarkId::new(name, "with_draw_coalescing"),
            &cmds,
            |b, cmds| {
                b.iter_batched(
                    || cmds.clone(),
                    |cmds| black_box(opt_coalesce.optimize(cmds)),
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_cmd_stream_parse, bench_cmd_optimize
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
