use std::collections::HashMap;

use aero_gpu::guest_memory::GuestMemory;
use aero_gpu::{
    decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8,
    expand_b5g5r5a1_unorm_to_rgba8, expand_b5g6r5_unorm_to_rgba8,
};
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuHandle, AerogpuShaderStage, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_STORAGE, AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
};
use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;
use aero_protocol::aerogpu::aerogpu_ring::AerogpuAllocEntry;
use anyhow::{anyhow, bail, Context, Result};

use crate::input_layout::{
    fnv1a_32, map_layout_to_shader_locations_compact, InputLayoutBinding, InputLayoutDesc,
    MappedInputLayout, VsInputSignatureElement,
};
use crate::wgsl_bootstrap::translate_sm4_to_wgsl_bootstrap;
use crate::{parse_signatures, translate_sm4_module_to_wgsl, DxbcFile, ShaderStage, Sm4Program};

fn device_supports_storage_buffers(device: &wgpu::Device) -> bool {
    let limits = device.limits();
    limits.max_storage_buffers_per_shader_stage > 0
        && limits.max_compute_workgroups_per_dimension > 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackingInfo {
    pub alloc_id: u32,
    pub alloc_offset_bytes: u32,
}

#[derive(Debug)]
pub struct BufferResource {
    pub buffer: wgpu::Buffer,
    pub size: u64,
    pub usage_flags: u32,
    pub backing: Option<BackingInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinearTextureFormat {
    /// A format representable directly by `wgpu::TextureFormat` (including BC formats even if the
    /// device does not have the corresponding compression features enabled).
    Wgpu(wgpu::TextureFormat),
    /// D3D-style packed 16-bit B5G6R5 UNORM, stored in guest memory as little-endian u16 with:
    /// `b5 | (g6 << 5) | (r5 << 11)`.
    ///
    /// wgpu 0.20's WebGPU `TextureFormat` does not expose this format, so we expand to RGBA8 on CPU
    /// when uploading.
    B5G6R5Unorm,
    /// D3D-style packed 16-bit B5G5R5A1 UNORM, stored in guest memory as little-endian u16 with:
    /// `b5 | (g5 << 5) | (r5 << 10) | (a1 << 15)`.
    ///
    /// wgpu 0.20's WebGPU `TextureFormat` does not expose this format, so we expand to RGBA8 on CPU
    /// when uploading.
    B5G5R5A1Unorm,
}

impl LinearTextureFormat {
    fn as_wgpu(self) -> Option<wgpu::TextureFormat> {
        match self {
            Self::Wgpu(fmt) => Some(fmt),
            _ => None,
        }
    }

    #[allow(dead_code)]
    fn is_srgb(self) -> bool {
        self.as_wgpu().is_some_and(is_srgb_format)
    }

    fn is_bc_compressed(self) -> bool {
        self.as_wgpu().is_some_and(is_bc_compressed_format)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    /// Format of the texture data in the linear backing store (guest allocations and UPLOAD_RESOURCE
    /// payloads).
    pub format: LinearTextureFormat,
    /// Actual format of the host `wgpu::Texture`.
    ///
    /// This differs from [`Self::format`] when the guest requests a BC-compressed format but the
    /// device does not have `TEXTURE_COMPRESSION_BC` enabled; in that case we fall back to an
    /// RGBA8 texture and decompress BC blocks on upload.
    ///
    /// Additionally, 16-bit packed B5G6R5 / B5G5R5A1 formats are always expanded to RGBA8 on upload
    /// since they are not representable as a `wgpu::TextureFormat`.
    pub texture_format: wgpu::TextureFormat,
    pub row_pitch_bytes: u32,
    pub upload_transform: TextureUploadTransform,
}

#[derive(Clone, Copy, Debug)]
pub struct Texture2dCreateDesc {
    pub usage_flags: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub row_pitch_bytes: u32,
    pub backing_alloc_id: u32,
    pub backing_offset_bytes: u32,
}

#[derive(Debug)]
pub struct Texture2dResource {
    pub texture: wgpu::Texture,
    pub desc: Texture2dDesc,
    pub usage_flags: u32,
    pub backing: Option<BackingInfo>,
    host_shadow: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureUploadTransform {
    Direct,
    Bc1ToRgba8,
    Bc2ToRgba8,
    Bc3ToRgba8,
    Bc7ToRgba8,
    B5G6R5ToRgba8,
    B5G5R5A1ToRgba8,
}

impl TextureUploadTransform {
    fn uses_bc_decompression(self) -> bool {
        matches!(
            self,
            Self::Bc1ToRgba8 | Self::Bc2ToRgba8 | Self::Bc3ToRgba8 | Self::Bc7ToRgba8
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct ShaderReflection {
    /// Vertex shader input signature mapping, derived from the DXBC `ISGN` chunk when available.
    pub vs_input_signature: Vec<VsInputSignatureElement>,
}

#[derive(Debug)]
pub struct ShaderResource {
    pub stage: AerogpuShaderStage,
    pub dxbc_hash_fnv1a64: u64,
    pub wgsl: String,
    pub module: wgpu::ShaderModule,
    pub reflection: ShaderReflection,
}

#[derive(Clone, Debug)]
pub struct InputLayoutResource {
    pub layout: InputLayoutDesc,
    pub mapping_cache: HashMap<u64, MappedInputLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRange {
    pub offset_bytes: u64,
    pub size_bytes: u64,
}

impl DirtyRange {
    pub fn end_bytes(self) -> Result<u64> {
        self.offset_bytes
            .checked_add(self.size_bytes)
            .ok_or_else(|| anyhow!("byte range overflows u64"))
    }
}

pub struct AerogpuResourceManager {
    device: wgpu::Device,
    queue: wgpu::Queue,
    supports_storage_buffers: bool,

    buffers: HashMap<AerogpuHandle, BufferResource>,
    textures2d: HashMap<AerogpuHandle, Texture2dResource>,
    shaders: HashMap<AerogpuHandle, ShaderResource>,
    input_layouts: HashMap<AerogpuHandle, InputLayoutResource>,
}

impl AerogpuResourceManager {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let supports_storage_buffers = device_supports_storage_buffers(&device);
        Self {
            device,
            queue,
            supports_storage_buffers,
            buffers: HashMap::new(),
            textures2d: HashMap::new(),
            shaders: HashMap::new(),
            input_layouts: HashMap::new(),
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn create_buffer(
        &mut self,
        handle: AerogpuHandle,
        usage_flags: u32,
        size_bytes: u64,
        backing_alloc_id: u32,
        backing_offset_bytes: u32,
    ) -> Result<()> {
        self.ensure_resource_handle_unused(handle)?;

        if size_bytes == 0 {
            bail!("CreateBuffer: size_bytes must be > 0");
        }
        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if !size_bytes.is_multiple_of(alignment) {
            bail!("CreateBuffer: size_bytes must be a multiple of {alignment} (got {size_bytes})");
        }

        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu buffer"),
            size: size_bytes,
            usage: map_buffer_usage_flags(usage_flags, self.supports_storage_buffers),
            mapped_at_creation: false,
        });

        let backing = (backing_alloc_id != 0).then_some(BackingInfo {
            alloc_id: backing_alloc_id,
            alloc_offset_bytes: backing_offset_bytes,
        });

        self.buffers.insert(
            handle,
            BufferResource {
                buffer,
                size: size_bytes,
                usage_flags,
                backing,
            },
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_texture2d(
        &mut self,
        handle: AerogpuHandle,
        desc: Texture2dCreateDesc,
    ) -> Result<()> {
        self.ensure_resource_handle_unused(handle)?;

        let Texture2dCreateDesc {
            usage_flags,
            format,
            width,
            height,
            mip_levels,
            array_layers,
            row_pitch_bytes,
            backing_alloc_id,
            backing_offset_bytes,
        } = desc;

        if width == 0 || height == 0 {
            bail!("CreateTexture2d: width/height must be non-zero");
        }
        if mip_levels == 0 {
            bail!("CreateTexture2d: mip_levels must be >= 1");
        }
        if array_layers == 0 {
            bail!("CreateTexture2d: array_layers must be >= 1");
        }
        // WebGPU validation requires `mip_level_count` to be within the possible chain length for
        // the given dimensions.
        let max_dim = width.max(height);
        let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
        if mip_levels > max_mip_levels {
            bail!(
                "CreateTexture2d: mip_levels too large for dimensions (width={width}, height={height}, mip_levels={mip_levels}, max_mip_levels={max_mip_levels})"
            );
        }
        if backing_alloc_id != 0 && row_pitch_bytes == 0 {
            bail!("CreateTexture2d: row_pitch_bytes is required for allocation-backed textures");
        }

        let linear_format = map_aerogpu_format(format)?;
        let bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let bc_enabled = bc_enabled
            && wgpu_compressed_texture_dimensions_compatible(
                linear_format,
                width,
                height,
                mip_levels,
            )?;
        let (texture_format, upload_transform) =
            select_texture_format_for_device(linear_format, bc_enabled)?;

        if row_pitch_bytes != 0 {
            let min_row_pitch = texture_unpadded_bytes_per_row(linear_format, width)?;
            if row_pitch_bytes < min_row_pitch {
                bail!(
                    "CreateTexture2d: row_pitch_bytes {} is smaller than required {}",
                    row_pitch_bytes,
                    min_row_pitch
                );
            }
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aerogpu texture2d"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: array_layers,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: texture_format,
            usage: map_texture_usage_flags(usage_flags),
            view_formats: &[],
        });

        let backing = (backing_alloc_id != 0).then_some(BackingInfo {
            alloc_id: backing_alloc_id,
            alloc_offset_bytes: backing_offset_bytes,
        });

        let desc = Texture2dDesc {
            width,
            height,
            mip_levels,
            array_layers,
            format: linear_format,
            texture_format,
            row_pitch_bytes,
            upload_transform,
        };

        let host_shadow = if backing.is_some() {
            None
        } else {
            Some(vec![0u8; texture_total_size_bytes(&desc)?])
        };

        self.textures2d.insert(
            handle,
            Texture2dResource {
                texture,
                desc,
                usage_flags,
                backing,
                host_shadow,
            },
        );
        Ok(())
    }

    pub fn create_shader_dxbc(
        &mut self,
        handle: AerogpuHandle,
        stage: u32,
        dxbc_bytes: &[u8],
    ) -> Result<()> {
        self.ensure_shader_handle_unused(handle)?;

        let stage = match stage {
            x if x == AerogpuShaderStage::Vertex as u32 => Some(AerogpuShaderStage::Vertex),
            x if x == AerogpuShaderStage::Pixel as u32 => Some(AerogpuShaderStage::Pixel),
            x if x == AerogpuShaderStage::Compute as u32 => Some(AerogpuShaderStage::Compute),
            x if x == AerogpuShaderStage::Geometry as u32 => None,
            _ => bail!("CreateShaderDxbc: unknown aerogpu_shader_stage {stage}"),
        };

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);

        let dxbc = DxbcFile::parse(dxbc_bytes).context("parse DXBC container")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse SM4/SM5 program")?;

        // SM5 geometry shaders can emit to multiple output streams via
        // `emit_stream` / `cut_stream` / `emitthen_cut_stream`.
        // Aero's initial GS bring-up only targets stream 0, so reject any shaders that use a
        // non-zero stream index with a clear diagnostic.
        //
        // Validate before stage dispatch so the policy is enforced even for GS/HS/DS shaders that
        // are accepted-but-ignored by this resource-manager/runtime path.
        if let Some(v) = crate::sm4::scan_sm5_nonzero_gs_stream(&program) {
            bail!(
                "CreateShaderDxbc: unsupported {} stream index {} at dword {} (only stream 0 is supported)",
                v.op_name,
                v.stream,
                v.at_dword
            );
        }

        let parsed_stage = match program.stage {
            ShaderStage::Vertex => Some(AerogpuShaderStage::Vertex),
            ShaderStage::Pixel => Some(AerogpuShaderStage::Pixel),
            ShaderStage::Compute => Some(AerogpuShaderStage::Compute),
            // WebGPU has no *native* geometry/hull/domain shader stage. Some Win7-era D3D11 apps
            // still create these shaders; accept the create to keep this resource-manager path
            // robust.
            //
            // Note: GS emulation via compute exists elsewhere (see `runtime/aerogpu_cmd_executor.rs`),
            // but it is not wired through this `AerogpuResourceManager` stack yet.
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                return Ok(());
            }
            other => bail!("CreateShaderDxbc: unsupported DXBC shader stage {other:?}"),
        };
        match (stage, parsed_stage) {
            (Some(cmd), Some(dxbc)) => {
                if cmd != dxbc {
                    bail!("CreateShaderDxbc: stage mismatch (cmd={cmd:?}, dxbc={dxbc:?})");
                }
            }
            (None, Some(dxbc)) => {
                bail!("CreateShaderDxbc: stage mismatch (cmd=Geometry, dxbc={dxbc:?})");
            }
            // DXBC GS/HS/DS stages are accepted-but-ignored by this resource-manager/runtime path;
            // stage mismatch is irrelevant.
            (_, None) => return Ok(()),
        }

        let stage = stage.expect("non-geometry stages handled above");
        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;

        // Future-proofing for SM5 geometry shader output streams:
        //
        // Signature entries include a `stream` field (used by GS multi-stream output / stream-out).
        // Our rasterization pipeline only supports stream 0 at the moment, so reject shaders that
        // declare non-zero streams to avoid silent misrendering.
        if matches!(
            stage,
            AerogpuShaderStage::Vertex | AerogpuShaderStage::Pixel
        ) {
            if let Some(osgn) = signatures.osgn.as_ref() {
                for p in &osgn.parameters {
                    if p.stream != 0 {
                        bail!(
                            "CreateShaderDxbc: output signature parameter {}{} (r{}) is declared on stream {} (only stream 0 is supported)",
                            p.semantic_name,
                            p.semantic_index,
                            p.register,
                            p.stream
                        );
                    }
                }
            }
        }

        // Compute shaders often omit signature chunks entirely. The signature-driven translator
        // can still handle compute modules, so only require ISGN/OSGN for VS/PS.
        let signature_driven = match stage {
            AerogpuShaderStage::Compute => true,
            _ => signatures.isgn.is_some() && signatures.osgn.is_some(),
        };
        let wgsl = if signature_driven {
            try_translate_sm4_signature_driven(&dxbc, &program, &signatures)?
        } else {
            translate_sm4_to_wgsl_bootstrap(&program)
                .map_err(|e| anyhow!("DXBC->WGSL translation failed: {e}"))?
                .wgsl
        };

        let module = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aerogpu shader module"),
                source: wgpu::ShaderSource::Wgsl(wgsl.clone().into()),
            });

        let reflection = if stage == AerogpuShaderStage::Vertex && signature_driven {
            let module =
                crate::sm4::decode_program(&program).context("decode SM4/5 token stream")?;
            ShaderReflection {
                vs_input_signature: extract_vs_input_signature_unique_locations(
                    &signatures,
                    &module,
                )?,
            }
        } else {
            build_shader_reflection(stage, &signatures)
        };

        self.shaders.insert(
            handle,
            ShaderResource {
                stage,
                dxbc_hash_fnv1a64,
                wgsl,
                module,
                reflection,
            },
        );
        Ok(())
    }

    pub fn create_input_layout(&mut self, handle: AerogpuHandle, blob: Vec<u8>) -> Result<()> {
        self.ensure_input_layout_handle_unused(handle)?;

        self.input_layouts.insert(
            handle,
            InputLayoutResource {
                layout: InputLayoutDesc::parse(&blob)
                    .map_err(|e| anyhow!("failed to parse ILAY input layout blob: {e}"))?,
                mapping_cache: HashMap::new(),
            },
        );
        Ok(())
    }

    pub fn destroy_resource(&mut self, handle: AerogpuHandle) -> Result<()> {
        let mut removed = false;
        removed |= self.buffers.remove(&handle).is_some();
        removed |= self.textures2d.remove(&handle).is_some();
        if removed {
            Ok(())
        } else {
            bail!("DestroyResource: unknown handle {handle}")
        }
    }

    pub fn destroy_shader(&mut self, handle: AerogpuHandle) -> Result<()> {
        // Destruction should be robust: some DXBC shader stages (GS/HS/DS) are accepted-but-ignored
        // by this resource-manager/runtime path. In those cases we never insert a shader resource,
        // so a later destroy should be a no-op rather than a hard error.
        self.shaders.remove(&handle);
        Ok(())
    }

    pub fn destroy_input_layout(&mut self, handle: AerogpuHandle) -> Result<()> {
        if self.input_layouts.remove(&handle).is_none() {
            bail!("DestroyInputLayout: unknown handle {handle}");
        }
        Ok(())
    }

    pub fn buffer(&self, handle: AerogpuHandle) -> Result<&BufferResource> {
        self.buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))
    }

    pub fn texture2d(&self, handle: AerogpuHandle) -> Result<&Texture2dResource> {
        self.textures2d
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown texture2d handle {handle}"))
    }

    pub fn shader(&self, handle: AerogpuHandle) -> Result<&ShaderResource> {
        self.shaders
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown shader handle {handle}"))
    }

    pub fn input_layout(&self, handle: AerogpuHandle) -> Result<&InputLayoutResource> {
        self.input_layouts
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown input layout handle {handle}"))
    }

    /// Map an ILAY input layout + currently-bound vertex buffer strides into WebGPU vertex layouts.
    ///
    /// The mapping is cached per input layout, keyed by `(vertex_shader_dxbc_hash, slot_strides)`.
    /// The returned [`MappedInputLayout`] includes both the WebGPU vertex buffer layouts and the
    /// D3D slot → WebGPU slot mapping that must be applied when binding vertex buffers.
    ///
    /// If the vertex shader's `ISGN` signature is unavailable, this falls back to mapping ILAY
    /// semantics in declaration order to shader locations `0..N` (sufficient for bring-up shaders).
    pub fn input_layout_vertex_buffer_layouts(
        &mut self,
        input_layout_handle: AerogpuHandle,
        vertex_shader_handle: AerogpuHandle,
        slot_strides: &[u32],
    ) -> Result<&MappedInputLayout> {
        let vs = self
            .shaders
            .get(&vertex_shader_handle)
            .ok_or_else(|| anyhow!("unknown shader handle {vertex_shader_handle}"))?;
        if vs.stage != AerogpuShaderStage::Vertex {
            bail!("shader {vertex_shader_handle} is not a vertex shader");
        }

        let layout = self
            .input_layouts
            .get_mut(&input_layout_handle)
            .ok_or_else(|| anyhow!("unknown input layout handle {input_layout_handle}"))?;

        let cache_key = hash_input_layout_mapping_key(vs.dxbc_hash_fnv1a64, slot_strides);
        if !layout.mapping_cache.contains_key(&cache_key) {
            let desc = &layout.layout;

            let vs_signature = if vs.reflection.vs_input_signature.is_empty() {
                build_fallback_vs_signature(desc)
            } else {
                vs.reflection.vs_input_signature.clone()
            };

            let binding = InputLayoutBinding::new(desc, slot_strides);
            let mapped = map_layout_to_shader_locations_compact(&binding, &vs_signature)
                .map_err(|e| anyhow!("{e}"))?;
            layout.mapping_cache.insert(cache_key, mapped);
        }

        Ok(layout
            .mapping_cache
            .get(&cache_key)
            .expect("mapping cache entry must exist"))
    }

    pub fn upload_resource(
        &mut self,
        handle: AerogpuHandle,
        range: DirtyRange,
        bytes: &[u8],
    ) -> Result<()> {
        let _end = range.end_bytes()?;
        let size_usize: usize = range
            .size_bytes
            .try_into()
            .context("UploadResource: size too large")?;
        if bytes.len() != size_usize {
            bail!(
                "UploadResource: payload size mismatch: cmd says {} bytes, payload has {}",
                range.size_bytes,
                bytes.len()
            );
        }

        if let Some(buf) = self.buffers.get(&handle) {
            validate_range_in_resource(range, buf.size)?;
            if buf.backing.is_some() {
                bail!("UploadResource: buffer {handle} is backed by a guest allocation");
            }
            let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
            if !range.offset_bytes.is_multiple_of(alignment)
                || !range.size_bytes.is_multiple_of(alignment)
            {
                bail!(
                    "UploadResource: buffer offset_bytes and size_bytes must be {alignment}-byte aligned (offset_bytes={} size_bytes={})",
                    range.offset_bytes,
                    range.size_bytes
                );
            }
            self.queue
                .write_buffer(&buf.buffer, range.offset_bytes, bytes);
            return Ok(());
        }

        if let Some(tex) = self.textures2d.get_mut(&handle) {
            let total_bytes = texture_total_size_bytes(&tex.desc)? as u64;
            validate_range_in_resource(range, total_bytes)?;
            if tex.backing.is_some() {
                bail!("UploadResource: texture {handle} is backed by a guest allocation");
            }

            let shadow = tex
                .host_shadow
                .as_mut()
                .ok_or_else(|| anyhow!("UploadResource: texture {handle} missing host shadow"))?;
            let start = range.offset_bytes as usize;
            shadow[start..start + bytes.len()].copy_from_slice(bytes);

            // For P0, conservatively re-upload the entire texture from the host shadow.
            upload_texture_from_linear_bytes(&self.queue, &tex.texture, &tex.desc, shadow)?;
            return Ok(());
        }

        bail!("UploadResource: unknown handle {handle}");
    }

    /// Ensure a backed buffer is present in the corresponding `wgpu::Buffer`.
    ///
    /// P0 behaviour is conservative: if any dirty range is reported, the entire
    /// buffer is re-uploaded from the guest backing allocation.
    pub fn ensure_buffer_uploaded(
        &mut self,
        handle: AerogpuHandle,
        dirty: DirtyRange,
        guest_mem: &mut dyn GuestMemory,
        alloc_table: &HashMap<u32, AerogpuAllocEntry>,
    ) -> Result<()> {
        let buf = self
            .buffers
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown buffer handle {handle}"))?;

        validate_range_in_resource(dirty, buf.size)?;

        let Some(backing) = buf.backing else {
            return Ok(());
        };
        let alloc = alloc_table.get(&backing.alloc_id).ok_or_else(|| {
            anyhow!(
                "missing alloc entry {} for buffer {handle}",
                backing.alloc_id
            )
        })?;

        let start = alloc
            .gpa
            .checked_add(backing.alloc_offset_bytes as u64)
            .ok_or_else(|| anyhow!("alloc GPA+offset overflows u64"))?;
        let end = (backing.alloc_offset_bytes as u64)
            .checked_add(buf.size)
            .ok_or_else(|| anyhow!("alloc offset+buffer size overflows u64"))?;
        if end > alloc.size_bytes {
            bail!(
                "buffer {handle} backing range out of bounds: alloc_size={} offset={} size={}",
                alloc.size_bytes,
                backing.alloc_offset_bytes,
                buf.size
            );
        }

        let alignment = wgpu::COPY_BUFFER_ALIGNMENT;
        if !buf.size.is_multiple_of(alignment) {
            bail!(
                "buffer {handle} size_bytes must be a multiple of {alignment} (got {})",
                buf.size
            );
        }

        let size_usize: usize = buf.size.try_into().context("buffer too large")?;
        let mut tmp = vec![0u8; size_usize];
        guest_mem
            .read(start, &mut tmp)
            .with_context(|| format!("read guest backing for buffer {handle}"))?;

        self.queue.write_buffer(&buf.buffer, 0, &tmp);
        Ok(())
    }

    /// Ensure a backed texture is present in the corresponding `wgpu::Texture`.
    ///
    /// P0 behaviour is conservative: if any dirty range is reported, the entire
    /// texture is re-uploaded from the guest backing allocation.
    pub fn ensure_texture_uploaded(
        &mut self,
        handle: AerogpuHandle,
        dirty: DirtyRange,
        guest_mem: &mut dyn GuestMemory,
        alloc_table: &HashMap<u32, AerogpuAllocEntry>,
    ) -> Result<()> {
        let tex = self
            .textures2d
            .get_mut(&handle)
            .ok_or_else(|| anyhow!("unknown texture2d handle {handle}"))?;

        let total_bytes = texture_total_size_bytes(&tex.desc)? as u64;
        validate_range_in_resource(dirty, total_bytes)?;

        let Some(backing) = tex.backing else {
            return Ok(());
        };
        let alloc = alloc_table.get(&backing.alloc_id).ok_or_else(|| {
            anyhow!(
                "missing alloc entry {} for texture {handle}",
                backing.alloc_id
            )
        })?;

        let start = alloc
            .gpa
            .checked_add(backing.alloc_offset_bytes as u64)
            .ok_or_else(|| anyhow!("alloc GPA+offset overflows u64"))?;
        let end = (backing.alloc_offset_bytes as u64)
            .checked_add(total_bytes)
            .ok_or_else(|| anyhow!("alloc offset+texture size overflows u64"))?;
        if end > alloc.size_bytes {
            bail!(
                "texture {handle} backing range out of bounds: alloc_size={} offset={} size={}",
                alloc.size_bytes,
                backing.alloc_offset_bytes,
                total_bytes
            );
        }

        let size_usize: usize = total_bytes.try_into().context("texture too large")?;
        let mut tmp = vec![0u8; size_usize];
        guest_mem
            .read(start, &mut tmp)
            .with_context(|| format!("read guest backing for texture {handle}"))?;

        upload_texture_from_linear_bytes(&self.queue, &tex.texture, &tex.desc, &tmp)?;
        Ok(())
    }

    fn ensure_resource_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.buffers.contains_key(&handle) || self.textures2d.contains_key(&handle) {
            bail!("resource handle {handle} is already in use");
        }
        Ok(())
    }

    fn ensure_shader_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.shaders.contains_key(&handle) {
            bail!("shader handle {handle} is already in use");
        }
        Ok(())
    }

    fn ensure_input_layout_handle_unused(&self, handle: AerogpuHandle) -> Result<()> {
        if self.input_layouts.contains_key(&handle) {
            bail!("input layout handle {handle} is already in use");
        }
        Ok(())
    }
}

fn try_translate_sm4_signature_driven(
    dxbc: &DxbcFile<'_>,
    program: &Sm4Program,
    signatures: &crate::ShaderSignatures,
) -> Result<String> {
    let module = crate::sm4::decode_program(program).context("decode SM4/5 token stream")?;
    let translated = translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")?;
    Ok(translated.wgsl)
}

fn extract_vs_input_signature_unique_locations(
    signatures: &crate::ShaderSignatures,
    module: &crate::Sm4Module,
) -> Result<Vec<VsInputSignatureElement>> {
    const D3D_NAME_VERTEX_ID: u32 = 6;
    const D3D_NAME_INSTANCE_ID: u32 = 8;

    let Some(isgn) = signatures.isgn.as_ref() else {
        return Ok(Vec::new());
    };

    let mut sivs = HashMap::<u32, u32>::new();
    for decl in &module.decls {
        if let crate::Sm4Decl::InputSiv { reg, sys_value, .. } = decl {
            sivs.insert(*reg, *sys_value);
        }
    }

    let mut out = Vec::new();
    let mut next_location = 0u32;
    for p in &isgn.parameters {
        let sys_value = sivs
            .get(&p.register)
            .copied()
            .or_else(|| (p.system_value_type != 0).then_some(p.system_value_type));

        let is_builtin = matches!(sys_value, Some(D3D_NAME_VERTEX_ID | D3D_NAME_INSTANCE_ID))
            || p.semantic_name.eq_ignore_ascii_case("SV_VertexID")
            || p.semantic_name.eq_ignore_ascii_case("SV_InstanceID");
        if is_builtin {
            continue;
        }

        out.push(VsInputSignatureElement {
            semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
            semantic_index: p.semantic_index,
            input_register: p.register,
            mask: p.mask,
            shader_location: next_location,
        });
        next_location += 1;
    }

    Ok(out)
}

fn build_shader_reflection(
    stage: AerogpuShaderStage,
    signatures: &crate::ShaderSignatures,
) -> ShaderReflection {
    let mut reflection = ShaderReflection::default();

    if stage == AerogpuShaderStage::Vertex {
        if let Some(isgn) = signatures.isgn.as_ref() {
            reflection.vs_input_signature = isgn
                .parameters
                .iter()
                .map(|p| VsInputSignatureElement {
                    // D3D semantics are case-insensitive, but the signature chunk stores the
                    // original string. The aerogpu ILAY protocol only preserves a hash, so we
                    // canonicalize to ASCII uppercase to match how the guest typically hashes
                    // semantic names.
                    semantic_name_hash: fnv1a_32(p.semantic_name.to_ascii_uppercase().as_bytes()),
                    semantic_index: p.semantic_index,
                    input_register: p.register,
                    mask: p.mask,
                    shader_location: p.register,
                })
                .collect();
        }
    }

    reflection
}

fn build_fallback_vs_signature(desc: &InputLayoutDesc) -> Vec<VsInputSignatureElement> {
    let mut seen: HashMap<(u32, u32), u32> = HashMap::new();
    let mut out: Vec<VsInputSignatureElement> = Vec::new();

    for elem in &desc.elements {
        let key = (elem.semantic_name_hash, elem.semantic_index);
        if seen.contains_key(&key) {
            continue;
        }
        let reg = out.len() as u32;
        seen.insert(key, reg);
        out.push(VsInputSignatureElement {
            semantic_name_hash: key.0,
            semantic_index: key.1,
            input_register: reg,
            mask: 0xF,
            shader_location: reg,
        });
    }

    out
}

fn hash_input_layout_mapping_key(vs_dxbc_hash_fnv1a64: u64, slot_strides: &[u32]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, &vs_dxbc_hash_fnv1a64.to_le_bytes());
    fnv1a64_update(&mut hash, &(slot_strides.len() as u32).to_le_bytes());
    for &stride in slot_strides {
        fnv1a64_update(&mut hash, &stride.to_le_bytes());
    }
    hash
}

pub fn map_aerogpu_format(format: u32) -> Result<LinearTextureFormat> {
    Ok(match format {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8Unorm)
        }
        x if x == AerogpuFormat::B8G8R8X8Unorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8Unorm)
        }
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8Unorm)
        }
        x if x == AerogpuFormat::R8G8B8X8Unorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8Unorm)
        }
        x if x == AerogpuFormat::B8G8R8A8UnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8UnormSrgb)
        }
        x if x == AerogpuFormat::B8G8R8X8UnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8UnormSrgb)
        }
        x if x == AerogpuFormat::R8G8B8A8UnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8UnormSrgb)
        }
        x if x == AerogpuFormat::R8G8B8X8UnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8UnormSrgb)
        }

        x if x == AerogpuFormat::B5G6R5Unorm as u32 => LinearTextureFormat::B5G6R5Unorm,
        x if x == AerogpuFormat::B5G5R5A1Unorm as u32 => LinearTextureFormat::B5G5R5A1Unorm,

        x if x == AerogpuFormat::BC1RgbaUnorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm)
        }
        x if x == AerogpuFormat::BC1RgbaUnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnormSrgb)
        }
        x if x == AerogpuFormat::BC2RgbaUnorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc2RgbaUnorm)
        }
        x if x == AerogpuFormat::BC2RgbaUnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc2RgbaUnormSrgb)
        }
        x if x == AerogpuFormat::BC3RgbaUnorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc3RgbaUnorm)
        }
        x if x == AerogpuFormat::BC3RgbaUnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc3RgbaUnormSrgb)
        }
        x if x == AerogpuFormat::BC7RgbaUnorm as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc7RgbaUnorm)
        }
        x if x == AerogpuFormat::BC7RgbaUnormSrgb as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc7RgbaUnormSrgb)
        }
        x if x == AerogpuFormat::D24UnormS8Uint as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Depth24PlusStencil8)
        }
        x if x == AerogpuFormat::D32Float as u32 => {
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Depth32Float)
        }
        _ => bail!("unsupported aerogpu_format {format}"),
    })
}

pub fn map_buffer_usage_flags(usage_flags: u32, supports_compute: bool) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
    // Compute-based GS emulation (vertex pulling + expansion) and raw/structured buffer bindings
    // represent buffers as `var<storage>` in WGSL. wgpu requires buffers used in storage bindings
    // to be created with `BufferUsages::STORAGE`. Gate this on backend support; downlevel/WebGL2
    // backends do not support compute/storage buffers.
    if supports_compute {
        out |= wgpu::BufferUsages::STORAGE;
    }
    if (usage_flags & AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER) != 0 {
        out |= wgpu::BufferUsages::VERTEX;
    }
    if (usage_flags & AEROGPU_RESOURCE_USAGE_INDEX_BUFFER) != 0 {
        out |= wgpu::BufferUsages::INDEX;
    }
    if (usage_flags & AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER) != 0 {
        out |= wgpu::BufferUsages::UNIFORM;
    }
    out
}

pub fn map_texture_usage_flags(usage_flags: u32) -> wgpu::TextureUsages {
    let mut out = wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
    if (usage_flags & AEROGPU_RESOURCE_USAGE_TEXTURE) != 0 {
        out |= wgpu::TextureUsages::TEXTURE_BINDING;
    }
    if (usage_flags
        & (AEROGPU_RESOURCE_USAGE_RENDER_TARGET
            | AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL
            | AEROGPU_RESOURCE_USAGE_SCANOUT))
        != 0
    {
        out |= wgpu::TextureUsages::RENDER_ATTACHMENT;
    }
    out
}

pub fn validate_range_in_resource(range: DirtyRange, total_size_bytes: u64) -> Result<()> {
    let end = range.end_bytes()?;
    if range.offset_bytes > total_size_bytes {
        bail!(
            "byte range out of bounds: offset={} total={}",
            range.offset_bytes,
            total_size_bytes
        );
    }
    if end > total_size_bytes {
        bail!(
            "byte range out of bounds: offset={} size={} end={} total={}",
            range.offset_bytes,
            range.size_bytes,
            end,
            total_size_bytes
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextureFormatLayout {
    Uncompressed {
        bytes_per_texel: u32,
    },
    BlockCompressed {
        block_width: u32,
        block_height: u32,
        bytes_per_block: u32,
    },
}

fn wgpu_format_layout_info(format: wgpu::TextureFormat) -> Result<TextureFormatLayout> {
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb
        | wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Depth24PlusStencil8
        | wgpu::TextureFormat::Depth32Float => {
            TextureFormatLayout::Uncompressed { bytes_per_texel: 4 }
        }

        wgpu::TextureFormat::Bc1RgbaUnorm | wgpu::TextureFormat::Bc1RgbaUnormSrgb => {
            TextureFormatLayout::BlockCompressed {
                block_width: 4,
                block_height: 4,
                bytes_per_block: 8,
            }
        }
        wgpu::TextureFormat::Bc2RgbaUnorm
        | wgpu::TextureFormat::Bc2RgbaUnormSrgb
        | wgpu::TextureFormat::Bc3RgbaUnorm
        | wgpu::TextureFormat::Bc3RgbaUnormSrgb
        | wgpu::TextureFormat::Bc7RgbaUnorm
        | wgpu::TextureFormat::Bc7RgbaUnormSrgb => TextureFormatLayout::BlockCompressed {
            block_width: 4,
            block_height: 4,
            bytes_per_block: 16,
        },

        other => bail!("unsupported texture format {other:?}"),
    })
}

fn linear_format_layout_info(format: LinearTextureFormat) -> Result<TextureFormatLayout> {
    Ok(match format {
        LinearTextureFormat::Wgpu(fmt) => wgpu_format_layout_info(fmt)?,
        LinearTextureFormat::B5G6R5Unorm | LinearTextureFormat::B5G5R5A1Unorm => {
            TextureFormatLayout::Uncompressed { bytes_per_texel: 2 }
        }
    })
}

fn is_srgb_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bgra8UnormSrgb
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bc1RgbaUnormSrgb
            | wgpu::TextureFormat::Bc2RgbaUnormSrgb
            | wgpu::TextureFormat::Bc3RgbaUnormSrgb
            | wgpu::TextureFormat::Bc7RgbaUnormSrgb
    )
}

fn is_bc_compressed_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bc1RgbaUnorm
            | wgpu::TextureFormat::Bc1RgbaUnormSrgb
            | wgpu::TextureFormat::Bc2RgbaUnorm
            | wgpu::TextureFormat::Bc2RgbaUnormSrgb
            | wgpu::TextureFormat::Bc3RgbaUnorm
            | wgpu::TextureFormat::Bc3RgbaUnormSrgb
            | wgpu::TextureFormat::Bc7RgbaUnorm
            | wgpu::TextureFormat::Bc7RgbaUnormSrgb
    )
}

fn select_texture_format_for_device(
    requested: LinearTextureFormat,
    bc_enabled: bool,
) -> Result<(wgpu::TextureFormat, TextureUploadTransform)> {
    Ok(match requested {
        LinearTextureFormat::B5G6R5Unorm => (
            wgpu::TextureFormat::Rgba8Unorm,
            TextureUploadTransform::B5G6R5ToRgba8,
        ),
        LinearTextureFormat::B5G5R5A1Unorm => (
            wgpu::TextureFormat::Rgba8Unorm,
            TextureUploadTransform::B5G5R5A1ToRgba8,
        ),
        LinearTextureFormat::Wgpu(requested) => {
            if !is_bc_compressed_format(requested) {
                return Ok((requested, TextureUploadTransform::Direct));
            }

            if bc_enabled {
                return Ok((requested, TextureUploadTransform::Direct));
            }

            let fallback = if is_srgb_format(requested) {
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                wgpu::TextureFormat::Rgba8Unorm
            };

            let transform = match requested {
                wgpu::TextureFormat::Bc1RgbaUnorm | wgpu::TextureFormat::Bc1RgbaUnormSrgb => {
                    TextureUploadTransform::Bc1ToRgba8
                }
                wgpu::TextureFormat::Bc2RgbaUnorm | wgpu::TextureFormat::Bc2RgbaUnormSrgb => {
                    TextureUploadTransform::Bc2ToRgba8
                }
                wgpu::TextureFormat::Bc3RgbaUnorm | wgpu::TextureFormat::Bc3RgbaUnormSrgb => {
                    TextureUploadTransform::Bc3ToRgba8
                }
                wgpu::TextureFormat::Bc7RgbaUnorm | wgpu::TextureFormat::Bc7RgbaUnormSrgb => {
                    TextureUploadTransform::Bc7ToRgba8
                }
                _ => bail!("unsupported BC format {requested:?}"),
            };

            (fallback, transform)
        }
    })
}

fn wgpu_compressed_texture_dimensions_compatible(
    format: LinearTextureFormat,
    width: u32,
    height: u32,
    mip_levels: u32,
) -> Result<bool> {
    if mip_levels == 0 {
        return Ok(false);
    }

    let TextureFormatLayout::BlockCompressed {
        block_width,
        block_height,
        ..
    } = linear_format_layout_info(format)?
    else {
        return Ok(true);
    };

    // wgpu/WebGPU validation currently requires the base mip dimensions to be block-aligned, even
    // when smaller than a full block (e.g. 2x2 BC). Fall back to an uncompressed host format when
    // the base dimensions are not aligned.
    if !width.is_multiple_of(block_width) || !height.is_multiple_of(block_height) {
        return Ok(false);
    }

    // WebGPU validation requires `mip_level_count` to be within the possible chain length for the
    // given dimensions (regardless of format). Rejecting this here avoids creating textures that
    // wgpu/WebGPU would reject, and prevents pathological loops if the guest provides an
    // excessively large mip count.
    let max_dim = width.max(height);
    let max_mip_levels = 32u32.saturating_sub(max_dim.leading_zeros());
    if mip_levels > max_mip_levels {
        return Ok(false);
    }

    for level in 0..mip_levels {
        let w = mip_extent(width, level);
        let h = mip_extent(height, level);
        // WebGPU validation requires block-compressed texture dimensions to be block-aligned when
        // at least one full block is present. (Smaller-than-block mips are allowed.)
        if (w >= block_width && !w.is_multiple_of(block_width))
            || (h >= block_height && !h.is_multiple_of(block_height))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn mip_extent(v: u32, level: u32) -> u32 {
    v.checked_shr(level).unwrap_or(0).max(1)
}

fn align_copy_bytes_per_row(bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes_per_row.div_ceil(align) * align
}

fn texture_unpadded_bytes_per_row(format: LinearTextureFormat, width_texels: u32) -> Result<u32> {
    let info = linear_format_layout_info(format)?;
    Ok(match info {
        TextureFormatLayout::Uncompressed { bytes_per_texel } => width_texels
            .checked_mul(bytes_per_texel)
            .ok_or_else(|| anyhow!("bytes_per_row overflow"))?,
        TextureFormatLayout::BlockCompressed {
            block_width,
            bytes_per_block,
            ..
        } => {
            let blocks_w = width_texels.div_ceil(block_width);
            blocks_w
                .checked_mul(bytes_per_block)
                .ok_or_else(|| anyhow!("bytes_per_row overflow"))?
        }
    })
}

#[derive(Clone, Copy, Debug)]
struct LinearMipLayout {
    width: u32,
    height: u32,
    /// Minimum required bytes per row (no padding), expressed in bytes-per-row-of-texels for
    /// uncompressed formats and bytes-per-row-of-blocks for BC formats.
    unpadded_bytes_per_row: u32,
    /// Row pitch in the linear backing store. For mip0 this comes from `row_pitch_bytes` when
    /// provided; higher mips are always tightly packed.
    row_pitch_bytes: u32,
    /// Row count in the linear backing store. For uncompressed formats this is `height`; for BC
    /// formats this is the number of block rows.
    rows: u32,
}

impl LinearMipLayout {
    fn subresource_size_bytes(self) -> Result<u64> {
        (self.row_pitch_bytes as u64)
            .checked_mul(self.rows as u64)
            .ok_or_else(|| anyhow!("subresource size overflows u64"))
    }
}

fn texture_mip_layout(desc: &Texture2dDesc, mip_level: u32) -> Result<LinearMipLayout> {
    let width = mip_extent(desc.width, mip_level);
    let height = mip_extent(desc.height, mip_level);
    let info = linear_format_layout_info(desc.format)?;

    let (unpadded_bytes_per_row, rows) = match info {
        TextureFormatLayout::Uncompressed { bytes_per_texel } => (
            width
                .checked_mul(bytes_per_texel)
                .ok_or_else(|| anyhow!("bytes_per_row overflow"))?,
            height,
        ),
        TextureFormatLayout::BlockCompressed {
            block_width,
            block_height,
            bytes_per_block,
        } => {
            let blocks_w = width.div_ceil(block_width);
            let blocks_h = height.div_ceil(block_height);
            let bytes_per_row = blocks_w
                .checked_mul(bytes_per_block)
                .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
            (bytes_per_row, blocks_h)
        }
    };

    let row_pitch_bytes = if mip_level == 0 && desc.row_pitch_bytes != 0 {
        desc.row_pitch_bytes
    } else {
        unpadded_bytes_per_row
    };
    if row_pitch_bytes < unpadded_bytes_per_row {
        bail!(
            "row_pitch_bytes {} is smaller than required {}",
            row_pitch_bytes,
            unpadded_bytes_per_row
        );
    }

    Ok(LinearMipLayout {
        width,
        height,
        unpadded_bytes_per_row,
        row_pitch_bytes,
        rows,
    })
}

fn texture_subresource_size_bytes(desc: &Texture2dDesc, mip_level: u32) -> Result<u64> {
    texture_mip_layout(desc, mip_level)?.subresource_size_bytes()
}

pub fn texture_total_size_bytes(desc: &Texture2dDesc) -> Result<usize> {
    let mut total: u64 = 0;
    for _layer in 0..desc.array_layers {
        for mip in 0..desc.mip_levels {
            total = total
                .checked_add(texture_subresource_size_bytes(desc, mip)?)
                .ok_or_else(|| anyhow!("texture size overflows u64"))?;
        }
    }
    total.try_into().context("texture too large")
}

fn upload_texture_from_linear_bytes(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    desc: &Texture2dDesc,
    bytes: &[u8],
) -> Result<()> {
    let expected = texture_total_size_bytes(desc)?;
    if bytes.len() != expected {
        bail!(
            "texture upload size mismatch: expected {} bytes, got {}",
            expected,
            bytes.len()
        );
    }

    let mut offset = 0usize;
    for layer in 0..desc.array_layers {
        for mip in 0..desc.mip_levels {
            let linear = texture_mip_layout(desc, mip)?;
            let subresource_len: usize = linear
                .subresource_size_bytes()?
                .try_into()
                .context("subresource too large")?;
            let data = bytes
                .get(offset..offset + subresource_len)
                .ok_or_else(|| anyhow!("texture upload out of bounds"))?;

            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            if desc.upload_transform.uses_bc_decompression() {
                let bc_tight: std::borrow::Cow<'_, [u8]> =
                    if linear.row_pitch_bytes == linear.unpadded_bytes_per_row {
                        std::borrow::Cow::Borrowed(data)
                    } else {
                        let mut tmp = vec![0u8; subresource_len];
                        // Repack to tight BC layout for the decompressor.
                        let tight_len = (linear.unpadded_bytes_per_row as usize)
                            .checked_mul(linear.rows as usize)
                            .ok_or_else(|| anyhow!("repack size overflows usize"))?;
                        tmp.resize(tight_len, 0);
                        for y in 0..linear.rows as usize {
                            let src_start = y * linear.row_pitch_bytes as usize;
                            let dst_start = y * linear.unpadded_bytes_per_row as usize;
                            tmp[dst_start..dst_start + linear.unpadded_bytes_per_row as usize]
                                .copy_from_slice(
                                    &data[src_start
                                        ..src_start + linear.unpadded_bytes_per_row as usize],
                                );
                        }
                        std::borrow::Cow::Owned(tmp)
                    };

                // `aero_gpu::decompress_bc*_rgba8` asserts on input length; validate here to avoid
                // panicking on malformed guest data.
                let expected_bc_len: usize = match linear_format_layout_info(desc.format)? {
                    TextureFormatLayout::BlockCompressed {
                        block_width,
                        block_height,
                        bytes_per_block,
                    } => {
                        let blocks_w = linear.width.div_ceil(block_width) as usize;
                        let blocks_h = linear.height.div_ceil(block_height) as usize;
                        blocks_w
                            .checked_mul(blocks_h)
                            .and_then(|v| v.checked_mul(bytes_per_block as usize))
                            .ok_or_else(|| anyhow!("BC decompression size overflow"))?
                    }
                    TextureFormatLayout::Uncompressed { .. } => {
                        bail!(
                            "BC decompression upload transform requires a BC format (got {:?})",
                            desc.format
                        );
                    }
                };
                if bc_tight.len() != expected_bc_len {
                    bail!(
                        "BC decompression data length mismatch: expected {} bytes for {}x{} {:?}, got {}",
                        expected_bc_len,
                        linear.width,
                        linear.height,
                        desc.format,
                        bc_tight.len()
                    );
                }

                let rgba = match desc.upload_transform {
                    TextureUploadTransform::Bc1ToRgba8 => {
                        decompress_bc1_rgba8(linear.width, linear.height, bc_tight.as_ref())
                    }
                    TextureUploadTransform::Bc2ToRgba8 => {
                        decompress_bc2_rgba8(linear.width, linear.height, bc_tight.as_ref())
                    }
                    TextureUploadTransform::Bc3ToRgba8 => {
                        decompress_bc3_rgba8(linear.width, linear.height, bc_tight.as_ref())
                    }
                    TextureUploadTransform::Bc7ToRgba8 => {
                        decompress_bc7_rgba8(linear.width, linear.height, bc_tight.as_ref())
                    }
                    _ => unreachable!(),
                };

                if desc.texture_format != wgpu::TextureFormat::Rgba8Unorm
                    && desc.texture_format != wgpu::TextureFormat::Rgba8UnormSrgb
                {
                    bail!(
                        "BC decompression requires RGBA8 texture_format (got {:?})",
                        desc.texture_format
                    );
                }

                let unpadded_bytes_per_row = linear
                    .width
                    .checked_mul(4)
                    .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
                let padded_bytes_per_row =
                    if linear.height > 1 && !unpadded_bytes_per_row.is_multiple_of(align) {
                        align_copy_bytes_per_row(unpadded_bytes_per_row)
                    } else {
                        unpadded_bytes_per_row
                    };

                let upload_bytes: std::borrow::Cow<'_, [u8]> = if padded_bytes_per_row
                    == unpadded_bytes_per_row
                {
                    std::borrow::Cow::Owned(rgba)
                } else {
                    let mut tmp = vec![0u8; padded_bytes_per_row as usize * linear.height as usize];
                    for y in 0..linear.height as usize {
                        let src_start = y * unpadded_bytes_per_row as usize;
                        let dst_start = y * padded_bytes_per_row as usize;
                        tmp[dst_start..dst_start + unpadded_bytes_per_row as usize]
                            .copy_from_slice(
                                &rgba[src_start..src_start + unpadded_bytes_per_row as usize],
                            );
                    }
                    std::borrow::Cow::Owned(tmp)
                };

                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &upload_bytes,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(linear.height),
                    },
                    wgpu::Extent3d {
                        width: linear.width,
                        height: linear.height,
                        depth_or_array_layers: 1,
                    },
                );
            } else if matches!(
                desc.upload_transform,
                TextureUploadTransform::B5G6R5ToRgba8 | TextureUploadTransform::B5G5R5A1ToRgba8
            ) {
                // Expand packed 16-bit B5 formats into RGBA8 on CPU before upload.
                //
                // Expansion produces RGBA byte order (R, G, B, A) matching
                // `wgpu::TextureFormat::Rgba8Unorm`.
                if desc.texture_format != wgpu::TextureFormat::Rgba8Unorm {
                    bail!(
                        "B5 expansion requires Rgba8Unorm texture_format (got {:?})",
                        desc.texture_format
                    );
                }

                // Convert each row from the guest's row pitch (which can include padding) into a
                // tight RGBA8 buffer.
                let src_bpr_usize: usize = linear
                    .row_pitch_bytes
                    .try_into()
                    .context("row_pitch_bytes out of range")?;
                let src_tight_bpr_usize: usize = linear
                    .unpadded_bytes_per_row
                    .try_into()
                    .context("unpadded_bytes_per_row out of range")?;
                let dst_tight_bpr = linear
                    .width
                    .checked_mul(4)
                    .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
                let dst_tight_bpr_usize: usize = dst_tight_bpr
                    .try_into()
                    .context("dst bytes_per_row out of range")?;
                let height_usize: usize =
                    linear.height.try_into().context("height out of range")?;
                let tight_len = dst_tight_bpr_usize
                    .checked_mul(height_usize)
                    .ok_or_else(|| anyhow!("expanded size overflows usize"))?;
                let mut rgba = vec![0u8; tight_len];

                for row in 0..height_usize {
                    let src_start = row
                        .checked_mul(src_bpr_usize)
                        .ok_or_else(|| anyhow!("B5 src row offset overflow"))?;
                    let src_end = src_start
                        .checked_add(src_tight_bpr_usize)
                        .ok_or_else(|| anyhow!("B5 src row end overflow"))?;
                    let dst_start = row
                        .checked_mul(dst_tight_bpr_usize)
                        .ok_or_else(|| anyhow!("B5 dst row offset overflow"))?;
                    let dst_end = dst_start
                        .checked_add(dst_tight_bpr_usize)
                        .ok_or_else(|| anyhow!("B5 dst row end overflow"))?;
                    let src_row = data
                        .get(src_start..src_end)
                        .ok_or_else(|| anyhow!("B5 source too small for row"))?;
                    let dst_row = rgba
                        .get_mut(dst_start..dst_end)
                        .ok_or_else(|| anyhow!("B5 output too small for row"))?;
                    match desc.upload_transform {
                        TextureUploadTransform::B5G6R5ToRgba8 => {
                            expand_b5g6r5_unorm_to_rgba8(src_row, dst_row)
                        }
                        TextureUploadTransform::B5G5R5A1ToRgba8 => {
                            expand_b5g5r5a1_unorm_to_rgba8(src_row, dst_row)
                        }
                        _ => unreachable!(),
                    }
                }

                let padded_bytes_per_row =
                    if linear.height > 1 && !dst_tight_bpr.is_multiple_of(align) {
                        align_copy_bytes_per_row(dst_tight_bpr)
                    } else {
                        dst_tight_bpr
                    };

                let upload_bytes: std::borrow::Cow<'_, [u8]> = if padded_bytes_per_row
                    == dst_tight_bpr
                {
                    std::borrow::Cow::Owned(rgba)
                } else {
                    let mut tmp = vec![0u8; padded_bytes_per_row as usize * linear.height as usize];
                    for y in 0..linear.height as usize {
                        let src_start = y * dst_tight_bpr as usize;
                        let dst_start = y * padded_bytes_per_row as usize;
                        tmp[dst_start..dst_start + dst_tight_bpr as usize]
                            .copy_from_slice(&rgba[src_start..src_start + dst_tight_bpr as usize]);
                    }
                    std::borrow::Cow::Owned(tmp)
                };

                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &upload_bytes,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(linear.height),
                    },
                    wgpu::Extent3d {
                        width: linear.width,
                        height: linear.height,
                        depth_or_array_layers: 1,
                    },
                );
            } else {
                // Direct upload (uncompressed or BC, depending on the texture format).
                if desc.format.is_bc_compressed() {
                    // WebGPU requires BC texture copy regions to be 4x4 block aligned unless the
                    // copy reaches the edge of the mip. This upload path always writes full mips at
                    // origin (0,0), but we still validate alignment so future refactors that add
                    // partial updates don't accidentally violate the rule.
                    validate_bc_region_alignment(
                        0,
                        0,
                        linear.width,
                        linear.height,
                        linear.width,
                        linear.height,
                    )?;
                }

                let needs_repack = linear.rows > 1 && !linear.row_pitch_bytes.is_multiple_of(align);

                let upload_bytes_per_row = if needs_repack {
                    align_copy_bytes_per_row(linear.unpadded_bytes_per_row)
                } else {
                    linear.row_pitch_bytes
                };

                let upload_bytes: std::borrow::Cow<'_, [u8]> = if needs_repack {
                    let mut tmp = vec![0u8; upload_bytes_per_row as usize * linear.rows as usize];
                    for y in 0..linear.rows as usize {
                        let src_start = y * linear.row_pitch_bytes as usize;
                        let dst_start = y * upload_bytes_per_row as usize;
                        tmp[dst_start..dst_start + linear.unpadded_bytes_per_row as usize]
                            .copy_from_slice(
                                &data
                                    [src_start..src_start + linear.unpadded_bytes_per_row as usize],
                            );
                    }
                    std::borrow::Cow::Owned(tmp)
                } else {
                    std::borrow::Cow::Borrowed(data)
                };

                let (extent_width, extent_height) =
                    match wgpu_format_layout_info(desc.texture_format)? {
                        TextureFormatLayout::BlockCompressed {
                            block_width,
                            block_height,
                            ..
                        } => {
                            // WebGPU requires BC uploads to use the physical (block-rounded) size, even
                            // when the mip itself is smaller than a full block.
                            let w = linear
                                .width
                                .div_ceil(block_width)
                                .checked_mul(block_width)
                                .ok_or_else(|| {
                                    anyhow!("texture upload extent width overflows u32")
                                })?;
                            let h = linear
                                .height
                                .div_ceil(block_height)
                                .checked_mul(block_height)
                                .ok_or_else(|| {
                                    anyhow!("texture upload extent height overflows u32")
                                })?;
                            (w, h)
                        }
                        TextureFormatLayout::Uncompressed { .. } => (linear.width, linear.height),
                    };

                queue.write_texture(
                    wgpu::ImageCopyTexture {
                        texture,
                        mip_level: mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: layer,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    &upload_bytes,
                    wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(upload_bytes_per_row),
                        rows_per_image: Some(linear.rows),
                    },
                    wgpu::Extent3d {
                        width: extent_width,
                        height: extent_height,
                        depth_or_array_layers: 1,
                    },
                );
            }

            offset += subresource_len;
        }
    }

    Ok(())
}

fn validate_bc_region_alignment(
    origin_x: u32,
    origin_y: u32,
    width: u32,
    height: u32,
    mip_width: u32,
    mip_height: u32,
) -> Result<()> {
    // Origin must be block-aligned.
    if !origin_x.is_multiple_of(4) || !origin_y.is_multiple_of(4) {
        bail!("BC-compressed writes must be 4x4 block aligned (origin=({origin_x},{origin_y}))");
    }

    // Copy size must be block-aligned unless reaching the edge of the mip.
    if (!width.is_multiple_of(4) && origin_x + width != mip_width)
        || (!height.is_multiple_of(4) && origin_y + height != mip_height)
    {
        bail!(
            "BC-compressed writes must be 4x4 block aligned unless reaching the mip edge (origin=({origin_x},{origin_y}) extent=({width},{height}) mip_size=({mip_width},{mip_height}))"
        );
    }

    Ok(())
}

const FNV1A64_OFFSET_BASIS: u64 = 14695981039346656037;
const FNV1A64_PRIME: u64 = 1099511628211;

fn fnv1a64_update(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= b as u64;
        *hash = hash.wrapping_mul(FNV1A64_PRIME);
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A64_OFFSET_BASIS;
    fnv1a64_update(&mut hash, bytes);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_STORAGE;
    use crate::input_layout::{AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION};
    use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_RESOURCE_USAGE_STORAGE;

    #[test]
    fn maps_aerogpu_formats() {
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B8G8R8A8Unorm as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::R8G8B8A8Unorm as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8Unorm)
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::R8G8B8A8UnormSrgb as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B8G8R8A8UnormSrgb as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bgra8UnormSrgb)
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B5G6R5Unorm as u32).unwrap(),
            LinearTextureFormat::B5G6R5Unorm
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B5G5R5A1Unorm as u32).unwrap(),
            LinearTextureFormat::B5G5R5A1Unorm
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::BC1RgbaUnorm as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm)
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::BC3RgbaUnormSrgb as u32).unwrap(),
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc3RgbaUnormSrgb)
        );
        assert!(map_aerogpu_format(AerogpuFormat::Invalid as u32).is_err());
    }

    #[test]
    fn maps_usage_flags_conservatively() {
        let bu = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, false);
        assert!(bu.contains(wgpu::BufferUsages::COPY_SRC));
        assert!(bu.contains(wgpu::BufferUsages::COPY_DST));
        assert!(bu.contains(wgpu::BufferUsages::VERTEX));
        assert!(!bu.contains(wgpu::BufferUsages::STORAGE));

        let storage = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_STORAGE, false);
        assert!(!storage.contains(wgpu::BufferUsages::STORAGE));

        let bu_compute = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER, true);
        assert!(bu_compute.contains(wgpu::BufferUsages::STORAGE));

        let ib = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, false);
        assert!(ib.contains(wgpu::BufferUsages::INDEX));
        assert!(!ib.contains(wgpu::BufferUsages::STORAGE));

        let ib_compute = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_INDEX_BUFFER, true);
        assert!(ib_compute.contains(wgpu::BufferUsages::STORAGE));

        let tu = map_texture_usage_flags(AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
        assert!(tu.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(tu.contains(wgpu::TextureUsages::COPY_DST));
        assert!(tu.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
    }

    #[test]
    fn map_buffer_usage_flags_includes_storage_for_uav_srv_buffers() {
        // SM5 compute shaders translate raw/structured buffers into `var<storage>` bindings in WGSL.
        // wgpu validates that any buffer used in a storage binding was created with
        // `wgpu::BufferUsages::STORAGE`, even for read-only storage buffers (SRVs).
        assert!(!map_buffer_usage_flags(0, false).contains(wgpu::BufferUsages::STORAGE));
        assert!(map_buffer_usage_flags(0, true).contains(wgpu::BufferUsages::STORAGE));
    }

    #[test]
    fn validates_byte_ranges() {
        validate_range_in_resource(
            DirtyRange {
                offset_bytes: 0,
                size_bytes: 4,
            },
            4,
        )
        .unwrap();
        assert!(validate_range_in_resource(
            DirtyRange {
                offset_bytes: 4,
                size_bytes: 1,
            },
            4
        )
        .is_err());
    }

    #[test]
    fn computes_texture_total_size() {
        let desc = Texture2dDesc {
            width: 4,
            height: 4,
            mip_levels: 2,
            array_layers: 1,
            format: LinearTextureFormat::Wgpu(wgpu::TextureFormat::Rgba8Unorm),
            texture_format: wgpu::TextureFormat::Rgba8Unorm,
            row_pitch_bytes: 0,
            upload_transform: TextureUploadTransform::Direct,
        };
        // mip0: 4*4*4 = 64, mip1: 2*2*4 = 16
        assert_eq!(texture_total_size_bytes(&desc).unwrap(), 80);
    }

    #[test]
    fn computes_bc_texture_total_size() {
        // BC1 has 8-byte blocks. 4x4 with a full mip chain:
        // mip0: 1 block = 8
        // mip1: 1 block = 8
        // mip2: 1 block = 8
        let bc1 = Texture2dDesc {
            width: 4,
            height: 4,
            mip_levels: 3,
            array_layers: 1,
            format: LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            texture_format: wgpu::TextureFormat::Bc1RgbaUnorm,
            row_pitch_bytes: 0,
            upload_transform: TextureUploadTransform::Direct,
        };
        assert_eq!(texture_total_size_bytes(&bc1).unwrap(), 24);

        // BC3 has 16-byte blocks.
        let bc3 = Texture2dDesc {
            width: 4,
            height: 4,
            mip_levels: 3,
            array_layers: 1,
            format: LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc3RgbaUnorm),
            texture_format: wgpu::TextureFormat::Bc3RgbaUnorm,
            row_pitch_bytes: 0,
            upload_transform: TextureUploadTransform::Direct,
        };
        assert_eq!(texture_total_size_bytes(&bc3).unwrap(), 48);
    }

    #[test]
    fn bc_dimension_compatibility_requires_block_aligned_base_mip() {
        assert!(!wgpu_compressed_texture_dimensions_compatible(
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            2,
            2,
            1
        )
        .unwrap());
        assert!(wgpu_compressed_texture_dimensions_compatible(
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            4,
            4,
            2
        )
        .unwrap());
    }

    #[test]
    fn bc_dimension_compatibility_rejects_mip_levels_beyond_possible_chain_length() {
        // WebGPU does not allow mip_level_count to exceed the number of distinct mip extents.
        assert!(!wgpu_compressed_texture_dimensions_compatible(
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            4,
            4,
            4
        )
        .unwrap());
        assert!(wgpu_compressed_texture_dimensions_compatible(
            LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            4,
            4,
            3
        )
        .unwrap());
    }

    #[test]
    fn bc_row_pitch_bytes_only_applies_to_mip0() {
        // BC1 4x4 mip chain with an explicitly padded row_pitch for mip0:
        // mip0: row_pitch=16, rows=1 -> 16
        // mip1: tight 8
        // mip2: tight 8
        let desc = Texture2dDesc {
            width: 4,
            height: 4,
            mip_levels: 3,
            array_layers: 1,
            format: LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
            texture_format: wgpu::TextureFormat::Bc1RgbaUnorm,
            row_pitch_bytes: 16,
            upload_transform: TextureUploadTransform::Direct,
        };
        assert_eq!(texture_total_size_bytes(&desc).unwrap(), 32);
    }

    async fn create_device_queue(
        required_features: wgpu::Features,
    ) -> Result<Option<(wgpu::Device, wgpu::Queue)>> {
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
        };

        let Some(adapter) = adapter else {
            return Ok(None);
        };

        if !adapter.features().contains(required_features) {
            return Ok(None);
        }

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu_resources test device"),
                    required_features,
                    required_limits: wgpu::Limits::downlevel_defaults(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("wgpu: request_device failed: {e:?}"))?;

        Ok(Some((device, queue)))
    }

    async fn read_texture_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width
            .checked_mul(bytes_per_pixel)
            .ok_or_else(|| anyhow!("bytes_per_row overflow"))?;
        let padded_bytes_per_row = align_copy_bytes_per_row(unpadded_bytes_per_row);
        let buffer_size = padded_bytes_per_row as u64 * height as u64;

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aerogpu_resources bc read_texture staging"),
            size: buffer_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("aerogpu_resources bc read_texture encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &staging,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        slice.map_async(wgpu::MapMode::Read, move |v| {
            sender.send(v).ok();
        });
        #[cfg(not(target_arch = "wasm32"))]
        device.poll(wgpu::Maintain::Wait);

        #[cfg(target_arch = "wasm32")]
        device.poll(wgpu::Maintain::Poll);

        receiver
            .receive()
            .await
            .ok_or_else(|| anyhow!("wgpu: map_async dropped"))?
            .context("wgpu: map_async failed")?;

        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            out.extend_from_slice(&mapped[start..start + unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    #[test]
    fn upload_bc1_texture_with_cpu_decompression() -> Result<()> {
        pollster::block_on(async {
            let Some((device, queue)) = create_device_queue(wgpu::Features::empty()).await? else {
                return Ok(());
            };

            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("aerogpu_resources bc1 test texture"),
                size: wgpu::Extent3d {
                    width: 4,
                    height: 8,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });

            // A single BC1 block with a known output pattern (from aero_gpu::bc_decompress tests).
            let bc1_block: [u8; 8] = [
                0xff, 0xff, // color0 (white)
                0x00, 0x00, // color1 (black)
                0x00, 0x55, 0xaa, 0xff, // indices
            ];

            // 4x8 BC1 has 1x2 blocks. Use a padded row_pitch to exercise repacking:
            //   row_pitch=12 = 8 bytes block + 4 bytes padding.
            let bytes: Vec<u8> = [
                bc1_block.as_slice(),
                &[0u8; 4],
                bc1_block.as_slice(),
                &[0u8; 4],
            ]
            .concat();

            let desc = Texture2dDesc {
                width: 4,
                height: 8,
                mip_levels: 1,
                array_layers: 1,
                format: LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm),
                texture_format: wgpu::TextureFormat::Rgba8Unorm,
                row_pitch_bytes: 12,
                upload_transform: TextureUploadTransform::Bc1ToRgba8,
            };

            upload_texture_from_linear_bytes(&queue, &texture, &desc, &bytes)?;

            let readback = read_texture_rgba8(&device, &queue, &texture, 4, 8).await?;

            let row0 = [255u8, 255, 255, 255].repeat(4);
            let row1 = [0u8, 0, 0, 255].repeat(4);
            let row2 = [170u8, 170, 170, 255].repeat(4);
            let row3 = [85u8, 85, 85, 255].repeat(4);
            let mut expected = Vec::new();
            for _ in 0..2 {
                expected.extend_from_slice(&row0);
                expected.extend_from_slice(&row1);
                expected.extend_from_slice(&row2);
                expected.extend_from_slice(&row3);
            }

            assert_eq!(readback, expected);
            Ok(())
        })
    }

    #[test]
    fn create_texture2d_bc_falls_back_when_dimensions_not_block_aligned_even_if_bc_enabled(
    ) -> Result<()> {
        pollster::block_on(async {
            let Some((device, queue)) =
                create_device_queue(wgpu::Features::TEXTURE_COMPRESSION_BC).await?
            else {
                // Adapter/device does not support BC compression; nothing to validate here.
                return Ok(());
            };

            let mut mgr = AerogpuResourceManager::new(device, queue);
            mgr.create_texture2d(
                1,
                Texture2dCreateDesc {
                    usage_flags: AEROGPU_RESOURCE_USAGE_TEXTURE,
                    format: AerogpuFormat::BC1RgbaUnorm as u32,
                    width: 9,
                    height: 9,
                    mip_levels: 1,
                    array_layers: 1,
                    row_pitch_bytes: 0,
                    backing_alloc_id: 0,
                    backing_offset_bytes: 0,
                },
            )?;

            let tex = mgr.texture2d(1)?;
            assert_eq!(
                tex.desc.format,
                LinearTextureFormat::Wgpu(wgpu::TextureFormat::Bc1RgbaUnorm)
            );
            assert_eq!(tex.desc.texture_format, wgpu::TextureFormat::Rgba8Unorm);
            assert_eq!(
                tex.desc.upload_transform,
                TextureUploadTransform::Bc1ToRgba8
            );
            Ok(())
        })
    }

    #[test]
    fn parses_input_layout_blob_v1() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_MAGIC.to_le_bytes());
        blob.extend_from_slice(&AEROGPU_INPUT_LAYOUT_BLOB_VERSION.to_le_bytes());
        blob.extend_from_slice(&1u32.to_le_bytes()); // element_count
        blob.extend_from_slice(&0u32.to_le_bytes()); // reserved0
                                                     // element
        blob.extend_from_slice(&123u32.to_le_bytes()); // semantic_name_hash
        blob.extend_from_slice(&2u32.to_le_bytes()); // semantic_index
        blob.extend_from_slice(&28u32.to_le_bytes()); // dxgi_format
        blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot
        blob.extend_from_slice(&0u32.to_le_bytes()); // aligned_byte_offset
        blob.extend_from_slice(&0u32.to_le_bytes()); // input_slot_class
        blob.extend_from_slice(&0u32.to_le_bytes()); // instance_data_step_rate

        let desc = InputLayoutDesc::parse(&blob).unwrap();
        assert_eq!(desc.header.magic, AEROGPU_INPUT_LAYOUT_BLOB_MAGIC);
        assert_eq!(desc.header.version, AEROGPU_INPUT_LAYOUT_BLOB_VERSION);
        assert_eq!(desc.header.element_count, 1);
        assert_eq!(desc.elements.len(), 1);
        assert_eq!(desc.elements[0].semantic_name_hash, 123);
        assert_eq!(desc.elements[0].dxgi_format, 28);
    }
}
