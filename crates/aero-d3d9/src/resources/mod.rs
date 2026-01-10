mod buffers;
mod evict;
mod formats;
mod rt_ds;
mod samplers;
mod textures;
mod upload;

pub use buffers::*;
pub use evict::*;
pub use formats::*;
pub use rt_ds::*;
pub use samplers::*;
pub use textures::*;
pub use upload::*;

use hashbrown::HashMap;
use std::sync::Arc;

/// A guest-provided handle/ID that identifies a D3D9 resource.
///
/// In real D3D9 this would be a COM pointer; in the emulator we typically operate on opaque
/// IDs coming from the guest-side translation layer.
pub type GuestResourceId = u32;

/// Resource memory pool, modeled after `D3DPOOL`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum D3DPool {
    /// GPU-local / default pool. If evicted, contents are lost.
    Default,
    /// Managed pool: system-memory copy exists and GPU representation can be recreated.
    Managed,
}

bitflags::bitflags! {
    /// Buffer usage flags (subset).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct BufferUsageFlags: u32 {
        /// Marks a buffer as dynamic (`D3DUSAGE_DYNAMIC`).
        const DYNAMIC = 0x1;
    }
}

bitflags::bitflags! {
    /// Lock flags (subset).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct LockFlags: u32 {
        /// `D3DLOCK_READONLY`
        const READONLY = 0x1;
        /// `D3DLOCK_DISCARD`
        const DISCARD = 0x2;
        /// `D3DLOCK_NOOVERWRITE`
        const NOOVERWRITE = 0x4;
    }
}

/// Top-level resource table.
///
/// This keeps D3D9-visible lifetime semantics while allowing the backend to drop GPU objects
/// (managed eviction) and recreate them on demand.
pub struct ResourceManager {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    frame_index: u64,
    options: ResourceManagerOptions,

    uploads: UploadQueue,

    vertex_buffers: HashMap<GuestResourceId, VertexBuffer>,
    index_buffers: HashMap<GuestResourceId, IndexBuffer>,
    textures: HashMap<GuestResourceId, Texture>,
    samplers: HashMap<GuestResourceId, Sampler>,
    render_targets: HashMap<GuestResourceId, RenderTarget>,
    depth_stencils: HashMap<GuestResourceId, DepthStencil>,

    // `DrawPrimitiveUP`/`DrawIndexedPrimitiveUP` upload arenas.
    pub(crate) transient_vertex: Option<TransientBufferArena>,
    pub(crate) transient_index: Option<TransientBufferArena>,
}

#[derive(Clone, Debug)]
pub struct ResourceManagerOptions {
    /// Approximate GPU texture memory budget in bytes. Managed textures may be evicted to stay
    /// under this budget.
    pub texture_budget_bytes: Option<usize>,
    /// Initial per-frame staging buffer size used for batched uploads.
    pub upload_staging_capacity_bytes: usize,
}

impl Default for ResourceManagerOptions {
    fn default() -> Self {
        Self {
            texture_budget_bytes: None,
            upload_staging_capacity_bytes: 4 * 1024 * 1024,
        }
    }
}

impl ResourceManager {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, options: ResourceManagerOptions) -> Self {
        let staging = options.upload_staging_capacity_bytes;
        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            frame_index: 0,
            uploads: UploadQueue::new(staging),
            options,
            vertex_buffers: HashMap::new(),
            index_buffers: HashMap::new(),
            textures: HashMap::new(),
            samplers: HashMap::new(),
            render_targets: HashMap::new(),
            depth_stencils: HashMap::new(),
            transient_vertex: None,
            transient_index: None,
        }
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Call once per rendered frame (or command buffer submission batch).
    ///
    /// This drives LRU timestamps and allows budget-based eviction.
    pub fn begin_frame(&mut self) {
        self.frame_index = self.frame_index.wrapping_add(1);
        self.reset_transient_arenas();
        self.maybe_evict_textures();
    }

    /// Encode any pending uploads into `encoder` before rendering commands that consume the
    /// updated resources.
    pub fn encode_uploads(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.uploads.encode_and_clear(&self.device, encoder);
    }

    /// Submit `encoder` and commit any queued uploads.
    ///
    /// Convenience wrapper used by tests and simple integration points.
    pub fn submit(&mut self, encoder: wgpu::CommandEncoder) {
        self.queue.submit([encoder.finish()]);
    }

    fn maybe_evict_textures(&mut self) {
        let Some(budget) = self.options.texture_budget_bytes else {
            return;
        };

        let safe_frame = self.frame_index.saturating_sub(2);

        // Compute current estimated bytes.
        let mut current: usize = self.textures.values().filter_map(|t| t.gpu_bytes()).sum();

        if current <= budget {
            return;
        }

        // Evict oldest managed textures first.
        let mut candidates: Vec<(GuestResourceId, u64, usize)> = self
            .textures
            .iter()
            .filter_map(|(id, tex)| {
                let bytes = tex.gpu_bytes()?;
                if tex.desc.pool != D3DPool::Managed {
                    return None;
                }
                if tex.last_used_frame > safe_frame {
                    return None;
                }
                Some((*id, tex.last_used_frame, bytes))
            })
            .collect();

        candidates.sort_by_key(|c| c.1);

        for (id, _last, bytes) in candidates {
            if current <= budget {
                break;
            }
            if let Some(tex) = self.textures.get_mut(&id) {
                if tex.evict_gpu() {
                    current = current.saturating_sub(bytes);
                }
            }
        }
    }
}
