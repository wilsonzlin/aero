use anyhow::{anyhow, Result};

use std::sync::Arc;

use super::{BufferUsageFlags, D3DPool, GuestResourceId, LockFlags, ResourceManager};

#[derive(Clone, Copy, Debug)]
pub struct VertexBufferDesc {
    pub size_bytes: u32,
    pub pool: D3DPool,
    pub usage: BufferUsageFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    U16,
    U32,
}

impl IndexFormat {
    pub fn wgpu(self) -> wgpu::IndexFormat {
        match self {
            Self::U16 => wgpu::IndexFormat::Uint16,
            Self::U32 => wgpu::IndexFormat::Uint32,
        }
    }

    pub fn stride_bytes(self) -> u32 {
        match self {
            Self::U16 => 2,
            Self::U32 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IndexBufferDesc {
    pub size_bytes: u32,
    pub format: IndexFormat,
    pub pool: D3DPool,
    pub usage: BufferUsageFlags,
}

#[derive(Clone, Debug)]
struct LockState {
    offset: u32,
    size: u32,
    flags: LockFlags,
}

#[derive(Debug)]
pub struct VertexBuffer {
    desc: VertexBufferDesc,
    gpu: Arc<wgpu::Buffer>,
    /// Shadow data for `D3DPool::Managed`.
    shadow: Option<Vec<u8>>,

    lock: Option<LockState>,
    lock_data: Vec<u8>,
}

impl VertexBuffer {
    pub fn desc(&self) -> &VertexBufferDesc {
        &self.desc
    }

    pub fn gpu_buffer(&self) -> &Arc<wgpu::Buffer> {
        &self.gpu
    }

    pub fn is_dynamic(&self) -> bool {
        self.desc.usage.contains(BufferUsageFlags::DYNAMIC)
    }

    fn create_gpu(device: &wgpu::Device, size: u64) -> Arc<wgpu::Buffer> {
        Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9.vertex_buffer"),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }))
    }

    pub fn lock(
        &mut self,
        device: &wgpu::Device,
        offset: u32,
        size: u32,
        flags: LockFlags,
    ) -> Result<&mut [u8]> {
        if self.lock.is_some() {
            return Err(anyhow!("buffer already locked"));
        }

        let size = if size == 0 {
            self.desc
                .size_bytes
                .checked_sub(offset)
                .ok_or_else(|| anyhow!("lock offset out of range"))?
        } else {
            size
        };

        if offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("lock range overflow"))?
            > self.desc.size_bytes
        {
            return Err(anyhow!("lock range out of bounds"));
        }

        if flags.contains(LockFlags::READONLY) && self.shadow.is_none() {
            return Err(anyhow!("READONLY lock requires managed shadow data"));
        }

        if flags.contains(LockFlags::DISCARD) && self.is_dynamic() {
            // Driver-style DISCARD: allocate fresh backing store so previous contents can remain
            // in-flight on the GPU.
            self.gpu = Self::create_gpu(device, self.desc.size_bytes as u64);
        }

        self.lock = Some(LockState {
            offset,
            size,
            flags,
        });
        self.lock_data.resize(size as usize, 0);

        if flags.contains(LockFlags::READONLY) {
            if let Some(shadow) = &self.shadow {
                let start = offset as usize;
                let end = start + size as usize;
                self.lock_data
                    .as_mut_slice()
                    .copy_from_slice(&shadow[start..end]);
            }
        } else if !flags.contains(LockFlags::DISCARD) {
            if let Some(shadow) = &self.shadow {
                let start = offset as usize;
                let end = start + size as usize;
                self.lock_data
                    .as_mut_slice()
                    .copy_from_slice(&shadow[start..end]);
            }
        }

        Ok(&mut self.lock_data)
    }

    pub fn unlock(&mut self, uploads: &mut super::UploadQueue) -> Result<()> {
        let Some(lock) = self.lock.take() else {
            return Err(anyhow!("buffer not locked"));
        };

        if !lock.flags.contains(LockFlags::READONLY) {
            uploads.write_buffer(&self.gpu, lock.offset as u64, &self.lock_data);

            if let Some(shadow) = &mut self.shadow {
                let start = lock.offset as usize;
                let end = start + lock.size as usize;
                shadow[start..end].copy_from_slice(&self.lock_data);
            }
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct IndexBuffer {
    desc: IndexBufferDesc,
    gpu: Arc<wgpu::Buffer>,
    shadow: Option<Vec<u8>>,

    lock: Option<LockState>,
    lock_data: Vec<u8>,
}

impl IndexBuffer {
    pub fn desc(&self) -> &IndexBufferDesc {
        &self.desc
    }

    pub fn gpu_buffer(&self) -> &Arc<wgpu::Buffer> {
        &self.gpu
    }

    pub fn wgpu_index_format(&self) -> wgpu::IndexFormat {
        self.desc.format.wgpu()
    }

    pub fn is_dynamic(&self) -> bool {
        self.desc.usage.contains(BufferUsageFlags::DYNAMIC)
    }

    fn create_gpu(device: &wgpu::Device, size: u64) -> Arc<wgpu::Buffer> {
        Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aero-d3d9.index_buffer"),
            size,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }))
    }

    pub fn lock(
        &mut self,
        device: &wgpu::Device,
        offset: u32,
        size: u32,
        flags: LockFlags,
    ) -> Result<&mut [u8]> {
        if self.lock.is_some() {
            return Err(anyhow!("buffer already locked"));
        }

        let size = if size == 0 {
            self.desc
                .size_bytes
                .checked_sub(offset)
                .ok_or_else(|| anyhow!("lock offset out of range"))?
        } else {
            size
        };

        if offset
            .checked_add(size)
            .ok_or_else(|| anyhow!("lock range overflow"))?
            > self.desc.size_bytes
        {
            return Err(anyhow!("lock range out of bounds"));
        }

        if flags.contains(LockFlags::READONLY) && self.shadow.is_none() {
            return Err(anyhow!("READONLY lock requires managed shadow data"));
        }

        if flags.contains(LockFlags::DISCARD) && self.is_dynamic() {
            self.gpu = Self::create_gpu(device, self.desc.size_bytes as u64);
        }

        self.lock = Some(LockState {
            offset,
            size,
            flags,
        });
        self.lock_data.resize(size as usize, 0);

        if flags.contains(LockFlags::READONLY) {
            if let Some(shadow) = &self.shadow {
                let start = offset as usize;
                let end = start + size as usize;
                self.lock_data
                    .as_mut_slice()
                    .copy_from_slice(&shadow[start..end]);
            }
        } else if !flags.contains(LockFlags::DISCARD) {
            if let Some(shadow) = &self.shadow {
                let start = offset as usize;
                let end = start + size as usize;
                self.lock_data
                    .as_mut_slice()
                    .copy_from_slice(&shadow[start..end]);
            }
        }

        Ok(&mut self.lock_data)
    }

    pub fn unlock(&mut self, uploads: &mut super::UploadQueue) -> Result<()> {
        let Some(lock) = self.lock.take() else {
            return Err(anyhow!("buffer not locked"));
        };

        if !lock.flags.contains(LockFlags::READONLY) {
            uploads.write_buffer(&self.gpu, lock.offset as u64, &self.lock_data);

            if let Some(shadow) = &mut self.shadow {
                let start = lock.offset as usize;
                let end = start + lock.size as usize;
                shadow[start..end].copy_from_slice(&self.lock_data);
            }
        }

        Ok(())
    }
}

/// A per-frame arena used to service `DrawPrimitiveUP` / `DrawIndexedPrimitiveUP` style draws.
///
/// The caller is expected to reset this arena once per frame (usually via
/// [`ResourceManager::begin_frame`]).
#[derive(Debug)]
pub struct TransientBufferArena {
    label: &'static str,
    usage: wgpu::BufferUsages,
    buffer: Arc<wgpu::Buffer>,
    capacity: u64,
    cursor: u64,
}

impl TransientBufferArena {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        capacity: u64,
    ) -> Self {
        let buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        Self {
            label,
            usage,
            buffer,
            capacity,
            cursor: 0,
        }
    }

    fn reset(&mut self) {
        self.cursor = 0;
    }

    fn allocate(
        &mut self,
        device: &wgpu::Device,
        uploads: &mut super::UploadQueue,
        data: &[u8],
        alignment: u64,
    ) -> (Arc<wgpu::Buffer>, u64, u64) {
        let aligned = align_up_u64(self.cursor, alignment.max(4));
        let required = aligned + data.len() as u64;

        if required > self.capacity {
            // Grow to next power-of-two-ish, but at least `required`.
            let mut new_cap = self.capacity.max(1);
            while new_cap < required {
                new_cap *= 2;
            }
            self.capacity = new_cap;
            self.buffer = Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: self.capacity,
                usage: self.usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.cursor = 0;
            return self.allocate(device, uploads, data, alignment);
        }

        uploads.write_buffer(&self.buffer, aligned, data);
        self.cursor = required;

        (Arc::clone(&self.buffer), aligned, data.len() as u64)
    }
}

fn align_up_u64(value: u64, alignment: u64) -> u64 {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}

impl ResourceManager {
    pub fn create_vertex_buffer(
        &mut self,
        id: GuestResourceId,
        desc: VertexBufferDesc,
    ) -> Result<()> {
        if self.vertex_buffers.contains_key(&id) {
            return Err(anyhow!("vertex buffer id already exists: {}", id));
        }

        let gpu = VertexBuffer::create_gpu(&self.device, desc.size_bytes as u64);
        let shadow = match desc.pool {
            D3DPool::Managed => Some(vec![0u8; desc.size_bytes as usize]),
            D3DPool::Default => None,
        };

        self.vertex_buffers.insert(
            id,
            VertexBuffer {
                desc,
                gpu,
                shadow,
                lock: None,
                lock_data: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn vertex_buffer_mut(&mut self, id: GuestResourceId) -> Result<&mut VertexBuffer> {
        self.vertex_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("vertex buffer not found: {}", id))
    }

    pub fn vertex_buffer(&self, id: GuestResourceId) -> Result<&VertexBuffer> {
        self.vertex_buffers
            .get(&id)
            .ok_or_else(|| anyhow!("vertex buffer not found: {}", id))
    }

    pub fn destroy_vertex_buffer(&mut self, id: GuestResourceId) -> bool {
        self.vertex_buffers.remove(&id).is_some()
    }

    pub fn lock_vertex_buffer(
        &mut self,
        id: GuestResourceId,
        offset: u32,
        size: u32,
        flags: LockFlags,
    ) -> Result<&mut [u8]> {
        let device = std::sync::Arc::clone(&self.device);
        let vb = self
            .vertex_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("vertex buffer not found: {}", id))?;
        vb.lock(&device, offset, size, flags)
    }

    pub fn unlock_vertex_buffer(&mut self, id: GuestResourceId) -> Result<()> {
        let uploads = &mut self.uploads;
        let vb = self
            .vertex_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("vertex buffer not found: {}", id))?;
        vb.unlock(uploads)
    }

    pub fn create_index_buffer(
        &mut self,
        id: GuestResourceId,
        desc: IndexBufferDesc,
    ) -> Result<()> {
        if self.index_buffers.contains_key(&id) {
            return Err(anyhow!("index buffer id already exists: {}", id));
        }

        let gpu = IndexBuffer::create_gpu(&self.device, desc.size_bytes as u64);
        let shadow = match desc.pool {
            D3DPool::Managed => Some(vec![0u8; desc.size_bytes as usize]),
            D3DPool::Default => None,
        };

        self.index_buffers.insert(
            id,
            IndexBuffer {
                desc,
                gpu,
                shadow,
                lock: None,
                lock_data: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn index_buffer_mut(&mut self, id: GuestResourceId) -> Result<&mut IndexBuffer> {
        self.index_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("index buffer not found: {}", id))
    }

    pub fn index_buffer(&self, id: GuestResourceId) -> Result<&IndexBuffer> {
        self.index_buffers
            .get(&id)
            .ok_or_else(|| anyhow!("index buffer not found: {}", id))
    }

    pub fn destroy_index_buffer(&mut self, id: GuestResourceId) -> bool {
        self.index_buffers.remove(&id).is_some()
    }

    pub fn lock_index_buffer(
        &mut self,
        id: GuestResourceId,
        offset: u32,
        size: u32,
        flags: LockFlags,
    ) -> Result<&mut [u8]> {
        let device = std::sync::Arc::clone(&self.device);
        let ib = self
            .index_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("index buffer not found: {}", id))?;
        ib.lock(&device, offset, size, flags)
    }

    pub fn unlock_index_buffer(&mut self, id: GuestResourceId) -> Result<()> {
        let uploads = &mut self.uploads;
        let ib = self
            .index_buffers
            .get_mut(&id)
            .ok_or_else(|| anyhow!("index buffer not found: {}", id))?;
        ib.unlock(uploads)
    }

    pub(crate) fn ensure_transient_arenas(&mut self) {
        // Lazy-init in case manager was constructed before we decided to use transient uploads.
        // (This avoids exposing arena configuration publicly.)
        if self.transient_vertex.is_none() {
            self.transient_vertex = Some(TransientBufferArena::new(
                &self.device,
                "aero-d3d9.transient_vertex",
                wgpu::BufferUsages::VERTEX,
                1024 * 1024,
            ));
        }
        if self.transient_index.is_none() {
            self.transient_index = Some(TransientBufferArena::new(
                &self.device,
                "aero-d3d9.transient_index",
                wgpu::BufferUsages::INDEX,
                256 * 1024,
            ));
        }
    }

    pub fn reset_transient_arenas(&mut self) {
        if let Some(arena) = &mut self.transient_vertex {
            arena.reset();
        }
        if let Some(arena) = &mut self.transient_index {
            arena.reset();
        }
    }

    pub fn upload_user_vertex_data(&mut self, data: &[u8]) -> (Arc<wgpu::Buffer>, u64, u64) {
        self.ensure_transient_arenas();
        self.transient_vertex
            .as_mut()
            .unwrap()
            .allocate(&self.device, &mut self.uploads, data, 4)
    }

    pub fn upload_user_index_data(&mut self, data: &[u8]) -> (Arc<wgpu::Buffer>, u64, u64) {
        self.ensure_transient_arenas();
        self.transient_index
            .as_mut()
            .unwrap()
            .allocate(&self.device, &mut self.uploads, data, 4)
    }
}
