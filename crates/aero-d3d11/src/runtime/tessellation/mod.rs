//! Tessellation (HS/DS) expansion runtime.
//!
//! WebGPU does not expose hull/domain shader stages. Aero emulates tessellation by running
//! compute-based prepasses that:
//! - execute the relevant stages,
//! - expand patch lists into a flat vertex + index buffer,
//! - and write an indirect draw argument buffer for the final render pass.
//!
//! This module contains allocation plumbing + sizing helpers (CPU-side), along with WGSL templates
//! for the compute passes used by tessellation emulation.
//!
//! Note: low-level tessellator math helpers (currently triangle-domain integer partitioning) live
//! in [`crate::runtime::tessellator`]. This module owns per-draw scratch allocations and (future)
//! compute pipeline state for HS/DS emulation.

pub mod buffers;
pub mod domain_eval;
pub mod hull;
pub mod layout_pass;
pub mod pipeline;
pub mod tessellator;
pub mod tri_domain_integer;
pub mod vs_as_compute;

use super::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchError};

/// Maximum tessellation factor supported by D3D11.
///
/// The runtime uses this value when computing conservative scratch buffer sizes and when deriving
/// per-patch tess levels in the GPU layout pass.
pub const MAX_TESS_FACTOR: u32 = super::tessellator::MAX_TESS_FACTOR;

/// Maximum tessellation factor supported by the WebGPU emulation path.
///
/// D3D11's fixed-function tessellator supports factors up to [`MAX_TESS_FACTOR`] (64), but the
/// compute-based expansion path needs to stay within reasonable WebGPU limits (scratch memory,
/// dispatch sizes, etc). HS tess factors are clamped to this value.
pub const MAX_TESS_FACTOR_SUPPORTED: u32 = 16;

fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

fn lcm_u64(a: u64, b: u64) -> u64 {
    debug_assert!(a > 0);
    debug_assert!(b > 0);
    let g = gcd_u64(a, b);
    (a / g).saturating_mul(b)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment > 0);
    let add = alignment - 1;
    match value.checked_add(add) {
        Some(v) => v / alignment * alignment,
        None => u64::MAX / alignment * alignment,
    }
}

fn scratch_storage_alignment(device: &wgpu::Device) -> u64 {
    (device.limits().min_storage_buffer_offset_alignment as u64).max(1)
}

fn scratch_alloc_alignment(requested: u64, storage_alignment: u64) -> u64 {
    lcm_u64(
        requested.max(1),
        lcm_u64(wgpu::COPY_BUFFER_ALIGNMENT, storage_alignment),
    )
}

fn scratch_add_alloc(
    offset: u64,
    size: u64,
    requested_alignment: u64,
    storage_alignment: u64,
) -> Result<u64, buffers::TessellationSizingError> {
    let alignment = scratch_alloc_alignment(requested_alignment, storage_alignment);
    let offset = align_up(offset, alignment);
    let size = align_up(size, wgpu::COPY_BUFFER_ALIGNMENT);
    offset
        .checked_add(size)
        .ok_or(buffers::TessellationSizingError::Overflow(
            "scratch allocation offset overflow",
        ))
}

fn compute_worst_case_scratch_usage_bytes(
    device: &wgpu::Device,
    sizes: &buffers::TessellationDrawScratchSizes,
    debug_counters_bytes: u64,
) -> Result<u64, buffers::TessellationSizingError> {
    let storage_alignment = scratch_storage_alignment(device);
    let mut offset = 0u64;

    // VS/HS stage outputs and patch constants are vertex-like storage payloads.
    for bytes in [
        sizes.vs_out_bytes,
        sizes.hs_out_bytes,
        sizes.hs_patch_constants_bytes,
        sizes.hs_tess_factors_bytes,
        sizes.tess_metadata_bytes,
        sizes.expanded_vertex_bytes,
    ] {
        offset = scratch_add_alloc(offset, bytes, 16, storage_alignment)?;
    }

    // Expanded indices + indirect args are u32-based.
    offset = scratch_add_alloc(offset, sizes.expanded_index_bytes, 4, storage_alignment)?;
    offset = scratch_add_alloc(offset, sizes.indirect_args_bytes, 4, storage_alignment)?;

    if debug_counters_bytes != 0 {
        offset = scratch_add_alloc(offset, debug_counters_bytes, 16, storage_alignment)?;
    }

    Ok(offset)
}

/// Uniform payload for the GPU tessellation *layout pass*.
///
/// Layout matches the WGSL `LayoutParams` struct in [`layout_pass`], and is padded to 16 bytes so
/// it can be bound as a WebGPU uniform buffer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TessellationLayoutParams {
    /// Number of patches to process.
    pub patch_count: u32,
    /// Capacity of the downstream expanded-vertex buffer, in vertices.
    pub max_vertices: u32,
    /// Capacity of the downstream expanded-index buffer, in indices.
    pub max_indices: u32,
    pub _pad0: u32,
}

impl TessellationLayoutParams {
    pub const fn layout() -> (u64, u64) {
        (
            core::mem::size_of::<Self>() as u64,
            core::mem::align_of::<Self>() as u64,
        )
    }

    /// Serializes this struct into little-endian bytes suitable for `Queue::write_buffer`.
    pub fn to_le_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.patch_count.to_le_bytes());
        out[4..8].copy_from_slice(&self.max_vertices.to_le_bytes());
        out[8..12].copy_from_slice(&self.max_indices.to_le_bytes());
        out[12..16].copy_from_slice(&self._pad0.to_le_bytes());
        out
    }
}

/// Per-patch metadata produced by the GPU tessellation *layout pass*.
///
/// This is the layout written by [`layout_pass::wgsl_tessellation_layout_pass`]. Offsets are in
/// elements (vertices/indices), not bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TessellationLayoutPatchMeta {
    pub tess_level: u32,
    pub vertex_base: u32,
    pub index_base: u32,
    pub vertex_count: u32,
    pub index_count: u32,
}

impl TessellationLayoutPatchMeta {
    pub const fn layout() -> (u64, u64) {
        (
            core::mem::size_of::<Self>() as u64,
            core::mem::align_of::<Self>() as u64,
        )
    }
}

// Compile-time layout validation (matches WGSL).
const _: [(); 16] = [(); core::mem::size_of::<TessellationLayoutParams>()];
const _: [(); 4] = [(); core::mem::align_of::<TessellationLayoutParams>()];
const _: [(); 20] = [(); core::mem::size_of::<TessellationLayoutPatchMeta>()];
const _: [(); 4] = [(); core::mem::align_of::<TessellationLayoutPatchMeta>()];

#[derive(Debug, Default)]
pub struct TessellationRuntime {
    pipelines: pipeline::TessellationPipelines,
}

/// Structured scratch OOM error for tessellation expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TessellationScratchOomError {
    pub patch_count_total: u32,
    pub tess_factor_clamped: u32,
    pub scratch_per_frame_capacity: u64,
    pub device_max_buffer_size: u64,
    pub sizes: buffers::TessellationDrawScratchSizes,
    pub debug_counters_bytes: u64,
    pub total_scratch_bytes: u64,
}

impl core::fmt::Display for TessellationScratchOomError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "tessellation scratch OOM: patch_count_total={} tess_factor_clamped={} \
vs_out_bytes={} hs_out_bytes={} hs_patch_constants_bytes={} hs_tess_factors_bytes={} tess_metadata_bytes={} \
expanded_vertex_bytes={} expanded_index_bytes={} indirect_args_bytes={} debug_counters_bytes={} \
total_scratch_bytes={} scratch_per_frame_capacity={} device_max_buffer_size={}",
            self.patch_count_total,
            self.tess_factor_clamped,
            self.sizes.vs_out_bytes,
            self.sizes.hs_out_bytes,
            self.sizes.hs_patch_constants_bytes,
            self.sizes.hs_tess_factors_bytes,
            self.sizes.tess_metadata_bytes,
            self.sizes.expanded_vertex_bytes,
            self.sizes.expanded_index_bytes,
            self.sizes.indirect_args_bytes,
            self.debug_counters_bytes,
            self.total_scratch_bytes,
            self.scratch_per_frame_capacity,
            self.device_max_buffer_size
        )
    }
}

impl std::error::Error for TessellationScratchOomError {}

#[derive(Debug)]
pub enum TessellationRuntimeError {
    Sizing(buffers::TessellationSizingError),
    Scratch(ExpansionScratchError),
    ScratchOom(Box<TessellationScratchOomError>),
}

impl core::fmt::Display for TessellationRuntimeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TessellationRuntimeError::Sizing(e) => write!(f, "tessellation sizing error: {e}"),
            TessellationRuntimeError::Scratch(e) => write!(f, "tessellation scratch error: {e}"),
            TessellationRuntimeError::ScratchOom(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TessellationRuntimeError {}

impl TessellationRuntime {
    pub fn reset(&mut self) {
        self.pipelines.reset();
    }

    pub(crate) fn pipelines_mut(&mut self) -> &mut pipeline::TessellationPipelines {
        &mut self.pipelines
    }

    /// Allocate per-draw scratch buffers for tessellation expansion.
    ///
    /// The returned allocations are all subranges of the shared [`ExpansionScratchAllocator`]
    /// backing buffer.
    pub fn alloc_draw_scratch(
        &mut self,
        device: &wgpu::Device,
        scratch: &mut ExpansionScratchAllocator,
        params: buffers::TessellationSizingParams,
    ) -> Result<buffers::TessellationDrawScratch, TessellationRuntimeError> {
        let tess_factor_clamped = params.max_tess_factor.clamp(1, MAX_TESS_FACTOR_SUPPORTED);
        let params = buffers::TessellationSizingParams {
            max_tess_factor: tess_factor_clamped,
            ..params
        };

        let sizes = buffers::TessellationDrawScratchSizes::new(params)
            .map_err(TessellationRuntimeError::Sizing)?;

        // Preflight scratch usage before performing any allocations. This prevents the scratch
        // allocator from silently growing to enormous buffers (or failing deep in the dispatch
        // path) and produces an actionable error with computed sizes.
        scratch
            .init(device)
            .map_err(TessellationRuntimeError::Scratch)?;
        let scratch_per_frame_capacity =
            scratch
                .per_frame_capacity()
                .ok_or(TessellationRuntimeError::Scratch(
                    ExpansionScratchError::InvalidDescriptor(
                        "scratch allocator did not report per-frame capacity after init",
                    ),
                ))?;

        let device_max_buffer_size = device.limits().max_buffer_size;
        // The layout pass always writes a debug flag (`u32`). When debug counters are enabled, the
        // struct expands to 16 bytes.
        let debug_counters_bytes: u64 = if cfg!(any(test, feature = "tessellation_debug_counters"))
        {
            16
        } else {
            // The layout pass always writes at least the `DebugOut.flag` field.
            4
        };
        let total_scratch_bytes =
            compute_worst_case_scratch_usage_bytes(device, &sizes, debug_counters_bytes)
                .map_err(TessellationRuntimeError::Sizing)?;

        let exceeds_device_limit = sizes.vs_out_bytes > device_max_buffer_size
            || sizes.hs_out_bytes > device_max_buffer_size
            || sizes.hs_patch_constants_bytes > device_max_buffer_size
            || sizes.hs_tess_factors_bytes > device_max_buffer_size
            || sizes.tess_metadata_bytes > device_max_buffer_size
            || sizes.expanded_vertex_bytes > device_max_buffer_size
            || sizes.expanded_index_bytes > device_max_buffer_size
            || sizes.indirect_args_bytes > device_max_buffer_size
            || debug_counters_bytes > device_max_buffer_size
            || total_scratch_bytes > device_max_buffer_size;
        let exceeds_scratch_capacity = total_scratch_bytes > scratch_per_frame_capacity;

        if exceeds_device_limit || exceeds_scratch_capacity {
            return Err(TessellationRuntimeError::ScratchOom(Box::new(
                TessellationScratchOomError {
                    patch_count_total: params.patch_count_total,
                    tess_factor_clamped,
                    scratch_per_frame_capacity,
                    device_max_buffer_size,
                    sizes,
                    debug_counters_bytes,
                    total_scratch_bytes,
                },
            )));
        }

        // Intermediate stage outputs are modelled as storage buffers, but allocating them via the
        // "vertex output" path keeps alignment consistent with other stage-emulation scratch.
        let vs_out = scratch
            .alloc_vertex_output(device, sizes.vs_out_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let hs_out = scratch
            .alloc_vertex_output(device, sizes.hs_out_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let hs_patch_constants = scratch
            .alloc_vertex_output(device, sizes.hs_patch_constants_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let hs_tess_factors = scratch
            .alloc_vertex_output(device, sizes.hs_tess_factors_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;

        let tess_metadata = scratch
            .alloc_metadata(device, sizes.tess_metadata_bytes, 16)
            .map_err(TessellationRuntimeError::Scratch)?;

        let expanded_vertices = scratch
            .alloc_vertex_output(device, sizes.expanded_vertex_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;
        let expanded_indices = scratch
            .alloc_index_output(device, sizes.expanded_index_bytes)
            .map_err(TessellationRuntimeError::Scratch)?;

        let indirect_args = scratch
            .alloc_indirect_draw_indexed(device)
            .map_err(TessellationRuntimeError::Scratch)?;

        Ok(buffers::TessellationDrawScratch {
            vs_out,
            hs_out,
            hs_patch_constants,
            hs_tess_factors,
            tess_metadata,
            expanded_vertices,
            expanded_indices,
            indirect_args,
            sizes,
        })
    }
}

use crate::binding_model::BINDING_BASE_INTERNAL;

/// Expansion-internal binding for the VS output register file (`vs_out_regs`).
///
/// This buffer is written by the compute-variant VS and consumed by HS.
///
/// Note: this lives in the reserved internal binding range (`BINDING_BASE_INTERNAL..`) to avoid
/// colliding with D3D register-space bindings (`b#`/`t#`/`s#`/`u#`) when HS resources are also mapped
/// to `@group(3)`.
pub const BINDING_VS_OUT_REGS: u32 = BINDING_BASE_INTERNAL + 80;

/// Expansion-internal binding for the HS output register file (`hs_out_regs`).
///
/// This buffer is written by the HS control-point phase and consumed by later tessellation stages.
pub const BINDING_HS_OUT_REGS: u32 = BINDING_BASE_INTERNAL + 81;

/// Expansion-internal binding for HS patch-constant outputs (`hs_patch_constants`).
pub const BINDING_HS_PATCH_CONSTANTS: u32 = BINDING_BASE_INTERNAL + 82;

/// Expansion-internal binding for compact tess factors (`hs_tess_factors`).
///
/// The layout pass consumes this buffer to derive per-patch tessellation levels.
pub const BINDING_HS_TESS_FACTORS: u32 = BINDING_BASE_INTERNAL + 83;

/// Number of `vec4<f32>` elements stored per patch in [`BINDING_HS_TESS_FACTORS`].
pub const HS_TESS_FACTOR_VEC4S_PER_PATCH: u32 = 1;

#[cfg(test)]
mod tests {
    use super::domain_eval::{
        build_triangle_domain_eval_wgsl, chunk_count_for_vertex_count, DomainEvalPipeline,
        DOMAIN_EVAL_WORKGROUP_SIZE_Y,
    };
    use super::*;
    use crate::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
    use std::sync::Arc;

    fn require_webgpu() -> bool {
        let Ok(raw) = std::env::var("AERO_REQUIRE_WEBGPU") else {
            return false;
        };
        let v = raw.trim();
        v == "1"
            || v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
    }

    fn skip_or_panic(test_name: &str, reason: &str) {
        if require_webgpu() {
            panic!("AERO_REQUIRE_WEBGPU is enabled but {test_name} cannot run: {reason}");
        }
        eprintln!("skipping {test_name}: {reason}");
    }

    async fn new_test_device() -> anyhow::Result<(wgpu::Device, wgpu::Queue)> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);
            if needs_runtime_dir {
                let dir = std::env::temp_dir()
                    .join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
                std::env::set_var("XDG_RUNTIME_DIR", &dir);
            }
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // Prefer GL on Linux CI to avoid crashes in some Vulkan software adapters.
            backends: if cfg!(target_os = "linux") {
                wgpu::Backends::GL
            } else {
                wgpu::Backends::PRIMARY
            },
            ..Default::default()
        });

        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: true,
            })
            .await
        {
            Some(adapter) => Some(adapter),
            None => {
                instance
                    .request_adapter(&wgpu::RequestAdapterOptions {
                        power_preference: wgpu::PowerPreference::LowPower,
                        compatible_surface: None,
                        force_fallback_adapter: false,
                    })
                    .await
            }
        }
        .ok_or_else(|| anyhow::anyhow!("wgpu: no suitable adapter found"))?;

        let supports_compute = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS);
        if !supports_compute {
            anyhow::bail!("wgpu adapter does not support compute shaders");
        }

        let requested_features = super::super::negotiated_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 tessellation test device"),
                    required_features: requested_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("wgpu: request_device failed: {e:?}"))?;

        Ok((device, queue))
    }

    async fn read_buffer(
        device: &wgpu::Device,
        buffer: &wgpu::Buffer,
        size: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let slice = buffer.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            sender.send(res).ok();
        });
        device.poll(wgpu::Maintain::Wait);
        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow::anyhow!("wgpu: map_async dropped"))?
            .map_err(|e| anyhow::anyhow!("wgpu: map_async failed: {e:?}"))?;

        let mapped = slice.get_mapped_range();
        let out = mapped
            .get(..size)
            .ok_or_else(|| anyhow::anyhow!("mapped buffer too small"))?
            .to_vec();
        drop(mapped);
        buffer.unmap();
        Ok(out)
    }

    fn read_u32_le(bytes: &[u8]) -> u32 {
        let mut arr = [0u8; 4];
        arr.copy_from_slice(&bytes[..4]);
        u32::from_le_bytes(arr)
    }

    fn read_f32_le(bytes: &[u8]) -> f32 {
        let mut arr = [0u8; 4];
        arr.copy_from_slice(&bytes[..4]);
        f32::from_le_bytes(arr)
    }

    fn read_vec4_f32_le(bytes: &[u8]) -> [f32; 4] {
        [
            read_f32_le(&bytes[0..4]),
            read_f32_le(&bytes[4..8]),
            read_f32_le(&bytes[8..12]),
            read_f32_le(&bytes[12..16]),
        ]
    }

    #[test]
    fn alloc_draw_scratch_allocates_expected_sizes() {
        pollster::block_on(async {
            let exec = match AerogpuD3d11Executor::new_for_tests().await {
                Ok(exec) => exec,
                Err(e) => {
                    eprintln!(
                        "skipping tessellation scratch allocation test: wgpu unavailable ({e:#})"
                    );
                    return;
                }
            };
            if !exec.caps().supports_compute || !exec.caps().supports_indirect_execution {
                eprintln!(
                    "skipping tessellation scratch allocation test: backend lacks compute/indirect execution"
                );
                return;
            }

            let mut scratch = ExpansionScratchAllocator::new(Default::default());
            let mut rt = TessellationRuntime::default();
            let params = buffers::TessellationSizingParams::new(2, 3, MAX_TESS_FACTOR, 2);
            let draw = rt
                .alloc_draw_scratch(exec.device(), &mut scratch, params)
                .expect("alloc_draw_scratch should succeed");

            assert_eq!(draw.vs_out.size, draw.sizes.vs_out_bytes);
            assert_eq!(draw.hs_out.size, draw.sizes.hs_out_bytes);
            assert_eq!(
                draw.hs_patch_constants.size,
                draw.sizes.hs_patch_constants_bytes
            );
            assert_eq!(draw.hs_tess_factors.size, draw.sizes.hs_tess_factors_bytes);
            assert_eq!(draw.tess_metadata.size, draw.sizes.tess_metadata_bytes);
            assert_eq!(
                draw.expanded_vertices.size,
                draw.sizes.expanded_vertex_bytes
            );
            assert_eq!(draw.expanded_indices.size, draw.sizes.expanded_index_bytes);
            assert_eq!(draw.indirect_args.size, draw.sizes.indirect_args_bytes);

            // All allocations should share the same backing buffer when capacity is sufficient.
            assert!(Arc::ptr_eq(&draw.vs_out.buffer, &draw.hs_out.buffer));
            assert!(Arc::ptr_eq(
                &draw.vs_out.buffer,
                &draw.expanded_vertices.buffer
            ));
        });
    }

    #[test]
    fn triangle_domain_integer_partitioning_domain_eval_writes_expanded_vertices() {
        pollster::block_on(async {
            let (device, queue) = match new_test_device().await {
                Ok(v) => v,
                Err(e) => {
                    skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                    return;
                }
            };

            let patch_count = 2u32;
            let tess_level = 2u32;
            let expected_vertex_count_per_patch = tessellator::tri_vertex_count(tess_level);
            let expected_index_count_per_patch = tessellator::tri_index_count(tess_level);
            let expected_vertex_count_total = patch_count * expected_vertex_count_per_patch;
            let expected_index_count_total = patch_count * expected_index_count_per_patch;
            let out_reg_count = 2u32;

            assert_eq!(
                chunk_count_for_vertex_count(expected_vertex_count_per_patch),
                1,
                "expected vertex count should fit within one y-chunk of {DOMAIN_EVAL_WORKGROUP_SIZE_Y}"
            );

            // Input control points for two triangle patches.
            // Patch 1 is offset so we can assert `patch_id` / `vertex_base` handling.
            let patch_control_points: [[[f32; 4]; 3]; 2] = [
                [
                    [0.0, 0.0, 0.0, 1.0],
                    [1.0, 0.0, 0.0, 1.0],
                    [0.0, 1.0, 0.0, 1.0],
                ],
                [
                    [10.0, 0.0, 0.0, 1.0],
                    [11.0, 0.0, 0.0, 1.0],
                    [10.0, 1.0, 0.0, 1.0],
                ],
            ];

            let mut cp_bytes = Vec::with_capacity(patch_count as usize * 3 * 16);
            for patch in &patch_control_points {
                for cp in patch {
                    for &f in cp {
                        cp_bytes.extend_from_slice(&f.to_le_bytes());
                    }
                }
            }

            let in_control_points = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test in control points"),
                size: cp_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&in_control_points, 0, &cp_bytes);

            let hs_control_points = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test hs control points"),
                size: cp_bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            let hs_patch_constants = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test hs patch constants"),
                size: patch_count as u64 * 16, // one vec4 per patch
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            let hs_params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test hs params"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut hs_params_bytes = [0u8; 16];
            hs_params_bytes[0..4].copy_from_slice(&tess_level.to_le_bytes());
            queue.write_buffer(&hs_params, 0, &hs_params_bytes);

            // HS: copy control points + emit tess level into patch constants.
            let hs_wgsl = r#"
struct HsParams {
    tess_level: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> in_control_points: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> out_control_points: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> out_patch_constants: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: HsParams;

@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let idx = id.x;
    out_control_points[idx] = in_control_points[idx];

    // One patch with 3 control points.
    if ((idx % 3u) == 0u) {
        let patch_id = idx / 3u;
        out_patch_constants[patch_id] = vec4<f32>(f32(params.tess_level));
    }
}
"#;

            let hs_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("tess test hs module"),
                source: wgpu::ShaderSource::Wgsl(hs_wgsl.into()),
            });

            let hs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tess test hs bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

            let hs_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tess test hs bg"),
                layout: &hs_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: in_control_points.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: hs_control_points.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: hs_patch_constants.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: hs_params.as_entire_binding(),
                    },
                ],
            });

            let hs_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("tess test hs pipeline layout"),
                bind_group_layouts: &[&hs_bgl],
                push_constant_ranges: &[],
            });

            let hs_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("tess test hs pipeline"),
                layout: Some(&hs_pl),
                module: &hs_module,
                entry_point: "cs_main",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            });

            // Layout pass: compute per-patch metadata (tess_level, counts, prefix sums).
            let patch_meta_size = core::mem::size_of::<TessellationLayoutPatchMeta>() as u64;
            let patch_meta = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test patch meta"),
                size: patch_meta_size * patch_count as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            let indirect_args_size = core::mem::size_of::<
                crate::runtime::indirect_args::DrawIndexedIndirectArgs,
            >() as u64;
            let indirect_args = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test indirect args"),
                size: indirect_args_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            // `layout_pass::wgsl_tessellation_layout_pass` enables debug counters in tests, which
            // expands `DebugOut` to 16 bytes (flag + counters + padding).
            let layout_debug_size: u64 = 16;
            let layout_debug = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test layout debug"),
                size: layout_debug_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            let layout_params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test layout params"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let layout_params_value = TessellationLayoutParams {
                patch_count,
                max_vertices: expected_vertex_count_total,
                max_indices: expected_index_count_total,
                _pad0: 0,
            };
            queue.write_buffer(&layout_params, 0, &layout_params_value.to_le_bytes());

            let layout_wgsl = super::layout_pass::wgsl_tessellation_layout_pass(0, 0, 1, 2, 3, 4);
            let layout_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("tess test layout module"),
                source: wgpu::ShaderSource::Wgsl(layout_wgsl.into()),
            });

            let layout_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tess test layout bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(16),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            // `layout_pass::wgsl_tessellation_layout_pass` binds `hs_tess_factors`
                            // as `read_write` even though it only reads the data (see WGSL doc
                            // comment), so mirror that here.
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

            let layout_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tess test layout bg"),
                layout: &layout_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: layout_params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: hs_patch_constants.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: patch_meta.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: indirect_args.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: layout_debug.as_entire_binding(),
                    },
                ],
            });

            let layout_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("tess test layout pipeline layout"),
                bind_group_layouts: &[&layout_bgl],
                push_constant_ranges: &[],
            });

            let layout_pipeline =
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("tess test layout pipeline"),
                    layout: Some(&layout_pl),
                    module: &layout_module,
                    entry_point: "cs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                });

            // DS eval pass: consumes meta + HS outputs, writes expanded vertex buffer.
            let expanded_size = expected_vertex_count_total as u64 * out_reg_count as u64 * 16;
            let expanded_vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test expanded vertices"),
                size: expanded_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });

            let ds_params = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test ds params"),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&ds_params, 0, &[0u8; 16]);

            let user_ds_wgsl = r#"
@group(3) @binding(0) var<uniform> ds_offset: vec4<f32>;

fn ds_eval(patch_id: u32, domain: vec3<f32>, _local_vertex: u32) -> AeroDsOut {
    var out: AeroDsOut;
    let base = patch_id * AERO_HS_CONTROL_POINTS_PER_PATCH;
    let cp0 = aero_hs_control_points[base + 0u];
    let cp1 = aero_hs_control_points[base + 1u];
    let cp2 = aero_hs_control_points[base + 2u];

    let pos = cp0 * domain.x + cp1 * domain.y + cp2 * domain.z + ds_offset;
    out.o0 = pos;
    out.o1 = pos * 2.0;
    return out;
}
"#;

            let ds_wgsl = build_triangle_domain_eval_wgsl(user_ds_wgsl, out_reg_count);
            let ds_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("tess test ds module"),
                source: wgpu::ShaderSource::Wgsl(ds_wgsl.into()),
            });

            let ds_domain_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("tess test ds domain bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

            let ds_domain_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("tess test ds domain bg"),
                layout: &ds_domain_bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ds_params.as_entire_binding(),
                }],
            });

            let ds_pipeline = DomainEvalPipeline::new(&device, &ds_module, &ds_domain_bgl);
            let ds_internal_bg = ds_pipeline.create_internal_bind_group(
                &device,
                wgpu::BufferBinding {
                    buffer: &patch_meta,
                    offset: 0,
                    size: None,
                },
                wgpu::BufferBinding {
                    buffer: &hs_control_points,
                    offset: 0,
                    size: None,
                },
                wgpu::BufferBinding {
                    buffer: &hs_patch_constants,
                    offset: 0,
                    size: None,
                },
                wgpu::BufferBinding {
                    buffer: &expanded_vertices,
                    offset: 0,
                    size: None,
                },
            );

            // Encode passes + readback copies.
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tess test encoder"),
            });

            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("tess test hs pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&hs_pipeline);
                pass.set_bind_group(0, &hs_bg, &[]);
                pass.dispatch_workgroups(patch_count * 3, 1, 1);
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("tess test layout pass"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&layout_pipeline);
                pass.set_bind_group(0, &layout_bg, &[]);
                // The layout pass performs its own serial loop over patches; only one invocation is
                // required (it ignores `gid.x != 0`).
                pass.dispatch_workgroups(1, 1, 1);
            }
            ds_pipeline.dispatch(
                &mut encoder,
                &ds_internal_bg,
                &ds_domain_bg,
                patch_count,
                chunk_count_for_vertex_count(expected_vertex_count_per_patch),
            );

            let staging_patch_meta = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test staging patch meta"),
                size: patch_meta_size * patch_count as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let staging_indirect = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test staging indirect args"),
                size: indirect_args_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let staging_debug = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test staging layout debug"),
                size: layout_debug_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let staging_vertices = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("tess test staging vertices"),
                size: expanded_size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            encoder.copy_buffer_to_buffer(
                &patch_meta,
                0,
                &staging_patch_meta,
                0,
                patch_meta_size * patch_count as u64,
            );
            encoder.copy_buffer_to_buffer(
                &indirect_args,
                0,
                &staging_indirect,
                0,
                indirect_args_size,
            );
            encoder.copy_buffer_to_buffer(&layout_debug, 0, &staging_debug, 0, layout_debug_size);
            encoder.copy_buffer_to_buffer(
                &expanded_vertices,
                0,
                &staging_vertices,
                0,
                expanded_size,
            );

            queue.submit([encoder.finish()]);

            let meta_bytes = read_buffer(
                &device,
                &staging_patch_meta,
                (patch_meta_size * patch_count as u64) as usize,
            )
            .await
            .unwrap();

            let meta0 = 0usize;
            let meta1 = patch_meta_size as usize;
            let tess0 = read_u32_le(&meta_bytes[meta0..meta0 + 4]);
            let vb0 = read_u32_le(&meta_bytes[meta0 + 4..meta0 + 8]);
            let ib0 = read_u32_le(&meta_bytes[meta0 + 8..meta0 + 12]);
            let vc0 = read_u32_le(&meta_bytes[meta0 + 12..meta0 + 16]);
            let ic0 = read_u32_le(&meta_bytes[meta0 + 16..meta0 + 20]);

            let tess1 = read_u32_le(&meta_bytes[meta1..meta1 + 4]);
            let vb1 = read_u32_le(&meta_bytes[meta1 + 4..meta1 + 8]);
            let ib1 = read_u32_le(&meta_bytes[meta1 + 8..meta1 + 12]);
            let vc1 = read_u32_le(&meta_bytes[meta1 + 12..meta1 + 16]);
            let ic1 = read_u32_le(&meta_bytes[meta1 + 16..meta1 + 20]);

            assert_eq!(tess0, tess_level);
            assert_eq!(vc0, expected_vertex_count_per_patch);
            assert_eq!(vb0, 0);
            assert_eq!(ic0, expected_index_count_per_patch);
            assert_eq!(ib0, 0);

            assert_eq!(tess1, tess_level);
            assert_eq!(vc1, expected_vertex_count_per_patch);
            assert_eq!(vb1, expected_vertex_count_per_patch);
            assert_eq!(ic1, expected_index_count_per_patch);
            assert_eq!(ib1, expected_index_count_per_patch);

            let vertex_count_total = vb1 + vc1;
            assert_eq!(
                vertex_count_total, expected_vertex_count_total,
                "vertex_count_total should match triangle integer-partitioning formula"
            );

            let debug_bytes = read_buffer(&device, &staging_debug, layout_debug_size as usize)
                .await
                .unwrap();
            assert_eq!(
                read_u32_le(&debug_bytes[0..4]),
                0,
                "layout pass should not clamp tessellation for this test"
            );

            let indirect_bytes =
                read_buffer(&device, &staging_indirect, indirect_args_size as usize)
                    .await
                    .unwrap();
            assert_eq!(
                read_u32_le(&indirect_bytes[0..4]),
                expected_index_count_total,
                "layout pass indirect args index_count mismatch"
            );

            let vbytes = read_buffer(&device, &staging_vertices, expanded_size as usize)
                .await
                .unwrap();

            // Validate a few known vertices. `out_reg_count=2`, so each vertex is 2x vec4.
            let stride = out_reg_count as usize * 16;
            for (patch_id, base_vertex) in [(0u32, vb0), (1u32, vb1)] {
                for &local in &[0u32, 1u32, expected_vertex_count_per_patch - 1] {
                    let domain = tessellator::tri_vertex_domain_location(tess_level, local);
                    let cps = patch_control_points[patch_id as usize];
                    let cp0 = cps[0];
                    let cp1 = cps[1];
                    let cp2 = cps[2];
                    let expected_pos = [
                        cp0[0] * domain[0] + cp1[0] * domain[1] + cp2[0] * domain[2],
                        cp0[1] * domain[0] + cp1[1] * domain[1] + cp2[1] * domain[2],
                        cp0[2] * domain[0] + cp1[2] * domain[1] + cp2[2] * domain[2],
                        cp0[3] * domain[0] + cp1[3] * domain[1] + cp2[3] * domain[2],
                    ];

                    let global = base_vertex + local;
                    let base = global as usize * stride;
                    let got_o0 = read_vec4_f32_le(&vbytes[base..base + 16]);
                    let got_o1 = read_vec4_f32_le(&vbytes[base + 16..base + 32]);

                    for c in 0..4 {
                        assert!(
                            (got_o0[c] - expected_pos[c]).abs() < 1e-6,
                            "patch {patch_id} vertex {local} o0[{c}] mismatch: got {} expected {}",
                            got_o0[c],
                            expected_pos[c]
                        );
                        assert!(
                            (got_o1[c] - expected_pos[c] * 2.0).abs() < 1e-6,
                            "patch {patch_id} vertex {local} o1[{c}] mismatch: got {} expected {}",
                            got_o1[c],
                            expected_pos[c] * 2.0
                        );
                    }
                }
            }
        });
    }
}
