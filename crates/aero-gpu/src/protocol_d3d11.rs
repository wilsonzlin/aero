//! D3D11 command protocol for AeroGPU.
//!
//! The intent is to keep this as a simple, word-addressed stream so it can be
//! written from a guest/driver context with minimal packing overhead.

use bitflags::bitflags;

pub type CmdWord = u32;
pub type ResourceId = u32;

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum D3D11Opcode {
    // Resource creation / updates.
    CreateBuffer = 0x01,
    UpdateBuffer = 0x02,
    CreateTexture2D = 0x03,
    UpdateTexture2D = 0x04,
    CreateTextureView = 0x05,
    CreateSampler = 0x06,
    CreateShaderModuleWgsl = 0x07,
    CreateRenderPipeline = 0x08,
    CreateComputePipeline = 0x09,

    // State binding.
    SetPipeline = 0x10,
    SetVertexBuffer = 0x11,
    SetIndexBuffer = 0x12,
    SetBindBuffer = 0x13,
    SetBindSampler = 0x14,
    SetBindTextureView = 0x15,

    // Pass control.
    BeginRenderPass = 0x20,
    EndRenderPass = 0x21,
    BeginComputePass = 0x22,
    EndComputePass = 0x23,

    // Commands.
    Draw = 0x30,
    DrawIndexed = 0x31,
    Dispatch = 0x32,
    CopyBufferToBuffer = 0x33,
}

impl D3D11Opcode {
    pub fn from_word(word: CmdWord) -> Option<Self> {
        Some(match word {
            x if x == Self::CreateBuffer as CmdWord => Self::CreateBuffer,
            x if x == Self::UpdateBuffer as CmdWord => Self::UpdateBuffer,
            x if x == Self::CreateTexture2D as CmdWord => Self::CreateTexture2D,
            x if x == Self::UpdateTexture2D as CmdWord => Self::UpdateTexture2D,
            x if x == Self::CreateTextureView as CmdWord => Self::CreateTextureView,
            x if x == Self::CreateSampler as CmdWord => Self::CreateSampler,
            x if x == Self::CreateShaderModuleWgsl as CmdWord => Self::CreateShaderModuleWgsl,
            x if x == Self::CreateRenderPipeline as CmdWord => Self::CreateRenderPipeline,
            x if x == Self::CreateComputePipeline as CmdWord => Self::CreateComputePipeline,
            x if x == Self::SetPipeline as CmdWord => Self::SetPipeline,
            x if x == Self::SetVertexBuffer as CmdWord => Self::SetVertexBuffer,
            x if x == Self::SetIndexBuffer as CmdWord => Self::SetIndexBuffer,
            x if x == Self::SetBindBuffer as CmdWord => Self::SetBindBuffer,
            x if x == Self::SetBindSampler as CmdWord => Self::SetBindSampler,
            x if x == Self::SetBindTextureView as CmdWord => Self::SetBindTextureView,
            x if x == Self::BeginRenderPass as CmdWord => Self::BeginRenderPass,
            x if x == Self::EndRenderPass as CmdWord => Self::EndRenderPass,
            x if x == Self::BeginComputePass as CmdWord => Self::BeginComputePass,
            x if x == Self::EndComputePass as CmdWord => Self::EndComputePass,
            x if x == Self::Draw as CmdWord => Self::Draw,
            x if x == Self::DrawIndexed as CmdWord => Self::DrawIndexed,
            x if x == Self::Dispatch as CmdWord => Self::Dispatch,
            x if x == Self::CopyBufferToBuffer as CmdWord => Self::CopyBufferToBuffer,
            _ => return None,
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct CmdHeader {
    pub opcode: D3D11Opcode,
    /// Payload length in `CmdWord`s (excluding the header).
    pub payload_words: u32,
}

#[derive(Debug)]
pub struct CmdPacket<'a> {
    pub header: CmdHeader,
    pub payload: &'a [CmdWord],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdParseError {
    TruncatedHeader {
        at_word: usize,
    },
    UnknownOpcode {
        opcode: CmdWord,
        at_word: usize,
    },
    TruncatedPayload {
        opcode: D3D11Opcode,
        expected_words: usize,
        remaining_words: usize,
        at_word: usize,
    },
}

impl std::fmt::Display for CmdParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdParseError::TruncatedHeader { at_word } => {
                write!(f, "truncated command header at word {at_word}")
            }
            CmdParseError::UnknownOpcode { opcode, at_word } => {
                write!(f, "unknown D3D11 opcode {opcode:#x} at word {at_word}")
            }
            CmdParseError::TruncatedPayload {
                opcode,
                expected_words,
                remaining_words,
                at_word,
            } => write!(
                f,
                "truncated payload for opcode {opcode:?} at word {at_word}: expected {expected_words} words, only {remaining_words} remaining"
            ),
        }
    }
}

impl std::error::Error for CmdParseError {}

pub struct CmdStream<'a> {
    words: &'a [CmdWord],
    cursor: usize,
}

impl<'a> CmdStream<'a> {
    pub fn new(words: &'a [CmdWord]) -> Self {
        Self { words, cursor: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.cursor >= self.words.len()
    }
}

impl<'a> Iterator for CmdStream<'a> {
    type Item = Result<CmdPacket<'a>, CmdParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.words.len() {
            return None;
        }
        if self.cursor + 2 > self.words.len() {
            return Some(Err(CmdParseError::TruncatedHeader {
                at_word: self.cursor,
            }));
        }

        let opcode_word = self.words[self.cursor];
        let payload_words = self.words[self.cursor + 1] as usize;
        let opcode = match D3D11Opcode::from_word(opcode_word) {
            Some(op) => op,
            None => {
                return Some(Err(CmdParseError::UnknownOpcode {
                    opcode: opcode_word,
                    at_word: self.cursor,
                }))
            }
        };

        let payload_start = self.cursor + 2;
        let payload_end = match payload_start.checked_add(payload_words) {
            Some(end) => end,
            None => {
                // Defensive: on 32-bit targets a u32 payload_words can overflow usize arithmetic.
                return Some(Err(CmdParseError::TruncatedPayload {
                    opcode,
                    expected_words: payload_words,
                    remaining_words: self.words.len().saturating_sub(payload_start),
                    at_word: self.cursor,
                }));
            }
        };
        if payload_end > self.words.len() {
            return Some(Err(CmdParseError::TruncatedPayload {
                opcode,
                expected_words: payload_words,
                remaining_words: self.words.len().saturating_sub(payload_start),
                at_word: self.cursor,
            }));
        }

        self.cursor = payload_end;
        Some(Ok(CmdPacket {
            header: CmdHeader {
                opcode,
                payload_words: payload_words as u32,
            },
            payload: &self.words[payload_start..payload_end],
        }))
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
    pub struct ShaderStageFlags: u32 {
        const VERTEX = 1 << 0;
        const FRAGMENT = 1 << 1;
        const COMPUTE = 1 << 2;
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
    pub struct BufferUsage: u32 {
        const MAP_READ = 1 << 0;
        const MAP_WRITE = 1 << 1;
        const COPY_SRC = 1 << 2;
        const COPY_DST = 1 << 3;
        const INDEX = 1 << 4;
        const VERTEX = 1 << 5;
        const UNIFORM = 1 << 6;
        const STORAGE = 1 << 7;
        const INDIRECT = 1 << 8;
    }
}

bitflags! {
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
    pub struct TextureUsage: u32 {
        const COPY_SRC = 1 << 0;
        const COPY_DST = 1 << 1;
        const TEXTURE_BINDING = 1 << 2;
        const STORAGE_BINDING = 1 << 3;
        const RENDER_ATTACHMENT = 1 << 4;
    }
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum DxgiFormat {
    Unknown = 0,
    R32G32B32A32Float = 2,
    R16G16B16A16Float = 10,
    R8G8B8A8Unorm = 28,
    R8G8B8A8UnormSrgb = 29,
    R32Float = 41,
    D32Float = 40,
    D24UnormS8Uint = 45,
    B8G8R8A8Unorm = 87,
    B8G8R8A8UnormSrgb = 91,
}

impl DxgiFormat {
    pub fn from_word(word: CmdWord) -> Self {
        match word {
            x if x == Self::R32G32B32A32Float as CmdWord => Self::R32G32B32A32Float,
            x if x == Self::R16G16B16A16Float as CmdWord => Self::R16G16B16A16Float,
            x if x == Self::R8G8B8A8Unorm as CmdWord => Self::R8G8B8A8Unorm,
            x if x == Self::R8G8B8A8UnormSrgb as CmdWord => Self::R8G8B8A8UnormSrgb,
            x if x == Self::R32Float as CmdWord => Self::R32Float,
            x if x == Self::D32Float as CmdWord => Self::D32Float,
            x if x == Self::D24UnormS8Uint as CmdWord => Self::D24UnormS8Uint,
            x if x == Self::B8G8R8A8Unorm as CmdWord => Self::B8G8R8A8Unorm,
            x if x == Self::B8G8R8A8UnormSrgb as CmdWord => Self::B8G8R8A8UnormSrgb,
            _ => Self::Unknown,
        }
    }
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PrimitiveTopology {
    TriangleList = 0,
    TriangleStrip = 1,
    LineList = 2,
    LineStrip = 3,
    PointList = 4,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VertexStepMode {
    Vertex = 0,
    Instance = 1,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum VertexFormat {
    Float32x2 = 0,
    Float32x3 = 1,
    Float32x4 = 2,
    Uint32 = 3,
    Uint32x2 = 4,
    Uint32x4 = 5,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum IndexFormat {
    Uint16 = 0,
    Uint32 = 1,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PipelineKind {
    Render = 0,
    Compute = 1,
}

#[repr(u32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum BindingType {
    UniformBuffer = 0,
    StorageBufferReadOnly = 1,
    StorageBufferReadWrite = 2,
    Sampler = 3,
    Texture2D = 4,
    StorageTexture2DWriteOnly = 5,
}

#[derive(Default)]
pub struct CmdWriter {
    words: Vec<CmdWord>,
}

impl CmdWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> Vec<CmdWord> {
        self.words
    }

    fn push_cmd(&mut self, opcode: D3D11Opcode, payload: &[CmdWord]) {
        self.words.push(opcode as CmdWord);
        self.words.push(payload.len() as CmdWord);
        self.words.extend_from_slice(payload);
    }

    fn push_cmd_with_bytes(
        &mut self,
        opcode: D3D11Opcode,
        fixed_payload: &[CmdWord],
        bytes: &[u8],
    ) {
        let byte_len = bytes.len() as u32;
        let padded_words = bytes.len().div_ceil(4) as u32;
        let payload_words = fixed_payload.len() as u32 + 1 + padded_words;

        self.words.push(opcode as CmdWord);
        self.words.push(payload_words as CmdWord);
        self.words.extend_from_slice(fixed_payload);
        self.words.push(byte_len);

        let mut i = 0;
        while i < bytes.len() {
            let mut tmp = [0u8; 4];
            let end = (i + 4).min(bytes.len());
            tmp[..(end - i)].copy_from_slice(&bytes[i..end]);
            self.words.push(u32::from_le_bytes(tmp));
            i += 4;
        }
    }

    pub fn create_buffer(&mut self, id: ResourceId, size: u64, usage: BufferUsage) {
        self.push_cmd(
            D3D11Opcode::CreateBuffer,
            &[
                id,
                (size & 0xffff_ffff) as u32,
                (size >> 32) as u32,
                usage.bits(),
            ],
        );
    }

    pub fn update_buffer(&mut self, id: ResourceId, offset: u64, data: &[u8]) {
        self.push_cmd_with_bytes(
            D3D11Opcode::UpdateBuffer,
            &[id, (offset & 0xffff_ffff) as u32, (offset >> 32) as u32],
            data,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_texture2d(&mut self, id: ResourceId, desc: Texture2dDesc) {
        self.push_cmd(
            D3D11Opcode::CreateTexture2D,
            &[
                id,
                desc.width,
                desc.height,
                desc.array_layers,
                desc.mip_level_count,
                desc.format as u32,
                desc.usage.bits(),
            ],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_texture2d(&mut self, texture_id: ResourceId, update: Texture2dUpdate<'_>) {
        self.push_cmd_with_bytes(
            D3D11Opcode::UpdateTexture2D,
            &[
                texture_id,
                update.mip_level,
                update.array_layer,
                update.width,
                update.height,
                update.bytes_per_row,
            ],
            update.data,
        );
    }

    pub fn create_texture_view(
        &mut self,
        view_id: ResourceId,
        texture_id: ResourceId,
        base_mip_level: u32,
        mip_level_count: u32,
        base_array_layer: u32,
        array_layer_count: u32,
    ) {
        self.push_cmd(
            D3D11Opcode::CreateTextureView,
            &[
                view_id,
                texture_id,
                base_mip_level,
                mip_level_count,
                base_array_layer,
                array_layer_count,
            ],
        );
    }

    pub fn create_sampler(
        &mut self,
        sampler_id: ResourceId,
        // min/mag/mip filter in one word to keep this minimal; 0 is "nearest".
        filter_mode: u32,
    ) {
        self.push_cmd(D3D11Opcode::CreateSampler, &[sampler_id, filter_mode]);
    }

    pub fn create_shader_module_wgsl(&mut self, shader_id: ResourceId, wgsl: &str) {
        self.push_cmd_with_bytes(
            D3D11Opcode::CreateShaderModuleWgsl,
            &[shader_id],
            wgsl.as_bytes(),
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_render_pipeline(
        &mut self,
        pipeline_id: ResourceId,
        desc: RenderPipelineDesc<'_>,
    ) {
        let mut payload: Vec<u32> = vec![
            pipeline_id,
            desc.vs_shader,
            desc.fs_shader,
            desc.color_format as u32,
            desc.depth_format as u32,
            desc.topology as u32,
        ];

        payload.push(desc.vertex_buffers.len() as u32);
        for vb in desc.vertex_buffers {
            payload.push(vb.array_stride);
            payload.push(vb.step_mode as u32);
            payload.push(vb.attributes.len() as u32);
            for a in vb.attributes {
                payload.push(a.shader_location);
                payload.push(a.offset);
                payload.push(a.format as u32);
            }
        }

        payload.push(desc.bindings.len() as u32);
        for b in desc.bindings {
            payload.push(b.binding);
            payload.push(b.ty as u32);
            payload.push(b.visibility.bits());
            payload.push(b.storage_texture_format.unwrap_or(DxgiFormat::Unknown) as u32);
        }

        self.push_cmd(D3D11Opcode::CreateRenderPipeline, &payload);
    }

    pub fn create_compute_pipeline(
        &mut self,
        pipeline_id: ResourceId,
        cs_shader: ResourceId,
        bindings: &[BindingDesc],
    ) {
        let mut payload: Vec<u32> = vec![pipeline_id, cs_shader];

        payload.push(bindings.len() as u32);
        for b in bindings {
            payload.push(b.binding);
            payload.push(b.ty as u32);
            payload.push(b.visibility.bits());
            payload.push(b.storage_texture_format.unwrap_or(DxgiFormat::Unknown) as u32);
        }

        self.push_cmd(D3D11Opcode::CreateComputePipeline, &payload);
    }

    pub fn set_pipeline(&mut self, kind: PipelineKind, pipeline_id: ResourceId) {
        self.push_cmd(D3D11Opcode::SetPipeline, &[kind as u32, pipeline_id]);
    }

    pub fn set_vertex_buffer(&mut self, slot: u32, buffer_id: ResourceId, offset: u64) {
        self.push_cmd(
            D3D11Opcode::SetVertexBuffer,
            &[
                slot,
                buffer_id,
                (offset & 0xffff_ffff) as u32,
                (offset >> 32) as u32,
            ],
        );
    }

    pub fn set_index_buffer(&mut self, buffer_id: ResourceId, format: IndexFormat, offset: u64) {
        self.push_cmd(
            D3D11Opcode::SetIndexBuffer,
            &[
                buffer_id,
                format as u32,
                (offset & 0xffff_ffff) as u32,
                (offset >> 32) as u32,
            ],
        );
    }

    pub fn set_bind_buffer(&mut self, binding: u32, buffer_id: ResourceId, offset: u64, size: u64) {
        self.push_cmd(
            D3D11Opcode::SetBindBuffer,
            &[
                binding,
                buffer_id,
                (offset & 0xffff_ffff) as u32,
                (offset >> 32) as u32,
                (size & 0xffff_ffff) as u32,
                (size >> 32) as u32,
            ],
        );
    }

    pub fn set_bind_sampler(&mut self, binding: u32, sampler_id: ResourceId) {
        self.push_cmd(D3D11Opcode::SetBindSampler, &[binding, sampler_id]);
    }

    pub fn set_bind_texture_view(&mut self, binding: u32, view_id: ResourceId) {
        self.push_cmd(D3D11Opcode::SetBindTextureView, &[binding, view_id]);
    }

    pub fn begin_render_pass(
        &mut self,
        color_view: ResourceId,
        clear_color_rgba: [f32; 4],
        depth_stencil_view: Option<ResourceId>,
        clear_depth: f32,
        clear_stencil: u32,
    ) {
        self.push_cmd(
            D3D11Opcode::BeginRenderPass,
            &[
                color_view,
                clear_color_rgba[0].to_bits(),
                clear_color_rgba[1].to_bits(),
                clear_color_rgba[2].to_bits(),
                clear_color_rgba[3].to_bits(),
                depth_stencil_view.unwrap_or(0),
                clear_depth.to_bits(),
                clear_stencil,
            ],
        );
    }

    pub fn end_render_pass(&mut self) {
        self.push_cmd(D3D11Opcode::EndRenderPass, &[]);
    }

    pub fn begin_compute_pass(&mut self) {
        self.push_cmd(D3D11Opcode::BeginComputePass, &[]);
    }

    pub fn end_compute_pass(&mut self) {
        self.push_cmd(D3D11Opcode::EndComputePass, &[]);
    }

    pub fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        self.push_cmd(
            D3D11Opcode::Draw,
            &[vertex_count, instance_count, first_vertex, first_instance],
        );
    }

    pub fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        base_vertex: i32,
        first_instance: u32,
    ) {
        self.push_cmd(
            D3D11Opcode::DrawIndexed,
            &[
                index_count,
                instance_count,
                first_index,
                base_vertex as u32,
                first_instance,
            ],
        );
    }

    pub fn dispatch(&mut self, x: u32, y: u32, z: u32) {
        self.push_cmd(D3D11Opcode::Dispatch, &[x, y, z]);
    }

    pub fn copy_buffer_to_buffer(
        &mut self,
        src: ResourceId,
        src_offset: u64,
        dst: ResourceId,
        dst_offset: u64,
        size: u64,
    ) {
        self.push_cmd(
            D3D11Opcode::CopyBufferToBuffer,
            &[
                src,
                (src_offset & 0xffff_ffff) as u32,
                (src_offset >> 32) as u32,
                dst,
                (dst_offset & 0xffff_ffff) as u32,
                (dst_offset >> 32) as u32,
                (size & 0xffff_ffff) as u32,
                (size >> 32) as u32,
            ],
        );
    }
}

pub struct VertexAttributeDesc {
    pub shader_location: u32,
    pub offset: u32,
    pub format: VertexFormat,
}

pub struct VertexBufferLayoutDesc<'a> {
    pub array_stride: u32,
    pub step_mode: VertexStepMode,
    pub attributes: &'a [VertexAttributeDesc],
}

pub struct BindingDesc {
    pub binding: u32,
    pub ty: BindingType,
    pub visibility: ShaderStageFlags,
    /// For storage textures, the declared format. Otherwise ignored.
    pub storage_texture_format: Option<DxgiFormat>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Texture2dDesc {
    pub width: u32,
    pub height: u32,
    pub array_layers: u32,
    pub mip_level_count: u32,
    pub format: DxgiFormat,
    pub usage: TextureUsage,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Texture2dUpdate<'a> {
    pub mip_level: u32,
    pub array_layer: u32,
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub data: &'a [u8],
}

#[derive(Copy, Clone)]
pub struct RenderPipelineDesc<'a> {
    pub vs_shader: ResourceId,
    pub fs_shader: ResourceId,
    pub color_format: DxgiFormat,
    pub depth_format: DxgiFormat,
    pub topology: PrimitiveTopology,
    pub vertex_buffers: &'a [VertexBufferLayoutDesc<'a>],
    pub bindings: &'a [BindingDesc],
}
