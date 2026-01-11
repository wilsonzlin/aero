//! Experimental byte-oriented command stream for host-side D3D9 microtests.
//!
//! This is **not** the canonical AeroGPU Guest↔Host protocol. The real transport
//! ABI is implemented by [`crate::protocol`] (mirroring
//! `drivers/aerogpu/protocol/aerogpu_cmd.h`).
//!
//! For the D3D9 command processor bring-up, we use a small self-describing
//! stream with a header (`"AGPU"`, version) followed by length-prefixed
//! commands. This makes fuzzing and deterministic unit tests straightforward
//! without requiring the full guest submission plumbing.

use std::fmt;

pub const STREAM_MAGIC: u32 = u32::from_le_bytes(*b"AGPU");
pub const STREAM_VERSION_MAJOR: u16 = 1;
pub const STREAM_VERSION_MINOR: u16 = 0;
pub const STREAM_HEADER_LEN: usize = 12;
pub const COMMAND_HEADER_LEN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Opcode {
    DeviceCreate = 0x0001,
    DeviceDestroy = 0x0002,
    ContextCreate = 0x0003,
    ContextDestroy = 0x0004,

    SwapChainCreate = 0x0010,
    SwapChainDestroy = 0x0011,

    BufferCreate = 0x0020,
    BufferUpdate = 0x0021,
    BufferDestroy = 0x0022,

    TextureCreate = 0x0030,
    TextureUpdate = 0x0031,
    TextureDestroy = 0x0032,

    SetRenderTargets = 0x0040,
    SetShaderKey = 0x0041,
    SetConstantsF32 = 0x0042,
    SetRenderStateU32 = 0x0043,
    SetVertexDeclaration = 0x0044,
    SetVertexStream = 0x0045,
    SetIndexBuffer = 0x0046,
    SetSamplerStateU32 = 0x0047,
    SetTexture = 0x0048,
    SetViewport = 0x0049,
    SetScissorRect = 0x004A,

    Draw = 0x0050,
    DrawIndexed = 0x0051,
    Present = 0x0052,

    FenceCreate = 0x0060,
    FenceSignal = 0x0061,
    FenceWait = 0x0062,
    FenceDestroy = 0x0063,

    // D3D9Ex shared surface interop (mirrors aerogpu_cmd.h values).
    ExportSharedSurface =
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdOpcode::ExportSharedSurface as u16,
    ImportSharedSurface =
        aero_protocol::aerogpu::aerogpu_cmd::AerogpuCmdOpcode::ImportSharedSurface as u16,
}

impl Opcode {
    pub fn from_u16(value: u16) -> Option<Self> {
        Some(match value {
            0x0001 => Self::DeviceCreate,
            0x0002 => Self::DeviceDestroy,
            0x0003 => Self::ContextCreate,
            0x0004 => Self::ContextDestroy,
            0x0010 => Self::SwapChainCreate,
            0x0011 => Self::SwapChainDestroy,
            0x0020 => Self::BufferCreate,
            0x0021 => Self::BufferUpdate,
            0x0022 => Self::BufferDestroy,
            0x0030 => Self::TextureCreate,
            0x0031 => Self::TextureUpdate,
            0x0032 => Self::TextureDestroy,
            0x0040 => Self::SetRenderTargets,
            0x0041 => Self::SetShaderKey,
            0x0042 => Self::SetConstantsF32,
            0x0043 => Self::SetRenderStateU32,
            0x0044 => Self::SetVertexDeclaration,
            0x0045 => Self::SetVertexStream,
            0x0046 => Self::SetIndexBuffer,
            0x0047 => Self::SetSamplerStateU32,
            0x0048 => Self::SetTexture,
            0x0049 => Self::SetViewport,
            0x004A => Self::SetScissorRect,
            0x0050 => Self::Draw,
            0x0051 => Self::DrawIndexed,
            0x0052 => Self::Present,
            0x0060 => Self::FenceCreate,
            0x0061 => Self::FenceSignal,
            0x0062 => Self::FenceWait,
            0x0063 => Self::FenceDestroy,
            0x0710 => Self::ExportSharedSurface,
            0x0711 => Self::ImportSharedSurface,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShaderStage {
    Vertex = 0,
    Fragment = 1,
}

impl ShaderStage {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Vertex,
            1 => Self::Fragment,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TextureFormat {
    Rgba8Unorm = 1,
    Rgba8UnormSrgb = 2,
    Depth24PlusStencil8 = 3,
}

impl TextureFormat {
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::Rgba8Unorm,
            2 => Self::Rgba8UnormSrgb,
            3 => Self::Depth24PlusStencil8,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TextureUsage {
    Sampled = 1 << 0,
    RenderTarget = 1 << 1,
    DepthStencil = 1 << 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BufferUsage {
    Vertex = 1 << 0,
    Index = 1 << 1,
    Uniform = 1 << 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum IndexFormat {
    U16 = 0,
    U32 = 1,
}

impl IndexFormat {
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::U16,
            1 => Self::U32,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VertexFormat {
    Float32x2 = 0,
    Float32x3 = 1,
    Float32x4 = 2,
    Unorm8x4 = 3,
}

impl VertexFormat {
    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Float32x2,
            1 => Self::Float32x3,
            2 => Self::Float32x4,
            3 => Self::Unorm8x4,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StreamEncoder {
    bytes: Vec<u8>,
}

impl StreamEncoder {
    pub fn new() -> Self {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&STREAM_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MAJOR.to_le_bytes());
        bytes.extend_from_slice(&STREAM_VERSION_MINOR.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // total payload size; patched in finish()
        Self { bytes }
    }

    pub fn finish(mut self) -> Vec<u8> {
        let payload_len = (self.bytes.len() - STREAM_HEADER_LEN) as u32;
        self.bytes[8..12].copy_from_slice(&payload_len.to_le_bytes());
        self.bytes
    }

    pub fn raw_bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn push_command(&mut self, opcode: Opcode, payload: &[u8]) {
        self.bytes.extend_from_slice(&(opcode as u16).to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes()); // flags/reserved
        self.bytes
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(payload);
    }

    pub fn device_create(&mut self, device_id: u32) {
        self.push_command(Opcode::DeviceCreate, &device_id.to_le_bytes());
    }

    pub fn device_destroy(&mut self, device_id: u32) {
        self.push_command(Opcode::DeviceDestroy, &device_id.to_le_bytes());
    }

    pub fn context_create(&mut self, device_id: u32, context_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&device_id.to_le_bytes());
        payload.extend_from_slice(&context_id.to_le_bytes());
        self.push_command(Opcode::ContextCreate, &payload);
    }

    pub fn context_destroy(&mut self, context_id: u32) {
        self.push_command(Opcode::ContextDestroy, &context_id.to_le_bytes());
    }

    pub fn swapchain_create(
        &mut self,
        context_id: u32,
        swapchain_id: u32,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&swapchain_id.to_le_bytes());
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&(format as u32).to_le_bytes());
        self.push_command(Opcode::SwapChainCreate, &payload);
    }

    pub fn swapchain_destroy(&mut self, context_id: u32, swapchain_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&swapchain_id.to_le_bytes());
        self.push_command(Opcode::SwapChainDestroy, &payload);
    }

    pub fn texture_create(
        &mut self,
        context_id: u32,
        texture_id: u32,
        width: u32,
        height: u32,
        mip_level_count: u32,
        format: TextureFormat,
        usage: u32,
    ) {
        let mut payload = Vec::with_capacity(28);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&texture_id.to_le_bytes());
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&mip_level_count.to_le_bytes());
        payload.extend_from_slice(&(format as u32).to_le_bytes());
        payload.extend_from_slice(&usage.to_le_bytes());
        self.push_command(Opcode::TextureCreate, &payload);
    }

    pub fn texture_update_full_mip(
        &mut self,
        context_id: u32,
        texture_id: u32,
        mip_level: u32,
        width: u32,
        height: u32,
        data: &[u8],
    ) {
        let mut payload = Vec::with_capacity(20 + data.len());
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&texture_id.to_le_bytes());
        payload.extend_from_slice(&mip_level.to_le_bytes());
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(data);
        self.push_command(Opcode::TextureUpdate, &payload);
    }

    pub fn texture_destroy(&mut self, context_id: u32, texture_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&texture_id.to_le_bytes());
        self.push_command(Opcode::TextureDestroy, &payload);
    }

    pub fn buffer_create(&mut self, context_id: u32, buffer_id: u32, size: u64, usage: u32) {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&buffer_id.to_le_bytes());
        payload.extend_from_slice(&size.to_le_bytes());
        payload.extend_from_slice(&usage.to_le_bytes());
        self.push_command(Opcode::BufferCreate, &payload);
    }

    pub fn buffer_update(&mut self, context_id: u32, buffer_id: u32, offset: u64, data: &[u8]) {
        let mut payload = Vec::with_capacity(16 + data.len());
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&buffer_id.to_le_bytes());
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(data);
        self.push_command(Opcode::BufferUpdate, &payload);
    }

    pub fn buffer_destroy(&mut self, context_id: u32, buffer_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&buffer_id.to_le_bytes());
        self.push_command(Opcode::BufferDestroy, &payload);
    }

    pub fn set_render_targets(
        &mut self,
        context_id: u32,
        color: RenderTarget,
        depth_stencil: Option<u32>,
    ) {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&(color.kind as u32).to_le_bytes());
        payload.extend_from_slice(&color.id.to_le_bytes());

        if let Some(depth_id) = depth_stencil {
            payload.extend_from_slice(&(RenderTargetKind::Texture as u32).to_le_bytes());
            payload.extend_from_slice(&depth_id.to_le_bytes());
        } else {
            payload.extend_from_slice(&(RenderTargetKind::None as u32).to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
        }

        self.push_command(Opcode::SetRenderTargets, &payload);
    }

    pub fn set_render_targets_swapchain(&mut self, context_id: u32, swapchain_id: u32) {
        self.set_render_targets(
            context_id,
            RenderTarget {
                kind: RenderTargetKind::SwapChain,
                id: swapchain_id,
            },
            None,
        );
    }

    pub fn set_shader_key(&mut self, context_id: u32, stage: ShaderStage, shader_key: u32) {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.push(stage as u8);
        payload.extend_from_slice(&[0u8; 3]);
        payload.extend_from_slice(&shader_key.to_le_bytes());
        self.push_command(Opcode::SetShaderKey, &payload);
    }

    pub fn set_constants_f32(
        &mut self,
        context_id: u32,
        stage: ShaderStage,
        start_register: u16,
        vec4_count: u16,
        data: &[f32],
    ) {
        let mut payload = Vec::with_capacity(12 + data.len() * 4);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.push(stage as u8);
        payload.push(0);
        payload.extend_from_slice(&start_register.to_le_bytes());
        payload.extend_from_slice(&vec4_count.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        for v in data {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        self.push_command(Opcode::SetConstantsF32, &payload);
    }

    pub fn set_render_state_u32(&mut self, context_id: u32, state_id: u32, value: u32) {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&state_id.to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        self.push_command(Opcode::SetRenderStateU32, &payload);
    }

    pub fn set_vertex_declaration(
        &mut self,
        context_id: u32,
        stride: u32,
        attrs: &[VertexAttributeWire],
    ) {
        let mut payload = Vec::with_capacity(12 + attrs.len() * 12);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&stride.to_le_bytes());
        payload.extend_from_slice(&(attrs.len() as u32).to_le_bytes());
        for attr in attrs {
            payload.extend_from_slice(&attr.location.to_le_bytes());
            payload.extend_from_slice(&(attr.format as u32).to_le_bytes());
            payload.extend_from_slice(&attr.offset.to_le_bytes());
        }
        self.push_command(Opcode::SetVertexDeclaration, &payload);
    }

    pub fn set_vertex_stream(
        &mut self,
        context_id: u32,
        stream_index: u8,
        buffer_id: u32,
        offset: u64,
        stride: u32,
    ) {
        let mut payload = Vec::with_capacity(24);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.push(stream_index);
        payload.extend_from_slice(&[0u8; 3]);
        payload.extend_from_slice(&buffer_id.to_le_bytes());
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(&stride.to_le_bytes());
        self.push_command(Opcode::SetVertexStream, &payload);
    }

    pub fn set_index_buffer(
        &mut self,
        context_id: u32,
        buffer_id: u32,
        offset: u64,
        format: IndexFormat,
    ) {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&buffer_id.to_le_bytes());
        payload.extend_from_slice(&offset.to_le_bytes());
        payload.extend_from_slice(&(format as u32).to_le_bytes());
        self.push_command(Opcode::SetIndexBuffer, &payload);
    }

    pub fn set_sampler_state_u32(
        &mut self,
        context_id: u32,
        stage: ShaderStage,
        slot: u8,
        state_id: u32,
        value: u32,
    ) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.push(stage as u8);
        payload.push(slot);
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&state_id.to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        self.push_command(Opcode::SetSamplerStateU32, &payload);
    }

    pub fn set_texture(&mut self, context_id: u32, stage: ShaderStage, slot: u8, texture_id: u32) {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.push(stage as u8);
        payload.push(slot);
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&texture_id.to_le_bytes());
        self.push_command(Opcode::SetTexture, &payload);
    }

    pub fn set_viewport(
        &mut self,
        context_id: u32,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        min_depth: f32,
        max_depth: f32,
    ) {
        let mut payload = Vec::with_capacity(28);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&x.to_le_bytes());
        payload.extend_from_slice(&y.to_le_bytes());
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&min_depth.to_le_bytes());
        payload.extend_from_slice(&max_depth.to_le_bytes());
        self.push_command(Opcode::SetViewport, &payload);
    }

    pub fn set_scissor_rect(&mut self, context_id: u32, x: u32, y: u32, width: u32, height: u32) {
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&x.to_le_bytes());
        payload.extend_from_slice(&y.to_le_bytes());
        payload.extend_from_slice(&width.to_le_bytes());
        payload.extend_from_slice(&height.to_le_bytes());
        self.push_command(Opcode::SetScissorRect, &payload);
    }

    pub fn draw_indexed(
        &mut self,
        context_id: u32,
        index_count: u32,
        first_index: u32,
        base_vertex: i32,
    ) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&index_count.to_le_bytes());
        payload.extend_from_slice(&first_index.to_le_bytes());
        payload.extend_from_slice(&base_vertex.to_le_bytes());
        self.push_command(Opcode::DrawIndexed, &payload);
    }

    pub fn draw(&mut self, context_id: u32, vertex_count: u32, first_vertex: u32) {
        let mut payload = Vec::with_capacity(12);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&vertex_count.to_le_bytes());
        payload.extend_from_slice(&first_vertex.to_le_bytes());
        self.push_command(Opcode::Draw, &payload);
    }

    pub fn present(&mut self, context_id: u32, swapchain_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&swapchain_id.to_le_bytes());
        self.push_command(Opcode::Present, &payload);
    }

    pub fn fence_create(&mut self, context_id: u32, fence_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&fence_id.to_le_bytes());
        self.push_command(Opcode::FenceCreate, &payload);
    }

    pub fn fence_signal(&mut self, context_id: u32, fence_id: u32, value: u64) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&fence_id.to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        self.push_command(Opcode::FenceSignal, &payload);
    }

    pub fn fence_wait(&mut self, context_id: u32, fence_id: u32, value: u64) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&fence_id.to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        self.push_command(Opcode::FenceWait, &payload);
    }

    pub fn fence_destroy(&mut self, context_id: u32, fence_id: u32) {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&context_id.to_le_bytes());
        payload.extend_from_slice(&fence_id.to_le_bytes());
        self.push_command(Opcode::FenceDestroy, &payload);
    }

    pub fn export_shared_surface(&mut self, resource_handle: u32, share_token: u64) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&resource_handle.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&share_token.to_le_bytes());
        self.push_command(Opcode::ExportSharedSurface, &payload);
    }

    pub fn import_shared_surface(&mut self, out_resource_handle: u32, share_token: u64) {
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&out_resource_handle.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&share_token.to_le_bytes());
        self.push_command(Opcode::ImportSharedSurface, &payload);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VertexAttributeWire {
    pub location: u32,
    pub format: VertexFormat,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct RenderTarget {
    pub kind: RenderTargetKind,
    pub id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum RenderTargetKind {
    None = 0,
    SwapChain = 1,
    Texture = 2,
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
