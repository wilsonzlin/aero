use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind, VirtioInputEvent};
use aero_virtio::memory::{
    read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory,
    GuestMemoryError,
};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;

#[derive(Debug)]
struct WindowedGuestMemory {
    base: u64,
    data: Vec<u8>,
}

impl WindowedGuestMemory {
    fn new(base: u64, size: usize) -> Self {
        Self {
            base,
            data: vec![0; size],
        }
    }

    fn translate(&self, addr: u64, len: usize) -> Result<(usize, usize), GuestMemoryError> {
        if addr < self.base {
            return Err(GuestMemoryError::OutOfBounds { addr, len });
        }
        let end = addr
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr, len })?;
        let start = addr - self.base;
        let end = end - self.base;
        if end > self.data.len() as u64 {
            return Err(GuestMemoryError::OutOfBounds { addr, len });
        }
        Ok((start as usize, end as usize))
    }
}

impl GuestMemory for WindowedGuestMemory {
    fn len(&self) -> u64 {
        self.base + self.data.len() as u64
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        dst.copy_from_slice(self.get_slice(addr, dst.len())?);
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        self.get_slice_mut(addr, src.len())?.copy_from_slice(src);
        Ok(())
    }

    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError> {
        let (start, end) = self.translate(addr, len)?;
        Ok(&self.data[start..end])
    }

    fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
        let (start, end) = self.translate(addr, len)?;
        Ok(&mut self.data[start..end])
    }
}

#[derive(Default)]
struct Caps {
    common: u64,
    notify: u64,
    notify_mult: u32,
}

fn parse_caps(dev: &mut VirtioPciDevice) -> Caps {
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
                assert!(cap_len >= 20);
                caps.notify = offset;
                caps.notify_mult = u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
            }
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

fn bar_write_u8(dev: &mut VirtioPciDevice, off: u64, val: u8) {
    dev.bar0_write(off, &[val]);
}

fn bar_write_u16(dev: &mut VirtioPciDevice, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u32(dev: &mut VirtioPciDevice, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u64_split(dev: &mut VirtioPciDevice, off: u64, val: u64) {
    bar_write_u32(dev, off, val as u32);
    bar_write_u32(dev, off + 4, (val >> 32) as u32);
}

#[test]
fn win7_contract_accepts_dma_addresses_above_4gib() {
    const BASE: u64 = 0x1_0000_0000;

    let input = VirtioInput::new(VirtioInputDeviceKind::Keyboard);
    let mut dev = VirtioPciDevice::new(Box::new(input), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);
    assert_eq!(caps.notify_mult, 4);

    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0004u16.to_le_bytes());

    let mut mem = WindowedGuestMemory::new(BASE, 0x20000);

    // Standard virtio modern init + feature negotiation (accept what the device offers).
    bar_write_u8(&mut dev, caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(&mut dev, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, caps.common + 0x08, 0);
    bar_write_u32(&mut dev, caps.common + 0x0c, f0);

    bar_write_u32(&mut dev, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, caps.common + 0x08, 1);
    bar_write_u32(&mut dev, caps.common + 0x0c, f1);

    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        &mut dev,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure virtio-input queue 0 (eventq) with ring addresses above 4GiB.
    let desc = BASE + 0x1000;
    let avail = BASE + 0x2000;
    let used = BASE + 0x3000;

    bar_write_u16(&mut dev, caps.common + 0x16, 0);
    bar_write_u64_split(&mut dev, caps.common + 0x20, desc);
    bar_write_u64_split(&mut dev, caps.common + 0x28, avail);
    bar_write_u64_split(&mut dev, caps.common + 0x30, used);
    bar_write_u16(&mut dev, caps.common + 0x1c, 1);

    // Post a single 8-byte event buffer, also above 4GiB.
    let event_buf = BASE + 0x4000;
    mem.write(event_buf, &[0u8; 8]).unwrap();
    write_u64_le(&mut mem, desc, event_buf).unwrap();
    write_u32_le(&mut mem, desc + 8, 8).unwrap();
    write_u16_le(&mut mem, desc + 12, VIRTQ_DESC_F_WRITE).unwrap();
    write_u16_le(&mut mem, desc + 14, 0).unwrap();

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    // Kicking the queue only makes the buffer available; without an event, it must not complete.
    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);
    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 0);

    // Deliver an event; the device should write it into the >4GiB buffer and complete the chain.
    let event = VirtioInputEvent {
        type_: 1,
        code: 30,
        value: 1,
    };
    dev.device_mut::<VirtioInput>().unwrap().push_event(event);
    dev.poll(&mut mem);

    assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
    assert_eq!(read_u32_le(&mem, used + 4).unwrap(), 0);
    assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);

    let bytes = mem.get_slice(event_buf, 8).unwrap();
    let got = VirtioInputEvent {
        type_: u16::from_le_bytes(bytes[0..2].try_into().unwrap()),
        code: u16::from_le_bytes(bytes[2..4].try_into().unwrap()),
        value: i32::from_le_bytes(bytes[4..8].try_into().unwrap()),
    };
    assert_eq!(got, event);
}
