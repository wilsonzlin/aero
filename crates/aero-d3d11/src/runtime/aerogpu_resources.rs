use std::collections::HashMap;

use aero_gpu::{decompress_bc1_rgba8, decompress_bc2_rgba8, decompress_bc3_rgba8, decompress_bc7_rgba8};
use aero_gpu::guest_memory::GuestMemory;
use aero_protocol::aerogpu::aerogpu_cmd::{
    AerogpuHandle, AerogpuShaderStage, AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER,
    AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL, AEROGPU_RESOURCE_USAGE_INDEX_BUFFER,
    AEROGPU_RESOURCE_USAGE_RENDER_TARGET, AEROGPU_RESOURCE_USAGE_SCANOUT,
    AEROGPU_RESOURCE_USAGE_TEXTURE, AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER,
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
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
    /// Format of the texture data in the linear backing store (guest allocations and UPLOAD_RESOURCE
    /// payloads).
    pub format: wgpu::TextureFormat,
    /// Actual format of the host `wgpu::Texture`.
    ///
    /// This differs from [`Self::format`] when the guest requests a BC-compressed format but the
    /// device does not have `TEXTURE_COMPRESSION_BC` enabled; in that case we fall back to an
    /// RGBA8 texture and decompress BC blocks on upload.
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

    buffers: HashMap<AerogpuHandle, BufferResource>,
    textures2d: HashMap<AerogpuHandle, Texture2dResource>,
    shaders: HashMap<AerogpuHandle, ShaderResource>,
    input_layouts: HashMap<AerogpuHandle, InputLayoutResource>,
}

impl AerogpuResourceManager {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self {
            device,
            queue,
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
            usage: map_buffer_usage_flags(usage_flags),
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
        if backing_alloc_id != 0 && row_pitch_bytes == 0 {
            bail!("CreateTexture2d: row_pitch_bytes is required for allocation-backed textures");
        }

        let linear_format = map_aerogpu_format(format)?;
        let bc_enabled = self
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
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
            x if x == AerogpuShaderStage::Vertex as u32 => AerogpuShaderStage::Vertex,
            x if x == AerogpuShaderStage::Pixel as u32 => AerogpuShaderStage::Pixel,
            x if x == AerogpuShaderStage::Compute as u32 => AerogpuShaderStage::Compute,
            _ => bail!("CreateShaderDxbc: unknown aerogpu_shader_stage {stage}"),
        };

        let dxbc_hash_fnv1a64 = fnv1a64(dxbc_bytes);

        let dxbc = DxbcFile::parse(dxbc_bytes).context("parse DXBC container")?;
        let program = Sm4Program::parse_from_dxbc(&dxbc).context("parse SM4/SM5 program")?;
        let parsed_stage = match program.stage {
            ShaderStage::Vertex => AerogpuShaderStage::Vertex,
            ShaderStage::Pixel => AerogpuShaderStage::Pixel,
            ShaderStage::Compute => AerogpuShaderStage::Compute,
            // AeroGPU's shader stage ABI matches WebGPU, which has no geometry/hull/domain stage.
            // Some Win7-era D3D11 apps still create these shaders (or buggy guests may forward them
            // with a placeholder stage). Accept the create to keep the runtime robust, but ignore
            // the shader since there is no way to bind it in the command stream.
            ShaderStage::Geometry | ShaderStage::Hull | ShaderStage::Domain => {
                return Ok(());
            }
            other => bail!("CreateShaderDxbc: unsupported DXBC shader stage {other:?}"),
        };
        if parsed_stage != stage {
            bail!("CreateShaderDxbc: stage mismatch (cmd={stage:?}, dxbc={parsed_stage:?})");
        }
        let signatures = parse_signatures(&dxbc).context("parse DXBC signatures")?;
        let wgsl = if signatures.isgn.is_some() && signatures.osgn.is_some() {
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

        let reflection = build_shader_reflection(stage, &signatures);

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
        // since the AeroGPU/WebGPU pipeline has no slot for them. In those cases we never insert a
        // shader resource, so a later destroy should be a no-op rather than a hard error.
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
    let module = program.decode().context("decode SM4/5 token stream")?;
    let translated = translate_sm4_module_to_wgsl(dxbc, &module, signatures)
        .context("signature-driven SM4/5 translation")?;
    Ok(translated.wgsl)
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

pub fn map_aerogpu_format(format: u32) -> Result<wgpu::TextureFormat> {
    Ok(match format {
        x if x == AerogpuFormat::B8G8R8A8Unorm as u32 => wgpu::TextureFormat::Bgra8Unorm,
        x if x == AerogpuFormat::B8G8R8X8Unorm as u32 => wgpu::TextureFormat::Bgra8Unorm,
        x if x == AerogpuFormat::R8G8B8A8Unorm as u32 => wgpu::TextureFormat::Rgba8Unorm,
        x if x == AerogpuFormat::R8G8B8X8Unorm as u32 => wgpu::TextureFormat::Rgba8Unorm,
        // sRGB formats (AeroGPU protocol extensions; values align with common DXGI_FORMAT
        // discriminants for forward-compatibility).
        29 => wgpu::TextureFormat::Rgba8UnormSrgb, // DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
        91 | 93 => wgpu::TextureFormat::Bgra8UnormSrgb, // DXGI_FORMAT_B8G8R8A8_UNORM_SRGB / B8G8R8X8_UNORM_SRGB
        // BC formats (DXGI_FORMAT_BC* numeric values).
        71 => wgpu::TextureFormat::Bc1RgbaUnorm,
        72 => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
        74 => wgpu::TextureFormat::Bc2RgbaUnorm,
        75 => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
        77 => wgpu::TextureFormat::Bc3RgbaUnorm,
        78 => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
        98 => wgpu::TextureFormat::Bc7RgbaUnorm,
        99 => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
        x if x == AerogpuFormat::D24UnormS8Uint as u32 => wgpu::TextureFormat::Depth24PlusStencil8,
        x if x == AerogpuFormat::D32Float as u32 => wgpu::TextureFormat::Depth32Float,
        _ => bail!("unsupported aerogpu_format {format}"),
    })
}

pub fn map_buffer_usage_flags(usage_flags: u32) -> wgpu::BufferUsages {
    let mut out = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
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
    Uncompressed { bytes_per_texel: u32 },
    BlockCompressed {
        block_width: u32,
        block_height: u32,
        bytes_per_block: u32,
    },
}

fn format_layout_info(format: wgpu::TextureFormat) -> Result<TextureFormatLayout> {
    Ok(match format {
        wgpu::TextureFormat::Bgra8Unorm
        | wgpu::TextureFormat::Bgra8UnormSrgb
        | wgpu::TextureFormat::Rgba8Unorm
        | wgpu::TextureFormat::Rgba8UnormSrgb
        | wgpu::TextureFormat::Depth24PlusStencil8
        | wgpu::TextureFormat::Depth32Float => TextureFormatLayout::Uncompressed { bytes_per_texel: 4 },

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
    requested: wgpu::TextureFormat,
    bc_enabled: bool,
) -> Result<(wgpu::TextureFormat, TextureUploadTransform)> {
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

    Ok((fallback, transform))
}

fn mip_extent(v: u32, level: u32) -> u32 {
    (v >> level).max(1)
}

fn align_copy_bytes_per_row(bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    bytes_per_row.div_ceil(align) * align
}

fn texture_unpadded_bytes_per_row(format: wgpu::TextureFormat, width_texels: u32) -> Result<u32> {
    let info = format_layout_info(format)?;
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
    let info = format_layout_info(desc.format)?;

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
                let expected_bc_len: usize = match format_layout_info(desc.format)? {
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
                    TextureUploadTransform::Direct => unreachable!(),
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
                let padded_bytes_per_row = if linear.height > 1
                    && !unpadded_bytes_per_row.is_multiple_of(align)
                {
                    align_copy_bytes_per_row(unpadded_bytes_per_row)
                } else {
                    unpadded_bytes_per_row
                };

                let upload_bytes: std::borrow::Cow<'_, [u8]> =
                    if padded_bytes_per_row == unpadded_bytes_per_row {
                        std::borrow::Cow::Owned(rgba)
                    } else {
                        let mut tmp = vec![
                            0u8;
                            padded_bytes_per_row as usize * linear.height as usize
                        ];
                        for y in 0..linear.height as usize {
                            let src_start = y * unpadded_bytes_per_row as usize;
                            let dst_start = y * padded_bytes_per_row as usize;
                            tmp[dst_start..dst_start + unpadded_bytes_per_row as usize]
                                .copy_from_slice(
                                    &rgba[src_start
                                        ..src_start + unpadded_bytes_per_row as usize],
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
            } else {
                // Direct upload (uncompressed or BC, depending on the texture format).
                if is_bc_compressed_format(desc.format) {
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
                    let mut tmp =
                        vec![0u8; upload_bytes_per_row as usize * linear.rows as usize];
                    for y in 0..linear.rows as usize {
                        let src_start = y * linear.row_pitch_bytes as usize;
                        let dst_start = y * upload_bytes_per_row as usize;
                        tmp[dst_start..dst_start + linear.unpadded_bytes_per_row as usize]
                            .copy_from_slice(
                                &data[src_start
                                    ..src_start + linear.unpadded_bytes_per_row as usize],
                            );
                    }
                    std::borrow::Cow::Owned(tmp)
                } else {
                    std::borrow::Cow::Borrowed(data)
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
                        width: linear.width,
                        height: linear.height,
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
        bail!(
            "BC-compressed writes must be 4x4 block aligned (origin=({origin_x},{origin_y}))"
        );
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
    use crate::input_layout::{AEROGPU_INPUT_LAYOUT_BLOB_MAGIC, AEROGPU_INPUT_LAYOUT_BLOB_VERSION};

    #[test]
    fn maps_aerogpu_formats() {
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::B8G8R8A8Unorm as u32).unwrap(),
            wgpu::TextureFormat::Bgra8Unorm
        );
        assert_eq!(
            map_aerogpu_format(AerogpuFormat::R8G8B8A8Unorm as u32).unwrap(),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            map_aerogpu_format(29).unwrap(),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            map_aerogpu_format(91).unwrap(),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
        assert_eq!(
            map_aerogpu_format(71).unwrap(),
            wgpu::TextureFormat::Bc1RgbaUnorm
        );
        assert_eq!(
            map_aerogpu_format(78).unwrap(),
            wgpu::TextureFormat::Bc3RgbaUnormSrgb
        );
        assert!(map_aerogpu_format(AerogpuFormat::Invalid as u32).is_err());
    }

    #[test]
    fn maps_usage_flags_conservatively() {
        let bu = map_buffer_usage_flags(AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER);
        assert!(bu.contains(wgpu::BufferUsages::COPY_SRC));
        assert!(bu.contains(wgpu::BufferUsages::COPY_DST));
        assert!(bu.contains(wgpu::BufferUsages::VERTEX));

        let tu = map_texture_usage_flags(AEROGPU_RESOURCE_USAGE_RENDER_TARGET);
        assert!(tu.contains(wgpu::TextureUsages::COPY_SRC));
        assert!(tu.contains(wgpu::TextureUsages::COPY_DST));
        assert!(tu.contains(wgpu::TextureUsages::RENDER_ATTACHMENT));
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
            format: wgpu::TextureFormat::Rgba8Unorm,
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
            format: wgpu::TextureFormat::Bc1RgbaUnorm,
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
            format: wgpu::TextureFormat::Bc3RgbaUnorm,
            texture_format: wgpu::TextureFormat::Bc3RgbaUnorm,
            row_pitch_bytes: 0,
            upload_transform: TextureUploadTransform::Direct,
        };
        assert_eq!(texture_total_size_bytes(&bc3).unwrap(), 48);
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
            format: wgpu::TextureFormat::Bc1RgbaUnorm,
            texture_format: wgpu::TextureFormat::Bc1RgbaUnorm,
            row_pitch_bytes: 16,
            upload_transform: TextureUploadTransform::Direct,
        };
        assert_eq!(texture_total_size_bytes(&desc).unwrap(), 32);
    }

    async fn create_device_queue() -> Result<Option<(wgpu::Device, wgpu::Queue)>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let needs_runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(|v| v.is_empty())
                .unwrap_or(true);

            if needs_runtime_dir {
                let dir =
                    std::env::temp_dir().join(format!("aero-d3d11-xdg-runtime-{}", std::process::id()));
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

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("aero-d3d11 aerogpu_resources test device"),
                    required_features: wgpu::Features::empty(),
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
            let Some((device, queue)) = create_device_queue().await? else {
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
                format: wgpu::TextureFormat::Bc1RgbaUnorm,
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
