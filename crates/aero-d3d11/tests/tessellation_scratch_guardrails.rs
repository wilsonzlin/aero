mod common;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::expansion_scratch::{
    ExpansionScratchAllocator, ExpansionScratchDescriptor,
};
use aero_d3d11::runtime::tessellation::{
    buffers::TessellationSizingParams, TessellationRuntime, MAX_TESS_FACTOR_SUPPORTED,
};

#[test]
fn tessellation_scratch_oom_error_includes_computed_sizes() {
    pollster::block_on(async {
        let exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let device = exec.device();

        // Force a tiny scratch segment so the tessellation preflight check fails deterministically.
        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor {
            label: Some("tessellation scratch guardrails test"),
            frames_in_flight: 1,
            per_frame_size: 1,
            ..ExpansionScratchDescriptor::default()
        });

        let patch_count_total: u32 = 2;
        // Intentionally exceed the supported range to ensure clamping is reflected in the error.
        let tess_factor_requested: u32 = 128;
        let control_points: u32 = 3;
        // Two output registers => 32-byte stride.
        let ds_output_register_count: u32 = 2;

        let mut rt = TessellationRuntime::default();
        let params = TessellationSizingParams::new(
            patch_count_total,
            control_points,
            tess_factor_requested,
            ds_output_register_count,
        );
        let err = rt
            .alloc_draw_scratch(device, &mut scratch, params)
            .expect_err("expected scratch validation to fail with a tiny per-frame capacity");

        let msg = err.to_string();

        let tess_factor_clamped = MAX_TESS_FACTOR_SUPPORTED;
        let vertices_per_patch = (tess_factor_clamped as u64 + 1).pow(2);
        let indices_per_patch = 6u64 * (tess_factor_clamped as u64).pow(2);
        let ds_stride_bytes = ds_output_register_count as u64 * 16;
        let expanded_vertex_bytes = vertices_per_patch * patch_count_total as u64 * ds_stride_bytes;
        let expanded_index_bytes = indices_per_patch * patch_count_total as u64 * 4;

        assert!(
            msg.contains(&format!("patch_count_total={patch_count_total}")),
            "error message must include patch_count_total; msg={msg}"
        );
        assert!(
            msg.contains(&format!("tess_factor_clamped={tess_factor_clamped}")),
            "error message must include tess_factor_clamped; msg={msg}"
        );
        assert!(
            msg.contains(&format!("expanded_vertex_bytes={expanded_vertex_bytes}")),
            "error message must include computed expanded_vertex_bytes; msg={msg}"
        );
        assert!(
            msg.contains(&format!("expanded_index_bytes={expanded_index_bytes}")),
            "error message must include computed expanded_index_bytes; msg={msg}"
        );
        assert!(
            msg.contains("indirect_args_bytes=20"),
            "error message must include indirect args size; msg={msg}"
        );
    });
}
