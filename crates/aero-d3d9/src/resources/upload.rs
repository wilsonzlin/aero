use std::sync::Arc;

use wgpu::util::DeviceExt;

pub(crate) enum UploadOp {
    Buffer {
        dst: Arc<wgpu::Buffer>,
        dst_offset: u64,
        src_offset: u64,
        size: u64,
    },
    Texture {
        dst: Arc<wgpu::Texture>,
        dst_mip_level: u32,
        dst_origin: wgpu::Origin3d,
        dst_aspect: wgpu::TextureAspect,
        copy_size: wgpu::Extent3d,
        layout: wgpu::ImageDataLayout,
        src_offset: u64,
    },
}

/// Per-frame upload queue that batches multiple buffer/texture updates into a single staging
/// buffer and a set of `copy_*` commands.
///
/// This avoids per-draw `queue.write_*` calls and matches the intended D3D9 "Lock/Unlock then
/// draw" update pattern.
pub struct UploadQueue {
    staging: Vec<u8>,
    ops: Vec<UploadOp>,
}

impl UploadQueue {
    pub fn new(initial_capacity: usize) -> Self {
        Self {
            staging: Vec::with_capacity(initial_capacity),
            ops: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn clear(&mut self) {
        self.staging.clear();
        self.ops.clear();
    }

    pub fn write_buffer(&mut self, buffer: &Arc<wgpu::Buffer>, dst_offset: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        debug_assert_eq!(
            dst_offset % wgpu::COPY_BUFFER_ALIGNMENT,
            0,
            "buffer uploads must be 4-byte aligned"
        );
        debug_assert_eq!(
            data.len() as u64 % wgpu::COPY_BUFFER_ALIGNMENT,
            0,
            "buffer uploads must be 4-byte aligned"
        );

        let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
        let src_offset = align_up(self.staging.len(), align);
        let size = data.len();
        self.staging.resize(src_offset + size, 0);
        self.staging[src_offset..src_offset + size].copy_from_slice(data);
        self.ops.push(UploadOp::Buffer {
            dst: Arc::clone(buffer),
            dst_offset,
            src_offset: src_offset as u64,
            size: size as u64,
        });
    }

    /// Stages data for a `copy_buffer_to_texture` operation.
    ///
    /// `bytes_per_row` must already satisfy WebGPU alignment rules (256-byte multiple when set).
    pub fn write_texture(
        &mut self,
        texture: &Arc<wgpu::Texture>,
        mip_level: u32,
        origin: wgpu::Origin3d,
        aspect: wgpu::TextureAspect,
        size: wgpu::Extent3d,
        bytes_per_row: u32,
        rows_per_image: u32,
        data: &[u8],
    ) {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let src_offset = align_up(self.staging.len(), align);
        let required = src_offset + data.len();
        self.staging.resize(required, 0);
        self.staging[src_offset..src_offset + data.len()].copy_from_slice(data);

        self.ops.push(UploadOp::Texture {
            dst: Arc::clone(texture),
            dst_mip_level: mip_level,
            dst_origin: origin,
            dst_aspect: aspect,
            copy_size: size,
            layout: wgpu::ImageDataLayout {
                offset: 0, // overridden via src_offset below
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(rows_per_image),
            },
            src_offset: src_offset as u64,
        });
    }

    pub fn encode_and_clear(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        if self.ops.is_empty() {
            return;
        }

        let staging = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aero-d3d9.upload_staging"),
            contents: &self.staging,
            usage: wgpu::BufferUsages::COPY_SRC,
        });

        for op in self.ops.drain(..) {
            match op {
                UploadOp::Buffer {
                    dst,
                    dst_offset,
                    src_offset,
                    size,
                } => {
                    encoder.copy_buffer_to_buffer(&staging, src_offset, &*dst, dst_offset, size);
                }
                UploadOp::Texture {
                    dst,
                    dst_mip_level,
                    dst_origin,
                    dst_aspect,
                    copy_size,
                    mut layout,
                    src_offset,
                } => {
                    layout.offset = src_offset;
                    encoder.copy_buffer_to_texture(
                        wgpu::ImageCopyBuffer {
                            buffer: &staging,
                            layout,
                        },
                        wgpu::ImageCopyTexture {
                            texture: &*dst,
                            mip_level: dst_mip_level,
                            origin: dst_origin,
                            aspect: dst_aspect,
                        },
                        copy_size,
                    );
                }
            }
        }

        self.staging.clear();
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    debug_assert!(alignment.is_power_of_two());
    (value + alignment - 1) & !(alignment - 1)
}
