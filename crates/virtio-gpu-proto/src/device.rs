use std::collections::HashMap;

use crate::protocol::{
    encode_resp_hdr_from_req, parse_ctrl_hdr, parse_rect, read_u32_le, read_u64_le, CtrlHdr, ProtocolError,
    Rect, VIRTIO_GPU_CMD_GET_DISPLAY_INFO, VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
    VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING, VIRTIO_GPU_CMD_RESOURCE_FLUSH, VIRTIO_GPU_CMD_RESOURCE_UNREF,
    VIRTIO_GPU_CMD_SET_SCANOUT, VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D, VIRTIO_GPU_CMD_UPDATE_CURSOR,
    VIRTIO_GPU_CMD_MOVE_CURSOR, VIRTIO_GPU_CMD_GET_EDID, VIRTIO_GPU_EDID_BLOB_SIZE,
    VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM, VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM, VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM,
    VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM, VIRTIO_GPU_MAX_SCANOUTS, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER,
    VIRTIO_GPU_RESP_ERR_UNSPEC, VIRTIO_GPU_RESP_OK_DISPLAY_INFO, VIRTIO_GPU_RESP_OK_EDID,
    VIRTIO_GPU_RESP_OK_NODATA,
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
            VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM
            | VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
            | VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM
            | VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM => Ok(4),
            _ => Err(ProtocolError::InvalidParameter("unsupported resource format")),
        }
    }

    fn needs_opaque_alpha(&self) -> bool {
        matches!(
            self.format,
            VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM | VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM
        )
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

    /// Current scanout dimensions (scanout 0).
    ///
    /// Note: this is treated as the "current output mode" for the prototype, and may change when
    /// the guest calls `SET_SCANOUT` with a different `rect`.
    pub fn display_size(&self) -> (u32, u32) {
        (self.display_width, self.display_height)
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
            VIRTIO_GPU_CMD_GET_EDID => self.cmd_get_edid(&hdr, req_bytes),
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

    fn cmd_get_edid(&self, req: &CtrlHdr, req_bytes: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        // struct virtio_gpu_cmd_get_edid:
        // hdr + scanout_id(u32) + padding(u32)
        let base = CtrlHdr::WIREFORMAT_SIZE;
        let want = base + 8;
        if req_bytes.len() < want {
            return Err(ProtocolError::BufferTooShort {
                want,
                got: req_bytes.len(),
            });
        }
        let scanout_id = read_u32_le(req_bytes, base)? as usize;
        if scanout_id >= VIRTIO_GPU_MAX_SCANOUTS {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        // Return a minimal, valid EDID blob. Many guest drivers only need *some* EDID in order to
        // expose a stable set of modes.
        let edid_128 = default_edid_1024x768();

        // struct virtio_gpu_resp_edid:
        // hdr + size(u32) + padding(u32) + edid[1024]
        let mut out = encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_OK_EDID);
        out.extend_from_slice(&(edid_128.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&edid_128);
        out.resize(out.len() + (VIRTIO_GPU_EDID_BLOB_SIZE - edid_128.len()), 0);
        Ok(out)
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
        if !matches!(
            format,
            VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM
                | VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM
                | VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM
                | VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM
        ) {
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
        let needs_opaque_alpha = res.needs_opaque_alpha();

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
            let dst = &mut res.pixels_bgra[dst_off..dst_off + row_bytes];
            dst.copy_from_slice(&row_buf);
            if needs_opaque_alpha {
                for px in dst.chunks_exact_mut(4) {
                    px[3] = 0xff;
                }
            }
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
        let Some(res) = self.resources.get(&resource_id) else {
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        };
        if rect.x != 0 || rect.y != 0 || rect.width == 0 || rect.height == 0 {
            // Keep the prototype simple: we only support scanout 0 being mapped 1:1 to the
            // output buffer (no panning / multi-rect scanouts).
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        if res.width != rect.width || res.height != rect.height {
            // The resource must match the scanout rect for a 1:1 scanout mapping.
            return Ok(encode_resp_hdr_from_req(req, VIRTIO_GPU_RESP_ERR_INVALID_PARAMETER));
        }

        // Treat the requested rect as the "current mode" and resize the scanout buffer.
        if rect.width != self.display_width || rect.height != self.display_height {
            let new_len = u64::from(rect.width)
                .checked_mul(u64::from(rect.height))
                .and_then(|v| v.checked_mul(4))
                .ok_or(ProtocolError::InvalidParameter("scanout size overflow"))?;
            let new_len = usize::try_from(new_len)
                .map_err(|_| ProtocolError::InvalidParameter("scanout size overflow"))?;
            self.scanout_bgra.resize(new_len, 0);
            self.display_width = rect.width;
            self.display_height = rect.height;
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

fn default_edid_1024x768() -> [u8; 128] {
    // A minimal, "good enough" EDID 1.4 blob advertising a 1024x768@60 preferred mode.
    //
    // This is intentionally simple (one detailed timing descriptor + blank fillers). It's not
    // intended to model a specific real monitor.
    let mut edid = [0u8; 128];
    edid[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);

    // Manufacturer ID "AER" (encoded as 5-bit letters, big-endian).
    // A=1, E=5, R=18 => 0b00001_00101_10010 = 0x04B2.
    edid[8] = 0x04;
    edid[9] = 0xb2;

    // Product code (little-endian) + serial.
    edid[10] = 0x01;
    edid[11] = 0x00;
    edid[12..16].copy_from_slice(&0u32.to_le_bytes());

    // Manufacture week/year.
    edid[16] = 1;
    edid[17] = 36; // 1990 + 36 = 2026

    // EDID version/revision.
    edid[18] = 1;
    edid[19] = 4;

    // Digital input.
    edid[20] = 0x80;
    // Screen size (cm) - unknown/unspecified.
    edid[21] = 0;
    edid[22] = 0;
    // Gamma: 2.2 => (2.2*100)-100 = 120.
    edid[23] = 120;
    // Features: default to RGB + preferred timing.
    edid[24] = 0x0a;

    // Chromaticity coordinates (leave as zero).
    // Established timings: 640x480@60, 800x600@60, 1024x768@60.
    edid[35] = 0x21; // 640x480@60 (bit0) + 800x600@60 (bit5)
    edid[36] = 0x08; // 1024x768@60 (bit3)
    edid[37] = 0x00;

    // Standard timings: unused (0x01 0x01).
    for i in 0..8 {
        edid[38 + i * 2] = 0x01;
        edid[38 + i * 2 + 1] = 0x01;
    }

    // Detailed timing descriptor 1: 1024x768 @ 60Hz (VESA).
    // Pixel clock: 65.00 MHz (6500 * 10kHz) => 0x1964 (LE).
    let dtd = 54;
    edid[dtd + 0] = 0x64;
    edid[dtd + 1] = 0x19;
    // H active/blanking: 1024 / 320.
    edid[dtd + 2] = 0x00; // 1024 LSB
    edid[dtd + 3] = 0x40; // 320 LSB
    edid[dtd + 4] = 0x41; // 1024 MSB=4, 320 MSB=1
    // V active/blanking: 768 / 38.
    edid[dtd + 5] = 0x00; // 768 LSB
    edid[dtd + 6] = 0x26; // 38 LSB
    edid[dtd + 7] = 0x30; // 768 MSB=3, 38 MSB=0
    // Sync offsets/pulse widths: hsync 24/136, vsync 3/6.
    edid[dtd + 8] = 0x18;
    edid[dtd + 9] = 0x88;
    edid[dtd + 10] = 0x36;
    edid[dtd + 11] = 0x00; // high bits
    // Physical size (mm) - unknown.
    edid[dtd + 12] = 0;
    edid[dtd + 13] = 0;
    edid[dtd + 14] = 0;
    edid[dtd + 15] = 0;
    edid[dtd + 16] = 0;
    // Separate sync, +H +V (bit4 + bits1..0).
    edid[dtd + 17] = 0x13;

    // Descriptor 2: monitor name "AERO".
    let name = 72;
    edid[name + 0..name + 18].copy_from_slice(&[
        0x00, 0x00, 0x00, 0xfc, 0x00, b'A', b'E', b'R', b'O', b'\n', 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ]);

    // Descriptors 3/4 unused.
    for base in [90usize, 108usize] {
        edid[base + 0..base + 18].copy_from_slice(&[
            0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
    }

    // No extensions.
    edid[126] = 0;

    // Checksum: sum of all 128 bytes must be 0 mod 256.
    let sum: u8 = edid.iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    edid[127] = (0u8).wrapping_sub(sum);
    edid
}
