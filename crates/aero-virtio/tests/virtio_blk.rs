use aero_virtio::devices::blk::{BlockBackend, VirtioBlk, VIRTIO_BLK_T_IN, VIRTIO_BLK_T_OUT};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct SharedDisk(Rc<RefCell<Vec<u8>>>);

impl BlockBackend for SharedDisk {
    fn len(&self) -> u64 {
        self.0.borrow().len() as u64
    }

    fn read_at(&mut self, offset: u64, dst: &mut [u8]) -> Result<(), ()> {
        let offset = offset as usize;
        dst.copy_from_slice(&self.0.borrow()[offset..offset + dst.len()]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, src: &[u8]) -> Result<(), ()> {
        let offset = offset as usize;
        self.0.borrow_mut()[offset..offset + src.len()].copy_from_slice(src);
        Ok(())
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

fn write_desc(mem: &mut GuestRam, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

#[test]
fn virtio_blk_enumerates_and_processes_requests() {
    let backing = Rc::new(RefCell::new(vec![0u8; 4096]));
    let backend = SharedDisk(backing.clone());

    let blk = VirtioBlk::new(backend);
    let mut dev = VirtioPciDevice::new(Box::new(blk), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    // BAR0 size probing (basic PCI correctness).
    dev.config_write(0x10, &0xffff_ffffu32.to_le_bytes());
    let mut bar = [0u8; 4];
    dev.config_read(0x10, &mut bar);
    let expected_mask = (!(dev.bar0_size() as u32 - 1)) & 0xffff_fff0;
    assert_eq!(u32::from_le_bytes(bar), expected_mask);
    dev.config_write(0x10, &0x8000_0000u32.to_le_bytes());
    dev.config_read(0x10, &mut bar);
    assert_eq!(u32::from_le_bytes(bar), 0x8000_0000);

    let caps = parse_caps(&dev);
    // `common` may legitimately be at BAR offset 0; the rest should be mapped.
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x10000);

    // Feature negotiation.
    bar_write_u8(&mut dev, &mut mem, caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    bar_write_u8(
        &mut dev,
        &mut mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, &mut mem, caps.common + 0x00, 0); // device_feature_select
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common + 0x08, 0); // driver_feature_select
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

    // Configure queue 0.
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, 0); // queue_select
    let qsz = bar_read_u16(&mut dev, caps.common + 0x18);
    assert!(qsz >= 8);

    let desc = 0x4000;
    let avail = 0x5000;
    let used = 0x6000;

    bar_write_u64(&mut dev, &mut mem, caps.common + 0x20, desc);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x28, avail);
    bar_write_u64(&mut dev, &mut mem, caps.common + 0x30, used);
    bar_write_u16(&mut dev, &mut mem, caps.common + 0x1c, 1); // queue_enable

    // Write request: OUT sector 1.
    let header = 0x7000;
    let data = 0x8000;
    let status = 0x9000;

    let sector = 1u64;
    write_u32_le(&mut mem, header, VIRTIO_BLK_T_OUT).unwrap();
    write_u32_le(&mut mem, header + 4, 0).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    let payload: Vec<u8> = (0..512u16).flat_map(|v| v.to_le_bytes()).collect();
    mem.write(data, &payload).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_desc(&mut mem, desc, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, desc, 1, data, payload.len() as u32, 0x0001, 2);
    write_desc(&mut mem, desc, 2, status, 1, 0x0002, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    // Kick queue 0.
    dev.bar0_write(caps.notify + 0 * u64::from(caps.notify_mult), &0u16.to_le_bytes(), &mut mem);

    assert_eq!(
        &backing.borrow()[(sector * 512) as usize..(sector * 512) as usize + payload.len()],
        payload.as_slice()
    );
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);

    // Read request: IN sector 1 into a new buffer.
    let data2 = 0xA000;
    mem.write(data2, &vec![0u8; payload.len()]).unwrap();
    mem.write(status, &[0xff]).unwrap();

    write_u32_le(&mut mem, header, VIRTIO_BLK_T_IN).unwrap();
    write_u64_le(&mut mem, header + 8, sector).unwrap();

    write_desc(&mut mem, desc, 0, header, 16, 0x0001, 1);
    write_desc(&mut mem, desc, 1, data2, payload.len() as u32, 0x0001 | 0x0002, 2);
    write_desc(&mut mem, desc, 2, status, 1, 0x0002, 0);

    // Add to avail ring at index 1.
    write_u16_le(&mut mem, avail + 4 + 2, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 2).unwrap();

    dev.bar0_write(caps.notify + 0 * u64::from(caps.notify_mult), &0u16.to_le_bytes(), &mut mem);

    let got = mem.get_slice(data2, payload.len()).unwrap();
    assert_eq!(got, payload.as_slice());
    assert_eq!(mem.get_slice(status, 1).unwrap()[0], 0);
}
