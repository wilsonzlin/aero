use std::collections::HashMap;

use crate::protocol::{
    encode_resp_hdr_from_req, parse_ctrl_hdr, parse_rect, read_u32_le, read_u64_le, CtrlHdr, ProtocolError,
    Rect, VIRTIO_GPU_CMD_GET_DISPLAY_INFO, VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
    VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_RESOURCE_UNREF,
    VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, VIRTIO_GPU_CMD_UPDATE_CURSOR,
    VIRTIO_GPU_CMD_MOVE_CURSOR, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM,
    VIRTIO_GPU_MAX_SCANOUTS, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER, VIRTIO_GPU_RESP_ERR_UNSPEC,
    VIRTIO_GPU_RESP_OK_DISPLAY_INFO, VIRTIO_GPU_RESP_OK_NODATA,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    OutOfBounds,
}

pub trait GuestMemory {
    fn read(&self, addr: u64, out: &mut [u8]) -> Result<(), MemError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemEntry {
    addr: u64,
    len: u32,
}

#[derive(Debug, Clone)]
struct Resource2D {
    width: u32,
    height: u32,
    format: u32,
    // Guest backing (virtio scatter/gather list; treated as a single linear byte array).
    backing: Vec<MemEntry>,
    backing_len: u64,
    // Host-side copy of pixels.
    pixels_bgra: Vec<u8>,
}

impl Resource2D {
    fn bytes_per_pixel(&self) -> Result<usize, ProtocolError> {
        match self.format {
            VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM => Ok(4),
            _ => Err(ProtocolError::InvalidParameter("unsupported resource format")),
        }
    }

    fn required_backing_len(&self) -> Result<u64, ProtocolError> {
        let bpp = self.bytes_per_pixel()? as u64;
        let pixels = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .ok_or(ProtocolError::InvalidParameter("resource size overflow"))?;
        pixels
            .checked_mul(bpp)
            .ok_or(ProtocolError::InvalidParameter("resource size overflow"))
    }

    fn read_backing_linear(
        &self,
        mem: &impl GuestMemory,
        mut offset: u64,
        mut out: &mut [u8],
    ) -> Result<(), ProtocolError> {
        if out.is_empty() {
            return Ok(());
        }
        if self.backing.is_empty() {
            return Err(ProtocolError::InvalidParameter("resource has no backing attached"));
        }
        if offset
            .checked_add(out.len() as u64)
            .filter(|end| *end <= self.backing_len)
            .is_none()
        {
            return Err(ProtocolError::InvalidParameter("backing read out of bounds"));
        }

        for entry in &self.backing {
            let entry_len = u64::from(entry.len);
            if offset >= entry_len {
                offset -= entry_len;
                continue;
            }

            let avail = entry_len - offset;
            let take = avail.min(out.len() as u64) as usize;
            let addr = entry
                .addr
                .checked_add(offset)
                .ok_or(ProtocolError::InvalidParameter("guest backing addr overflow"))?;
            mem.read(addr, &mut out[..take])
                .map_err(|_| ProtocolError::InvalidParameter("guest memory read failed"))?;
            out = &mut out[take..];
            offset = 0;
            if out.is_empty() {
                return Ok(());
            }
        }

        // We already bounds-checked against `backing_len`, so this should be unreachable unless the
        // entry list is internally inconsistent.
        Err(ProtocolError::InvalidParameter("backing read fell off end"))
    }
}

#[derive(Debug, Clone)]
struct Scanout {
    enabled: bool,
    rect: Rect,
    resource_id: u32,
}

/// Minimal virtio-gpu (2D-only) device model prototype.
///
/// Notes:
/// - This processes *one* control queue message at a time (`process_control_command`).
/// - Virtqueue DMA / descriptor walking is out of scope for this crate.
/// - Only BGRA8888 resources are supported (enough for a basic desktop scanout).
pub struct VirtioGpuDevice {
    display_width: u32,
    display_height: u32,
    resources: HashMap<u32, Resource2D>,
    scanouts: [Scanout; VIRTIO_GPU_MAX_SCANOUTS],
    // Current scanout framebuffer (BGRA).
    scanout_bgra: Vec<u8>,
}

impl VirtioGpuDevice {
    pub fn new(display_width: u32, display_height: u32) -> Self {
        let mut scanouts: [Scanout; VIRTIO_GPU_MAX_SCANOUTS] =
            core::array::from_fn(|_| Scanout {
                enabled: false,
                rect: Rect::new(0, 0, 0, 0),
                resource_id: 0,
            });

        // Provide a default enabled scanout mode (like QEMU does).
        scanouts[0].enabled = true;
        scanouts[0].rect = Rect::new(0, 0, display_width, display_height);

        Self {
            display_width,
            display_height,
            resources: HashMap::new(),
            scanouts,
            scanout_bgra: vec![0; display_width as usize * display_height as usize * 4],
        }
    }

    pub fn scanout_bgra(&self) -> &[u8] {
        &self.scanout_bgra
    }

    pub fn process_control_command(
        &mut self,
        req_bytes: &[u8],
        mem: &impl GuestMemory,
    ) -> Result<Vec<u8>, ProtocolError> {
        let hdr = parse_ctrl_hdr(req_bytes)?;
        match hdr.type_ {
            VIRTIO_GPU_CMD_GET_DISPLAY_INFO => self.cmd_get_display_info(&hdr),
            VIRTIO_GPU_CMD_RESOURCE_CREATE_2D => self.cmd_resource_create_2d(&hdr, req_bytes),
            VIRTIO_GPU_CMD_RESOURCE_UNREF => self.cmd_resource_unref(&hdr, req_bytes),
            VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING => self.cmd_resource_attach_backing(&hdr, req_bytes),
            VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING => self.cmd_resource_detach_backing(&hdr, req_bytes),
            VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D => self.cmd_transfer_to_host_2d(&hdr, req_bytes, mem),
            VIRTIO_GPU_CMD_SET_SCANOUT => self.cmd_set_scanout(&hdr, req_bytes),
            VIRTIO_GPU_CMD_RESOURCE_FLUSH => self.cmd_resource_flush(&hdr, req_bytes),
            VIRTIO_GPU_CMD_UPDATE_CURSOR | VIRTIO_GPU_CMD_MOVE_CURSOR => {
                // Cursor updates are sent on a separate virtqueue. This prototype doesn't implement a
                // hardware cursor yet, but Windows drivers may use these unconditionally.
                Ok(encode_resp_hdr_from_req(&hdr, VIRTIO_GPU_RESP_OK_NODATA))
            }
            other => Err(ProtocolError::UnknownCommand(other)),
        }
    }

    fn cmd_get_display_info(&self, req: &CtrlHdr) -> Result<Vec<u8>, ProtocolError> {
        // virtio_gpu_resp_display_info:
        // hdr + 16 scanouts, each:
        //   rect (16 bytes) + enabled (u32) + flags (u32)
        let mut out = encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_DISPLAY_INFO);
        for s in &self.scanouts {
            // rect
            out.extend_from_slice(&s.rect.x.to_le_bytes());
            out.extend_from_slice(&s.rect.y.to_le_bytes());
            out.extend_from_slice(&s.rect.width.to_le_bytes());
            out.extend_from_slice(&s.rect.height.to_le_bytes());
            // enabled + flags
            out.extend_from_slice(&(s.enabled as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        Ok(out)
    }

    fn cmd_resource_create_2d(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_resource_create_2d:
        // hdr + resource_id + format + width + height
        let want = CtrlHdr::WIREFORMAT_SIZE + 16;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let resource_id = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE)?;
        let format = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE + 4)?;
        let width = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE + 8)?;
        let height = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE + 12)?;

        if width == 0 || height == 0 {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }
        if format != VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        let size = width
            .checked_mul(height)
            .and_then(|v| v.checked_mul(4))
            .ok_or(ProtocolError::InvalidParameter("resource size overflow"))?;

        self.resources.insert(
            resource_id,
            Resource2D {
                width,
                height,
                format,
                backing: Vec::new(),
                backing_len: 0,
                pixels_bgra: vec![0; size as usize],
            },
        );

        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_resource_unref(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // hdr + resource_id + padding
        let want = CtrlHdr::WIREFORMAT_SIZE + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let resource_id = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE)?;
        self.resources.remove(&resource_id);
        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_resource_attach_backing(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_resource_attach_backing:
        // hdr + resource_id + nr_entries + entries[nr_entries]
        let base = CtrlHdr::WIREFORMAT_SIZE;
        let want = base + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let resource_id = read_u32_le(req_bytes, base)?;
        let nr_entries = read_u32_le(req_bytes, base + 4)? as usize;

        let entry_off = base + 8;
        let entry_size: usize = 16;
        let want2 = entry_off
            .checked_add(entry_size.checked_mul(nr_entries).ok_or(ProtocolError::InvalidParameter(
                "attach_backing size overflow",
            ))?)
            .ok_or(ProtocolError::InvalidParameter("attach_backing size overflow"))?;
        if req_bytes.len() < want2 {
            return Err(ProtocolError::BufferTooShort {
                want: want2,
                got: req_bytes.len(),
            });
        }

        if nr_entries == 0 {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        let res = self
            .resources
            .get_mut(&resource_id)
            .ok_or(ProtocolError::InvalidParameter("unknown resource_id"))?;

        // Parse + validate backing.
        let mut backing = Vec::with_capacity(nr_entries);
        let mut total_len: u64 = 0;
        for i in 0..nr_entries {
            let off = entry_off + i * entry_size;
            let addr = read_u64_le(req_bytes, off)?;
            let len = read_u32_le(req_bytes, off + 8)?;
            // padding at +12 ignored
            total_len = total_len
                .checked_add(u64::from(len))
                .ok_or(ProtocolError::InvalidParameter("backing length overflow"))?;
            backing.push(MemEntry { addr, len });
        }

        let required = res.required_backing_len()?;
        if total_len < required {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        res.backing = backing;
        res.backing_len = total_len;
        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_resource_detach_backing(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // hdr + resource_id + padding
        let want = CtrlHdr::WIREFORMAT_SIZE + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let resource_id = read_u32_le(req_bytes, CtrlHdr::WIREFORMAT_SIZE)?;
        if let Some(res) = self.resources.get_mut(&resource_id) {
            res.backing.clear();
            res.backing_len = 0;
        }
        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_transfer_to_host_2d(
        &mut self,
        req: &CtrlHdr,
        req_bytes: &[u8],
        mem: &impl GuestMemory,
    ) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_transfer_to_host_2d:
        // hdr + rect + offset(u64) + resource_id(u32) + padding(u32)
        let base = CtrlHdr::WIREFORMAT_SIZE;
        let want = base + 16 + 8 + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let rect = parse_rect(req_bytes, base)?;
        let offset = read_u64_le(req_bytes, base + 16)?;
        let resource_id = read_u32_le(req_bytes, base + 24)?;

        let res = self
            .resources
            .get_mut(&resource_id)
            .ok_or(ProtocolError::InvalidParameter("unknown resource_id"))?;

        let bpp = res.bytes_per_pixel()? as u64;
        let stride = res.width as u64 * bpp;
        if res.backing.is_empty() {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        // Bounds checks.
        if rect.x.checked_add(rect.width).unwrap_or(u32::MAX) > res.width
            || rect.y.checked_add(rect.height).unwrap_or(u32::MAX) > res.height
        {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        let row_bytes = rect.width as usize * bpp as usize;
        let mut row_buf = vec![0u8; row_bytes];

        for row in 0..rect.height as u64 {
            let src_off = offset
                .checked_add(row.checked_mul(stride).ok_or(ProtocolError::InvalidParameter(
                    "guest src overflow",
                ))?)
                .ok_or(ProtocolError::InvalidParameter("guest src overflow"))?;
            res.read_backing_linear(mem, src_off, &mut row_buf)?;

            let dst_pixel = ((rect.y as u64 + row) * res.width as u64 + rect.x as u64)
                .checked_mul(bpp)
                .ok_or(ProtocolError::InvalidParameter("dst overflow"))?;
            let dst_off = dst_pixel as usize;
            res.pixels_bgra[dst_off..dst_off + row_bytes].copy_from_slice(&row_buf);
        }

        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_set_scanout(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_set_scanout:
        // hdr + rect + scanout_id(u32) + resource_id(u32)
        let base = CtrlHdr::WIREFORMAT_SIZE;
        let want = base + 16 + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let rect = parse_rect(req_bytes, base)?;
        let scanout_id = read_u32_le(req_bytes, base + 16)? as usize;
        let resource_id = read_u32_le(req_bytes, base + 20)?;

        if scanout_id >= VIRTIO_GPU_MAX_SCANOUTS {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }
        if resource_id == 0 {
            // Disable scanout.
            self.scanouts[scanout_id].enabled = false;
            self.scanouts[scanout_id].resource_id = 0;
            self.scanouts[scanout_id].rect = Rect::new(0, 0, 0, 0);
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA));
        }
        if scanout_id != 0 {
            // This prototype only exposes one scanout buffer.
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }
        if !self.resources.contains_key(&resource_id) {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }
        if rect.x.checked_add(rect.width).unwrap_or(u32::MAX) > self.display_width
            || rect.y.checked_add(rect.height).unwrap_or(u32::MAX) > self.display_height
            || rect.width == 0
            || rect.height == 0
        {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        self.scanouts[scanout_id].enabled = true;
        self.scanouts[scanout_id].rect = rect;
        self.scanouts[scanout_id].resource_id = resource_id;
        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }

    fn cmd_resource_flush(&mut self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_resource_flush:
        // hdr + rect + resource_id(u32) + padding(u32)
        let base = CtrlHdr::WIREFORMAT_SIZE;
        let want = base + 16 + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let rect = parse_rect(req_bytes, base)?;
        let resource_id = read_u32_le(req_bytes, base + 16)?;

        let res = match self.resources.get(&resource_id) {
            Some(r) => r,
            None => return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER)),
        };

        // Bounds checks.
        if rect.x.checked_add(rect.width).unwrap_or(u32::MAX) > res.width
            || rect.y.checked_add(rect.height).unwrap_or(u32::MAX) > res.height
        {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        // If this resource isn't currently bound to scanout 0, nothing to do.
        let scanout = &self.scanouts[0];
        if !scanout.enabled || scanout.resource_id != resource_id {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA));
        }
        if scanout.rect.x != 0
            || scanout.rect.y != 0
            || scanout.rect.width != self.display_width
            || scanout.rect.height != self.display_height
        {
            // The prototype only supports scanout 0 mapping 1:1 to the display buffer.
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_UNSPEC));
        }
        if res.width != self.display_width || res.height != self.display_height {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_UNSPEC));
        }

        // Copy updated rect to "display".
        let bpp = 4usize;
        let src_stride = res.width as usize * bpp;
        let dst_stride = self.display_width as usize * bpp;
        let row_bytes = rect.width as usize * bpp;

        for row in 0..rect.height as usize {
            let src_row = (rect.y as usize + row) * src_stride;
            let src_off = src_row + rect.x as usize * bpp;
            let dst_row = (rect.y as usize + row) * dst_stride;
            let dst_off = dst_row + rect.x as usize * bpp;
            self.scanout_bgra[dst_off..dst_off + row_bytes]
                .copy_from_slice(&res.pixels_bgra[src_off..src_off + row_bytes]);
        }
        Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_NODATA))
    }
}
