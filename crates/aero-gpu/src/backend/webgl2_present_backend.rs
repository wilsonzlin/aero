use crate::hal::*;
use crate::{GpuCapabilities, GpuError};

/// Adapter trait for Task 251's raw WebGL2 presenter.
///
/// The full WebGL2 presenter lives elsewhere; this is the narrow surface needed by the HAL.
pub trait WebGl2Presenter {
    fn present_rgba8_framebuffer(
        &mut self,
        width: u32,
        height: u32,
        rgba8: &[u8],
    ) -> Result<(), GpuError>;
    fn screenshot_rgba8(&mut self) -> Result<Vec<u8>, GpuError>;
}

/// Minimal backend that only supports presenting an RGBA8 framebuffer via raw WebGL2.
pub struct WebGl2PresentBackend<P> {
    presenter: P,
    capabilities: GpuCapabilities,
}

impl<P> WebGl2PresentBackend<P> {
    pub fn new(presenter: P) -> Self {
        Self {
            presenter,
            capabilities: GpuCapabilities {
                supports_compute: false,
                timestamp_queries_supported: false,
                ..Default::default()
            },
        }
    }
}

impl<P: WebGl2Presenter> GpuBackend for WebGl2PresentBackend<P> {
    fn kind(&self) -> BackendKind {
        BackendKind::WebGl2Raw
    }

    fn capabilities(&self) -> &GpuCapabilities {
        &self.capabilities
    }

    fn create_buffer(&mut self, _desc: BufferDesc) -> Result<BufferId, GpuError> {
        Err(GpuError::Unsupported("hal.create_buffer"))
    }

    fn destroy_buffer(&mut self, _id: BufferId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_buffer"))
    }

    fn write_buffer(
        &mut self,
        _buffer: BufferId,
        _offset: u64,
        _data: &[u8],
    ) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.write_buffer"))
    }

    fn create_texture(&mut self, _desc: TextureDesc) -> Result<TextureId, GpuError> {
        Err(GpuError::Unsupported("hal.create_texture"))
    }

    fn destroy_texture(&mut self, _id: TextureId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_texture"))
    }

    fn create_texture_view(
        &mut self,
        _texture: TextureId,
        _desc: TextureViewDesc,
    ) -> Result<TextureViewId, GpuError> {
        Err(GpuError::Unsupported("hal.create_texture_view"))
    }

    fn destroy_texture_view(&mut self, _id: TextureViewId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_texture_view"))
    }

    fn create_sampler(&mut self, _desc: SamplerDesc) -> Result<SamplerId, GpuError> {
        Err(GpuError::Unsupported("hal.create_sampler"))
    }

    fn destroy_sampler(&mut self, _id: SamplerId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_sampler"))
    }

    fn create_bind_group_layout(
        &mut self,
        _desc: BindGroupLayoutDesc,
    ) -> Result<BindGroupLayoutId, GpuError> {
        Err(GpuError::Unsupported("hal.create_bind_group_layout"))
    }

    fn destroy_bind_group_layout(&mut self, _id: BindGroupLayoutId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_bind_group_layout"))
    }

    fn create_bind_group(&mut self, _desc: BindGroupDesc) -> Result<BindGroupId, GpuError> {
        Err(GpuError::Unsupported("hal.create_bind_group"))
    }

    fn destroy_bind_group(&mut self, _id: BindGroupId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_bind_group"))
    }

    fn create_render_pipeline(
        &mut self,
        _desc: RenderPipelineDesc,
    ) -> Result<PipelineId, GpuError> {
        Err(GpuError::Unsupported("hal.create_render_pipeline"))
    }

    fn create_compute_pipeline(
        &mut self,
        _desc: ComputePipelineDesc,
    ) -> Result<PipelineId, GpuError> {
        Err(GpuError::Unsupported("hal.create_compute_pipeline"))
    }

    fn destroy_pipeline(&mut self, _id: PipelineId) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.destroy_pipeline"))
    }

    fn create_command_buffer(
        &mut self,
        _commands: &[GpuCommand],
    ) -> Result<CommandBufferId, GpuError> {
        Err(GpuError::Unsupported("hal.create_command_buffer"))
    }

    fn submit(&mut self, _command_buffers: &[CommandBufferId]) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.submit"))
    }

    fn present(&mut self) -> Result<(), GpuError> {
        Err(GpuError::Unsupported("hal.present"))
    }

    fn present_rgba8_framebuffer(
        &mut self,
        width: u32,
        height: u32,
        rgba8: &[u8],
    ) -> Result<(), GpuError> {
        self.presenter
            .present_rgba8_framebuffer(width, height, rgba8)
    }

    fn screenshot_rgba8(&mut self) -> Result<Vec<u8>, GpuError> {
        self.presenter.screenshot_rgba8()
    }
}
