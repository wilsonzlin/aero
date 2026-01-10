use aero_virtio::devices::snd::{SndOutput, VirtioSnd, VIRTIO_SND_QUEUE_TX};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct CaptureOutput(Rc<RefCell<Vec<f32>>>);

impl SndOutput for CaptureOutput {
    fn push_interleaved_stereo_f32(&mut self, samples: &[f32]) {
        self.0.borrow_mut().extend_from_slice(samples);
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

fn parse_caps(dev: &VirtioPciDevice) -> Caps {
    let mut cfg = [0u8; 256];
    dev.config_read(0, &mut cfg);
    let mut caps = Caps::default();

    let mut ptr = cfg[0x34] as usize;
    while ptr != 0 {
        assert_eq!(cfg[ptr], 0x09);
        let next = cfg[ptr + 1] as usize;
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

fn bar_write_u32(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u16(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u64(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes(), mem);
}

fn bar_write_u8(dev: &mut VirtioPciDevice, mem: &mut GuestRam, off: u64, val: u8) {
    dev.bar0_write(off, &[val], mem);
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

#[test]
fn virtio_snd_tx_pushes_samples_to_backend() {
    let samples = Rc::new(RefCell::new(Vec::<f32>::new()));
    let output = CaptureOutput(samples.clone());
    let snd = VirtioSnd::new(output, 48_000);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    let caps = parse_caps(&dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x20000);

    // Feature negotiation: accept everything the device offers.
    bar_write_u8(&mut dev, &mut mem, caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x0c, f0);

    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 1);
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

    // Configure TX queue.
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, VIRTIO_SND_QUEUE_TX);
    let qsz = bar_read_u16(&mut dev, caps.common + 0x18);
    assert!(qsz >= 8);

    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1);

    // PCM payload: 2 frames of 16-bit stereo.
    let pcm_addr = 0x4000;
    let status_addr = 0x5000;
    let pcm: [i16; 4] = [0, 16_384, -16_384, 0];
    let mut pcm_bytes = Vec::new();
    for v in pcm {
        pcm_bytes.extend_from_slice(&v.to_le_bytes());
    }
    mem.write(pcm_addr, &pcm_bytes).unwrap();
    mem.write(status_addr, &[0xff]).unwrap();

    write_desc(
        &mut mem,
        desc,
        0,
        pcm_addr,
        pcm_bytes.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut mem, desc, 1, status_addr, 1, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(VIRTIO_SND_QUEUE_TX) * u64::from(caps.notify_mult),
        &VIRTIO_SND_QUEUE_TX.to_le_bytes(),
        &mut mem,
    );

    assert_eq!(mem.get_slice(status_addr, 1).unwrap()[0], 0);

    let got = samples.borrow().clone();
    assert_eq!(got.len(), 4);
    let expect = [
        0.0f32,
        16_384.0f32 / 32_768.0,
        -16_384.0f32 / 32_768.0,
        0.0,
    ];
    for (g, e) in got.iter().zip(expect.iter()) {
        assert!((g - e).abs() < 1e-6, "got {g} expected {e}");
    }
}

