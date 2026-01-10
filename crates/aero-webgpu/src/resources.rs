use crate::BackendCaps;

#[derive(Debug, thiserror::Error)]
pub enum AllocationError {
    #[error("requested buffer size ({requested} bytes) exceeds device limit ({limit} bytes)")]
    BufferTooLarge { requested: u64, limit: u64 },

    #[error("requested texture dimension ({requested}px) exceeds device limit ({limit}px)")]
    TextureTooLarge { requested: u32, limit: u32 },
}

/// Minimal buffer allocation helper.
///
/// This is not a sub-allocator yet; it exists to centralize limit checks and
/// provide a single place to evolve into pooling/suballocation later.
pub struct GpuBufferAllocator<'a> {
    device: &'a wgpu::Device,
    max_buffer_size: u64,
}

impl<'a> GpuBufferAllocator<'a> {
    pub fn new(device: &'a wgpu::Device, caps: &BackendCaps) -> Self {
        Self {
            device,
            max_buffer_size: caps.max_buffer_size,
        }
    }

    pub fn create(
        &self,
        mut desc: wgpu::BufferDescriptor<'_>,
    ) -> Result<wgpu::Buffer, AllocationError> {
        if desc.size > self.max_buffer_size {
            return Err(AllocationError::BufferTooLarge {
                requested: desc.size,
                limit: self.max_buffer_size,
            });
        }

        // Ensure mapped buffers are usable without extra copies (common for staging).
        if desc.mapped_at_creation {
            desc.usage |= wgpu::BufferUsages::MAP_WRITE;
        }

        Ok(self.device.create_buffer(&desc))
    }

    pub fn create_init(
        &self,
        label: Option<&str>,
        contents: &[u8],
        usage: wgpu::BufferUsages,
    ) -> Result<wgpu::Buffer, AllocationError> {
        use wgpu::util::DeviceExt as _;

        let size = contents.len() as u64;
        if size > self.max_buffer_size {
            return Err(AllocationError::BufferTooLarge {
                requested: size,
                limit: self.max_buffer_size,
            });
        }

        Ok(self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label,
                contents,
                usage,
            }))
    }
}

/// Minimal texture allocation helper.
pub struct GpuTextureAllocator<'a> {
    device: &'a wgpu::Device,
    max_texture_dimension_2d: u32,
}

impl<'a> GpuTextureAllocator<'a> {
    pub fn new(device: &'a wgpu::Device, caps: &BackendCaps) -> Self {
        Self {
            device,
            max_texture_dimension_2d: caps.max_texture_dimension_2d,
        }
    }

    pub fn create_texture(
        &self,
        desc: &wgpu::TextureDescriptor<'_>,
    ) -> Result<wgpu::Texture, AllocationError> {
        let size = desc.size;
        if size.width > self.max_texture_dimension_2d || size.height > self.max_texture_dimension_2d
        {
            return Err(AllocationError::TextureTooLarge {
                requested: size.width.max(size.height),
                limit: self.max_texture_dimension_2d,
            });
        }

        Ok(self.device.create_texture(desc))
    }
}
