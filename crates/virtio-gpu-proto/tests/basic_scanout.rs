use virtio_gpu_proto::device::{GuestMemory, VirtioGpuDevice};
use virtio_gpu_proto::protocol::{
    write_u32_le, write_u64_le, Rect, CtrlHdr, VIRTIO_GPU_CMD_GET_DISPLAY_INFO, VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
    VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SET_SCANOUT,
    VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
};

struct VecGuestMem {
    base: u64,
    buf: Vec<u8>,
}

impl VecGuestMem {
    fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            buf: vec![0; size],
        }
    }

    fn write(&mut self, addr: u64, data: &[u8]) {
        let off = (addr - self.base) as usize;
        self.buf[off..off + data.len()].copy_from_slice(data);
    }
}

impl GuestMemory for VecGuestMem {
    fn read(&self, addr: u64, out: &mut [u8]) -> Result<(), virtio_gpu_proto::device::MemError> {
        let off = addr.checked_sub(self.base).ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)? as usize;
        let end = off.checked_add(out.len()).ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)?;
        let src = self.buf.get(off..end).ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)?;
        out.copy_from_slice(src);
        Ok(())
    }
}

fn hdr(ty: u32) -> Vec<u8> {
    let mut out = Vec::new();
    write_u32_le(&mut out, ty);
    write_u32_le(&mut out, 0); // flags
    write_u64_le(&mut out, 0); // fence_id
    write_u32_le(&mut out, 0); // ctx_id
    write_u32_le(&mut out, 0); // padding
    debug_assert_eq!(out.len(), CtrlHdr::WIREFORMAT_SIZE);
    out
}

fn push_rect(out: &mut Vec<u8>, r: Rect) {
    write_u32_le(out, r.x);
    write_u32_le(out, r.y);
    write_u32_le(out, r.width);
    write_u32_le(out, r.height);
}

#[test]
fn basic_2d_scanout_roundtrip() {
    let mut dev = VirtioGpuDevice::new(4, 4);
    let mut mem = VecGuestMem::new(0x1000, 0x1000);

    // Fill a 4x4 BGRA test pattern in guest memory at 0x1000.
    // Pixel i = (b=i, g=0x80, r=0x40, a=0xff).
    let mut pixels = Vec::new();
    for i in 0u8..16 {
        pixels.extend_from_slice(&[i, 0x80, 0x40, 0xff]);
    }
    mem.write(0x1000, &pixels);

    // GET_DISPLAY_INFO (sanity).
    let req = hdr(VIRTIO_GPU_CMD_GET_DISPLAY_INFO);
    let resp = dev.process_control_command(&req, &mem).unwrap();
    assert!(resp.len() > CtrlHdr::WIREFORMAT_SIZE);

    // RESOURCE_CREATE_2D
    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D);
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM);
    write_u32_le(&mut req, 4);
    write_u32_le(&mut req, 4);
    dev.process_control_command(&req, &mem).unwrap();

    // RESOURCE_ATTACH_BACKING (single entry)
    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING);
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 1); // nr_entries
    write_u64_le(&mut req, 0x1000); // addr
    write_u32_le(&mut req, 64); // len
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    // TRANSFER_TO_HOST_2D (fullscreen)
    let mut req = hdr(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D);
    push_rect(&mut req, Rect::new(0, 0, 4, 4));
    write_u64_le(&mut req, 0); // offset
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    // SET_SCANOUT (scanout 0 uses resource 1)
    let mut req = hdr(VIRTIO_GPU_CMD_SET_SCANOUT);
    push_rect(&mut req, Rect::new(0, 0, 4, 4));
    write_u32_le(&mut req, 0); // scanout_id
    write_u32_le(&mut req, 1); // resource_id
    dev.process_control_command(&req, &mem).unwrap();

    // RESOURCE_FLUSH (fullscreen)
    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_FLUSH);
    push_rect(&mut req, Rect::new(0, 0, 4, 4));
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    assert_eq!(dev.scanout_bgra(), pixels.as_slice());
}

