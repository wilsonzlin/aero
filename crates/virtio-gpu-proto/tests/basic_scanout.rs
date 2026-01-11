use virtio_gpu_proto::device::{GuestMemory, VirtioGpuDevice};
use virtio_gpu_proto::protocol::{
    write_u32_le, write_u64_le, CtrlHdr, Rect, VIRTIO_GPU_CMD_GET_DISPLAY_INFO,
    VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
    VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
    VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
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
        let off = addr
            .checked_sub(self.base)
            .ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)? as usize;
        let end = off
            .checked_add(out.len())
            .ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)?;
        let src = self
            .buf
            .get(off..end)
            .ok_or(virtio_gpu_proto::device::MemError::OutOfBounds)?;
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
    let mut mem = VecGuestMem::new(0x1000, 0x2000);

    // Fill a 4x4 BGRA test pattern in guest memory split across two backing entries:
    // - entry0: 0x1000..0x101f (first 8 pixels)
    // - entry1: 0x2000..0x201f (last 8 pixels)
    // Pixel i = (b=i, g=0x80, r=0x40, a=0xff).
    let mut pixels = Vec::new();
    for i in 0u8..16 {
        pixels.extend_from_slice(&[i, 0x80, 0x40, 0xff]);
    }
    mem.write(0x1000, &pixels[..32]);
    mem.write(0x2000, &pixels[32..]);

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

    // RESOURCE_ATTACH_BACKING (two entries)
    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING);
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 2); // nr_entries
                               // entry 0
    write_u64_le(&mut req, 0x1000); // addr
    write_u32_le(&mut req, 32); // len
    write_u32_le(&mut req, 0); // padding
                               // entry 1
    write_u64_le(&mut req, 0x2000); // addr
    write_u32_le(&mut req, 32); // len
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

    // Now update a 2x2 rect at (1,1) in guest memory and do a partial transfer+flush.
    // This exercises:
    // - backing offset handling (offset != 0)
    // - reading across backing entries (second row crosses entry boundary)
    let rect = Rect::new(1, 1, 2, 2);
    let stride = 4usize * 4usize;
    let bpp = 4usize;
    let offset = (rect.y as usize * stride + rect.x as usize * bpp) as u64;

    // Overwrite those 4 pixels with a distinctive pattern (BGRA = 0xaa,0xbb,0xcc,0xff).
    let new_px = [0xaa, 0xbb, 0xcc, 0xff];
    let mut expected = pixels.clone();
    for dy in 0..rect.height as usize {
        for dx in 0..rect.width as usize {
            let x = rect.x as usize + dx;
            let y = rect.y as usize + dy;
            let pixel_index = y * 4 + x;
            expected[pixel_index * 4..pixel_index * 4 + 4].copy_from_slice(&new_px);

            let backing_off = y * stride + x * bpp;
            let (addr_base, entry_off) = if backing_off < 32 {
                (0x1000u64, backing_off)
            } else {
                (0x2000u64, backing_off - 32)
            };
            mem.write(addr_base + entry_off as u64, &new_px);
        }
    }

    let mut req = hdr(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D);
    push_rect(&mut req, rect);
    write_u64_le(&mut req, offset); // offset within backing for top-left of rect
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_FLUSH);
    push_rect(&mut req, rect);
    write_u32_le(&mut req, 1); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    assert_eq!(dev.scanout_bgra(), expected.as_slice());

    // Resolution change: create a new 2x2 resource and bind it as the new scanout.
    let new_pixels: [u8; 16] = [
        0x10, 0x20, 0x30, 0xff, // (0,0)
        0x11, 0x21, 0x31, 0xff, // (1,0)
        0x12, 0x22, 0x32, 0xff, // (0,1)
        0x13, 0x23, 0x33, 0xff, // (1,1)
    ];
    mem.write(0x1800, &new_pixels);

    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D);
    write_u32_le(&mut req, 2); // resource_id
    write_u32_le(&mut req, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM);
    write_u32_le(&mut req, 2);
    write_u32_le(&mut req, 2);
    dev.process_control_command(&req, &mem).unwrap();

    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING);
    write_u32_le(&mut req, 2); // resource_id
    write_u32_le(&mut req, 1); // nr_entries
    write_u64_le(&mut req, 0x1800); // addr
    write_u32_le(&mut req, 16); // len
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    let mut req = hdr(VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D);
    push_rect(&mut req, Rect::new(0, 0, 2, 2));
    write_u64_le(&mut req, 0); // offset
    write_u32_le(&mut req, 2); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    let mut req = hdr(VIRTIO_GPU_CMD_SET_SCANOUT);
    push_rect(&mut req, Rect::new(0, 0, 2, 2));
    write_u32_le(&mut req, 0); // scanout_id
    write_u32_le(&mut req, 2); // resource_id
    dev.process_control_command(&req, &mem).unwrap();

    let mut req = hdr(VIRTIO_GPU_CMD_RESOURCE_FLUSH);
    push_rect(&mut req, Rect::new(0, 0, 2, 2));
    write_u32_le(&mut req, 2); // resource_id
    write_u32_le(&mut req, 0); // padding
    dev.process_control_command(&req, &mem).unwrap();

    assert_eq!(dev.display_size(), (2, 2));
    assert_eq!(dev.scanout_bgra(), new_pixels.as_slice());
}
