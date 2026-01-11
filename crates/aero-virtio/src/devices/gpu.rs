use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};
use core::any::Any;

use virtio_gpu_proto::device as proto_dev;
use virtio_gpu_proto::protocol as proto;

pub const VIRTIO_DEVICE_TYPE_GPU: u16 = 16;

/// A sink for presenting scanout pixels to the host.
///
/// The emulator's WebGPU frontend will likely implement this by copying into a
/// `GPUTexture` or `ImageBitmap`.
pub trait ScanoutSink {
    fn present(&mut self, width: u32, height: u32, bgra: &[u8]);
}

/// A no-op [`ScanoutSink`] suitable for tests that only exercise protocol handling.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullScanoutSink;

impl ScanoutSink for NullScanoutSink {
    fn present(&mut self, _width: u32, _height: u32, _bgra: &[u8]) {}
}

struct MemAdapter<'a> {
    mem: &'a dyn GuestMemory,
}

impl<'a> proto_dev::GuestMemory for MemAdapter<'a> {
    fn read(&self, addr: u64, out: &mut [u8]) -> Result<(), proto_dev::MemError> {
        self.mem
            .read(addr, out)
            .map_err(|_| proto_dev::MemError::OutOfBounds)
    }
}

/// Minimal virtio-gpu (2D) device model, intended for "first pixels" bring-up.
///
/// This is a thin wrapper over `virtio-gpu-proto` that:
/// - pulls request/response buffers out of virtqueues
/// - exposes a callback on `RESOURCE_FLUSH` to publish scanout pixels
pub struct VirtioGpu2d<S: ScanoutSink> {
    initial_width: u32,
    initial_height: u32,
    features: u64,
    sink: S,
    gpu: proto_dev::VirtioGpuDevice,
}

impl<S: ScanoutSink> VirtioGpu2d<S> {
    pub fn new(width: u32, height: u32, sink: S) -> Self {
        Self {
            initial_width: width,
            initial_height: height,
            features: 0,
            sink,
            gpu: proto_dev::VirtioGpuDevice::new(width, height),
        }
    }

    fn handle_chain(
        &mut self,
        chain: &DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let descs = chain.descriptors();
        if descs.is_empty() {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        let mut req = Vec::new();
        let mut resp_descs = Vec::new();
        let mut in_resp = false;

        for d in descs {
            if d.is_write_only() {
                in_resp = true;
                resp_descs.push(*d);
                continue;
            }
            if in_resp {
                // Virtio requires all device-writable descriptors to come after the readable ones.
                return Err(VirtioDeviceError::BadDescriptorChain);
            }
            let chunk = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;
            req.extend_from_slice(chunk);
        }

        if resp_descs.is_empty() {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        // Parse the header so we can always reply with an error response on failures.
        let ctrl =
            proto::parse_ctrl_hdr(&req).map_err(|_| VirtioDeviceError::BadDescriptorChain)?;

        let mem_adapter = MemAdapter { mem: &*mem };
        let resp = match self.gpu.process_control_command(&req, &mem_adapter) {
            Ok(r) => r,
            Err(_) => proto::encode_resp_hdr_from_req(&ctrl, proto::VIRTIO_GPU_RESP_ERR_UNSPEC),
        };

        let resp_len = resp.len();
        let total_resp_cap: usize = resp_descs.iter().map(|d| d.len as usize).sum();
        if resp_len > total_resp_cap {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        // Scatter response back into guest buffers.
        let mut written = 0usize;
        for d in &resp_descs {
            if written == resp_len {
                break;
            }
            let take = (d.len as usize).min(resp_len - written);
            let dst = mem
                .get_slice_mut(d.addr, take)
                .map_err(|_| VirtioDeviceError::IoError)?;
            dst.copy_from_slice(&resp[written..written + take]);
            written += take;
        }

        if written != resp_len {
            return Err(VirtioDeviceError::BadDescriptorChain);
        }

        // Present on flush.
        if ctrl.type_ == proto::VIRTIO_GPU_CMD_RESOURCE_FLUSH {
            let (w, h) = self.gpu.display_size();
            self.sink.present(w, h, self.gpu.scanout_bgra());
        }

        let need_irq = queue
            .add_used(mem, chain.head_index(), resp_len as u32)
            .map_err(|_| VirtioDeviceError::IoError)?;
        Ok(need_irq)
    }
}

impl<S: ScanoutSink + 'static> VirtioDevice for VirtioGpu2d<S> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_GPU
    }

    fn device_features(&self) -> u64 {
        // Keep the feature surface minimal; this is a 2D-only scanout implementation.
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC
    }

    fn set_features(&mut self, features: u64) {
        self.features = features;
    }

    fn num_queues(&self) -> u16 {
        // 0 = controlq, 1 = cursorq.
        2
    }

    fn queue_max_size(&self, _queue: u16) -> u16 {
        128
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        match queue_index {
            0 | 1 => self.handle_chain(&chain, queue, mem),
            _ => Err(VirtioDeviceError::Unsupported),
        }
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // struct virtio_gpu_config {
        //   le32 events_read;
        //   le32 events_clear;
        //   le32 num_scanouts;
        //   le32 num_capsets;
        // };
        let mut cfg = [0u8; 16];
        // events_read/events_clear = 0
        cfg[8..12].copy_from_slice(&1u32.to_le_bytes()); // num_scanouts
        cfg[12..16].copy_from_slice(&0u32.to_le_bytes()); // num_capsets

        data.fill(0);
        let start = offset as usize;
        if start >= cfg.len() {
            return;
        }
        let end = (start + data.len()).min(cfg.len());
        data[..end - start].copy_from_slice(&cfg[start..end]);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Read-only for now.
    }

    fn reset(&mut self) {
        self.features = 0;
        self.gpu = proto_dev::VirtioGpuDevice::new(self.initial_width, self.initial_height);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
