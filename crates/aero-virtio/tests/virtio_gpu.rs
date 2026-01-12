use aero_virtio::devices::gpu::{ScanoutSink, VirtioGpu2d};
use aero_virtio::memory::{
    read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use virtio_gpu_proto::protocol as proto;

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct SharedScanout(Rc<RefCell<Vec<u8>>>);

impl ScanoutSink for SharedScanout {
    fn present(&mut self, _width: u32, _height: u32, bgra: &[u8]) {
        *self.0.borrow_mut() = bgra.to_vec();
    }
}

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    isr: u64,
    device: u64,
    notify_mult: u32,
}

fn parse_caps(dev: &mut VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        let cap_id = cfg[ptr];
        let next = cfg[ptr + 1] as usize;
        if cap_id == 0x09 {
            let cap_len = cfg[ptr + 2] as usize;
            let cfg_type = cfg[ptr + 3];
            let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
                VIRTIO_PCI_CAP_NOTIFY_CFG => {
                    caps.notify = offset;
                    caps.notify_mult =
                        u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
                }
                VIRTIO_PCI_CAP_ISR_CFG => caps.isr = offset,
                VIRTIO_PCI_CAP_DEVICE_CFG => caps.device = offset,
                _ => {}
            }
            assert!(cap_len >= 16);
        }
        ptr = next;
    }

    caps
}

fn bar_read_u32(dev: &mut VirtioPciDevice, off: u64) -> u32 {
    let mut buf = [0u8; 4];
    dev.bar0_read(off, &mut buf);
    u32::from_le_bytes(buf)
}

fn bar_read_u16(dev: &mut VirtioPciDevice, off: u64) -> u16 {
    let mut buf = [0u8; 2];
    dev.bar0_read(off, &mut buf);
    u16::from_le_bytes(buf)
}

fn bar_write_u32(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u16(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u64(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u8(dev: &mut VirtioPciDevice, _mem: &mut GuestRam, off: u64, val: u8) {
    dev.bar0_write(off, &[val]);
}

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

fn ctrl_hdr(ty: u32) -> Vec<u8> {
    let mut out = Vec::new();
    proto::write_u32_le(&mut out, ty);
    proto::write_u32_le(&mut out, 0); // flags
    proto::write_u64_le(&mut out, 0); // fence_id
    proto::write_u32_le(&mut out, 0); // ctx_id
    proto::write_u32_le(&mut out, 0); // padding
    debug_assert_eq!(out.len(), proto::CtrlHdr::WIREFORMAT_SIZE);
    out
}

fn push_rect(out: &mut Vec<u8>, r: proto::Rect) {
    proto::write_u32_le(out, r.x);
    proto::write_u32_le(out, r.y);
    proto::write_u32_le(out, r.width);
    proto::write_u32_le(out, r.height);
}

struct ControlQueue {
    qsz: u16,
    desc: u64,
    avail: u64,
    used: u64,
    avail_idx: u16,
    used_idx: u16,
    req_addr: u64,
    resp_addr: u64,
}

impl ControlQueue {
    fn new(qsz: u16, desc: u64, avail: u64, used: u64, req_addr: u64, resp_addr: u64) -> Self {
        Self {
            qsz,
            desc,
            avail,
            used,
            avail_idx: 0,
            used_idx: 0,
            req_addr,
            resp_addr,
        }
    }
}

fn submit_control(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    ctrlq: &mut ControlQueue,
    req: &[u8],
    resp_capacity: usize,
) -> Vec<u8> {
    mem.write(ctrlq.req_addr, req).unwrap();
    mem.write(ctrlq.resp_addr, &vec![0u8; resp_capacity])
        .unwrap();

    // Request (read-only) + response (write-only).
    write_desc(
        mem,
        ctrlq.desc,
        0,
        ctrlq.req_addr,
        req.len() as u32,
        0x0001,
        1,
    );
    write_desc(
        mem,
        ctrlq.desc,
        1,
        ctrlq.resp_addr,
        resp_capacity as u32,
        0x0002,
        0,
    );

    let ring_index = u64::from(ctrlq.avail_idx % ctrlq.qsz);
    write_u16_le(mem, ctrlq.avail + 4 + ring_index * 2, 0).unwrap(); // head=0
    ctrlq.avail_idx = ctrlq.avail_idx.wrapping_add(1);
    write_u16_le(mem, ctrlq.avail + 2, ctrlq.avail_idx).unwrap();

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(mem);

    let used_ring_index = u64::from(ctrlq.used_idx % ctrlq.qsz);
    let used_elem = ctrlq.used + 4 + used_ring_index * 8;
    let len = read_u32_le(mem, used_elem + 4).unwrap() as usize;
    ctrlq.used_idx = ctrlq.used_idx.wrapping_add(1);

    mem.get_slice(ctrlq.resp_addr, len).unwrap().to_vec()
}

#[test]
fn virtio_gpu_2d_scanout_via_virtqueue() {
    let shared_scanout = Rc::new(RefCell::new(Vec::new()));
    let gpu = VirtioGpu2d::new(4, 4, SharedScanout(shared_scanout.clone()));
    let mut dev = VirtioPciDevice::new(Box::new(gpu), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    // Class code: display controller / other.
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    assert_eq!(cfg[0x0b], 0x03);
    assert_eq!(cfg[0x0a], 0x80);

    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x20000);

    // Feature negotiation (accept whatever the device offers).
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);

    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure queue 0 (controlq).
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    let qsz = bar_read_u16(&mut dev, caps.common + 0x18);
    assert!(qsz >= 8);

    let desc = 0x4000;
    let avail = 0x5000;
    let used = 0x6000;
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Init rings.
    write_u16_le(&mut mem, avail, 0).unwrap(); // flags
    write_u16_le(&mut mem, avail + 2, 0).unwrap(); // idx
    write_u16_le(&mut mem, used, 0).unwrap(); // flags
    write_u16_le(&mut mem, used + 2, 0).unwrap(); // idx

    let req_addr = 0x7000;
    let resp_addr = 0x8000;
    let mut ctrlq = ControlQueue::new(qsz, desc, avail, used, req_addr, resp_addr);

    // GET_DISPLAY_INFO.
    let resp = submit_control(
        &mut dev,
        &mut mem,
        &caps,
        &mut ctrlq,
        &ctrl_hdr(proto::VIRTIO_GPU_CMD_GET_DISPLAY_INFO),
        512,
    );
    assert_eq!(
        u32::from_le_bytes(resp[0..4].try_into().unwrap()),
        proto::VIRTIO_GPU_RESP_OK_DISPLAY_INFO
    );

    // GET_EDID (ensures large response writes correctly through virtqueue).
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_GET_EDID);
    proto::write_u32_le(&mut req, 0); // scanout_id
    proto::write_u32_le(&mut req, 0); // padding
    let resp = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 2048);
    assert_eq!(
        u32::from_le_bytes(resp[0..4].try_into().unwrap()),
        proto::VIRTIO_GPU_RESP_OK_EDID
    );
    let edid_size = u32::from_le_bytes(resp[24..28].try_into().unwrap());
    assert!(edid_size >= 128);

    // RESOURCE_CREATE_2D.
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_RESOURCE_CREATE_2D);
    proto::write_u32_le(&mut req, 1);
    proto::write_u32_le(&mut req, proto::VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM);
    proto::write_u32_le(&mut req, 4);
    proto::write_u32_le(&mut req, 4);
    let resp = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);
    assert_eq!(
        u32::from_le_bytes(resp[0..4].try_into().unwrap()),
        proto::VIRTIO_GPU_RESP_OK_NODATA
    );

    // Pixel backing split across two entries.
    let mut pixels = Vec::new();
    for i in 0u8..16 {
        pixels.extend_from_slice(&[i, 0x80, 0x40, 0xff]);
    }
    mem.write(0x9000, &pixels[..32]).unwrap();
    mem.write(0xA000, &pixels[32..]).unwrap();

    // RESOURCE_ATTACH_BACKING (2 entries).
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING);
    proto::write_u32_le(&mut req, 1); // resource_id
    proto::write_u32_le(&mut req, 2); // nr_entries
    proto::write_u64_le(&mut req, 0x9000);
    proto::write_u32_le(&mut req, 32);
    proto::write_u32_le(&mut req, 0);
    proto::write_u64_le(&mut req, 0xA000);
    proto::write_u32_le(&mut req, 32);
    proto::write_u32_le(&mut req, 0);
    let resp = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);
    assert_eq!(
        u32::from_le_bytes(resp[0..4].try_into().unwrap()),
        proto::VIRTIO_GPU_RESP_OK_NODATA
    );

    // TRANSFER_TO_HOST_2D (fullscreen).
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D);
    push_rect(&mut req, proto::Rect::new(0, 0, 4, 4));
    proto::write_u64_le(&mut req, 0);
    proto::write_u32_le(&mut req, 1);
    proto::write_u32_le(&mut req, 0);
    let _ = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);

    // SET_SCANOUT.
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_SET_SCANOUT);
    push_rect(&mut req, proto::Rect::new(0, 0, 4, 4));
    proto::write_u32_le(&mut req, 0); // scanout_id
    proto::write_u32_le(&mut req, 1); // resource_id
    let _ = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);

    // RESOURCE_FLUSH (fullscreen) -> triggers `present()`.
    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_RESOURCE_FLUSH);
    push_rect(&mut req, proto::Rect::new(0, 0, 4, 4));
    proto::write_u32_le(&mut req, 1); // resource_id
    proto::write_u32_le(&mut req, 0); // padding
    let _ = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);
    assert_eq!(&*shared_scanout.borrow(), pixels.as_slice());

    // Partial update: overwrite a 2x2 rect at (1,1).
    let rect = proto::Rect::new(1, 1, 2, 2);
    let stride = 16usize;
    let bpp = 4usize;
    let offset = (rect.y as usize * stride + rect.x as usize * bpp) as u64;

    let new_px = [0xaa, 0xbb, 0xcc, 0xff];
    let mut expected = pixels.clone();
    for dy in 0..rect.height as usize {
        for dx in 0..rect.width as usize {
            let x = rect.x as usize + dx;
            let y = rect.y as usize + dy;
            let idx = (y * 4 + x) * 4;
            expected[idx..idx + 4].copy_from_slice(&new_px);

            let backing_off = y * stride + x * bpp;
            let (addr_base, entry_off) = if backing_off < 32 {
                (0x9000u64, backing_off)
            } else {
                (0xA000u64, backing_off - 32)
            };
            mem.write(addr_base + entry_off as u64, &new_px).unwrap();
        }
    }

    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D);
    push_rect(&mut req, rect);
    proto::write_u64_le(&mut req, offset);
    proto::write_u32_le(&mut req, 1);
    proto::write_u32_le(&mut req, 0);
    let _ = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);

    let mut req = ctrl_hdr(proto::VIRTIO_GPU_CMD_RESOURCE_FLUSH);
    push_rect(&mut req, rect);
    proto::write_u32_le(&mut req, 1);
    proto::write_u32_le(&mut req, 0);
    let _ = submit_control(&mut dev, &mut mem, &caps, &mut ctrlq, &req, 64);

    assert_eq!(&*shared_scanout.borrow(), expected.as_slice());
}

#[test]
fn virtio_gpu_rejects_oversize_request_without_wedging_queue() {
    const MAX_REQ_BYTES: usize = 256 * 1024;

    let shared_scanout = Rc::new(RefCell::new(Vec::new()));
    let gpu = VirtioGpu2d::new(4, 4, SharedScanout(shared_scanout));
    let mut dev = VirtioPciDevice::new(Box::new(gpu), Box::new(InterruptLog::default()));

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x20000);

    // Feature negotiation (accept whatever the device offers).
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);

    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure queue 0 (controlq).
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0);
    let qsz = bar_read_u16(&mut dev, caps.common + 0x18);
    assert!(qsz >= 8);

    let desc = 0x4000;
    let avail = 0x5000;
    let used = 0x6000;
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // Init rings.
    write_u16_le(&mut mem, avail, 0).unwrap(); // flags
    write_u16_le(&mut mem, avail + 2, 0).unwrap(); // idx
    write_u16_le(&mut mem, used, 0).unwrap(); // flags
    write_u16_le(&mut mem, used + 2, 0).unwrap(); // idx

    let mut avail_idx = 0u16;
    let mut used_idx = 0u16;

    // Submit an oversized request descriptor chain. The device must reject it without trying to
    // buffer unbounded request bytes, but the transport must still advance used->idx so the queue
    // doesn't wedge.
    let req_addr = 0x7000;
    let resp_addr = 0x8000;
    mem.write(resp_addr, &[0u8; 64]).unwrap();
    write_desc(
        &mut mem,
        desc,
        0,
        req_addr,
        (MAX_REQ_BYTES as u32) + 1,
        0x0001,
        1,
    );
    write_desc(&mut mem, desc, 1, resp_addr, 64, 0x0002, 0);

    let ring_index = (avail_idx % qsz) as u64;
    write_u16_le(&mut mem, avail + 4 + ring_index * 2, 0).unwrap();
    avail_idx = avail_idx.wrapping_add(1);
    write_u16_le(&mut mem, avail + 2, avail_idx).unwrap();

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    let used_ring_index = (used_idx % qsz) as u64;
    let used_elem = used + 4 + used_ring_index * 8;
    assert_eq!(read_u32_le(&mem, used_elem).unwrap(), 0);
    assert_eq!(read_u32_le(&mem, used_elem + 4).unwrap(), 0);
    used_idx = used_idx.wrapping_add(1);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x16), 0);

    // A follow-up valid request must still complete (queue not wedged).
    let mut ctrlq = ControlQueue {
        qsz,
        desc,
        avail,
        used,
        avail_idx,
        used_idx,
        req_addr,
        resp_addr,
    };
    let resp = submit_control(
        &mut dev,
        &mut mem,
        &caps,
        &mut ctrlq,
        &ctrl_hdr(proto::VIRTIO_GPU_CMD_GET_DISPLAY_INFO),
        512,
    );
    assert_eq!(
        u32::from_le_bytes(resp[0..4].try_into().unwrap()),
        proto::VIRTIO_GPU_RESP_OK_DISPLAY_INFO
    );
}
