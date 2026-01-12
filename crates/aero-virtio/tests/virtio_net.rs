use aero_virtio::devices::net::{LoopbackNet, NetBackend, VirtioNet};
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::memory::{
    read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
};
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
struct SharedNet(Rc<RefCell<LoopbackNet>>);

impl NetBackend for SharedNet {
    fn transmit(&mut self, packet: Vec<u8>) {
        self.0.borrow_mut().tx_packets.push(packet);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        let mut net = self.0.borrow_mut();
        if net.rx_packets.is_empty() {
            None
        } else {
            Some(net.rx_packets.remove(0))
        }
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
        assert_eq!(cfg[ptr], 0x09);
        let next = cfg[ptr + 1] as usize;
        let cfg_type = cfg[ptr + 3];
        let offset = u32::from_le_bytes(cfg[ptr + 8..ptr + 12].try_into().unwrap()) as u64;
        match cfg_type {
            VIRTIO_PCI_CAP_COMMON_CFG => caps.common = offset,
            VIRTIO_PCI_CAP_NOTIFY_CFG => {
                caps.notify = offset;
                caps.notify_mult = u32::from_le_bytes(cfg[ptr + 16..ptr + 20].try_into().unwrap());
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

fn bar_write_u32(dev: &mut VirtioPciDevice, off: u64, val: u32) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u16(dev: &mut VirtioPciDevice, off: u64, val: u16) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u64(dev: &mut VirtioPciDevice, off: u64, val: u64) {
    dev.bar0_write(off, &val.to_le_bytes());
}

fn bar_write_u8(dev: &mut VirtioPciDevice, off: u64, val: u8) {
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

#[test]
fn virtio_net_tx_and_rx_complete_via_pci_transport() {
    let backing = Rc::new(RefCell::new(LoopbackNet::default()));
    let backend = SharedNet(backing.clone());

    let net = VirtioNet::new(backend, [0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut dev = VirtioPciDevice::new(Box::new(net), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x20000);

    // Feature negotiation: accept everything the device offers.
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

    // Contract v1 config layout: mac + status + max_virtqueue_pairs.
    let mut cfg = [0u8; 10];
    dev.bar0_read(caps.device, &mut cfg);
    assert_eq!(&cfg[0..6], &[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let link_status = u16::from_le_bytes(cfg[6..8].try_into().unwrap());
    assert_ne!(link_status & 1, 0);
    let max_pairs = u16::from_le_bytes(cfg[8..10].try_into().unwrap());
    assert_eq!(max_pairs, 1);

    // Configure RX queue 0.
    let rx_desc = 0x1000;
    let rx_avail = 0x2000;
    let rx_used = 0x3000;
    bar_write_u16(&mut dev, caps.common + 0x16, 0);
    assert!(bar_read_u16(&mut dev, caps.common + 0x18) >= 8);
    bar_write_u64(&mut dev, caps.common + 0x20, rx_desc);
    bar_write_u64(&mut dev, caps.common + 0x28, rx_avail);
    bar_write_u64(&mut dev, caps.common + 0x30, rx_used);
    bar_write_u16(&mut dev, caps.common + 0x1c, 1);

    // Configure TX queue 1.
    let tx_desc = 0x4000;
    let tx_avail = 0x5000;
    let tx_used = 0x6000;
    bar_write_u16(&mut dev, caps.common + 0x16, 1);
    assert!(bar_read_u16(&mut dev, caps.common + 0x18) >= 8);
    bar_write_u64(&mut dev, caps.common + 0x20, tx_desc);
    bar_write_u64(&mut dev, caps.common + 0x28, tx_avail);
    bar_write_u64(&mut dev, caps.common + 0x30, tx_used);
    bar_write_u16(&mut dev, caps.common + 0x1c, 1);

    // TX: header + payload.
    let hdr_addr = 0x7000;
    let payload_addr = 0x7100;
    let hdr = [0u8; VirtioNetHdr::BASE_LEN];
    // Contract v1: virtio-net frames must be between 14 and 1514 bytes.
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    mem.write(hdr_addr, &hdr).unwrap();
    mem.write(payload_addr, payload).unwrap();

    write_desc(
        &mut mem,
        tx_desc,
        0,
        hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &mut mem,
        tx_desc,
        1,
        payload_addr,
        payload.len() as u32,
        0,
        0,
    );

    write_u16_le(&mut mem, tx_avail, 0).unwrap();
    write_u16_le(&mut mem, tx_avail + 2, 1).unwrap();
    write_u16_le(&mut mem, tx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, tx_used, 0).unwrap();
    write_u16_le(&mut mem, tx_used + 2, 0).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(caps.notify_mult),
        &1u16.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);

    assert_eq!(backing.borrow().tx_packets, vec![payload.to_vec()]);
    assert_eq!(read_u16_le(&mem, tx_used + 2).unwrap(), 1);
    assert_eq!(read_u32_le(&mem, tx_used + 8).unwrap(), 0);

    // RX: guest posts a buffer, then host delivers a packet later.
    let rx_hdr_addr = 0x7200;
    let rx_payload_addr = 0x7300;
    mem.write(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN])
        .unwrap();
    mem.write(rx_payload_addr, &[0xbb; 64]).unwrap();

    write_desc(
        &mut mem,
        rx_desc,
        0,
        rx_hdr_addr,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        &mut mem,
        rx_desc,
        1,
        rx_payload_addr,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_u16_le(&mut mem, rx_avail, 0).unwrap();
    write_u16_le(&mut mem, rx_avail + 2, 1).unwrap();
    write_u16_le(&mut mem, rx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, rx_used, 0).unwrap();
    write_u16_le(&mut mem, rx_used + 2, 0).unwrap();

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);
    assert_eq!(read_u16_le(&mem, rx_used + 2).unwrap(), 0);

    let rx_packet = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00".to_vec();
    backing.borrow_mut().rx_packets.push(rx_packet.clone());
    dev.poll(&mut mem);

    let used_idx = read_u16_le(&mem, rx_used + 2).unwrap();
    assert_eq!(used_idx, 1);
    assert_eq!(
        read_u32_le(&mem, rx_used + 8).unwrap(),
        (VirtioNetHdr::BASE_LEN + rx_packet.len()) as u32
    );

    let expected_hdr = [0u8; VirtioNetHdr::BASE_LEN];
    assert_eq!(
        mem.get_slice(rx_hdr_addr, expected_hdr.len()).unwrap(),
        &expected_hdr
    );
    assert_eq!(
        mem.get_slice(rx_payload_addr, rx_packet.len()).unwrap(),
        rx_packet.as_slice()
    );
}

#[test]
fn virtio_net_drops_frame_when_buffer_insufficient_without_consuming_chain() {
    let backing = Rc::new(RefCell::new(LoopbackNet::default()));
    let backend = SharedNet(backing.clone());

    let net = VirtioNet::new(backend, [0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let mut dev = VirtioPciDevice::new(Box::new(net), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x20000);

    // Feature negotiation: accept everything the device offers.
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

    // Configure RX queue 0.
    let rx_desc = 0x1000;
    let rx_avail = 0x2000;
    let rx_used = 0x3000;
    bar_write_u16(&mut dev, caps.common + 0x16, 0);
    bar_write_u64(&mut dev, caps.common + 0x20, rx_desc);
    bar_write_u64(&mut dev, caps.common + 0x28, rx_avail);
    bar_write_u64(&mut dev, caps.common + 0x30, rx_used);
    bar_write_u16(&mut dev, caps.common + 0x1c, 1);

    // Post an RX buffer that is large enough for small frames but not for a 60-byte frame.
    let rx_hdr_addr = 0x4000;
    let rx_payload_addr = 0x4100;
    mem.write(rx_hdr_addr, &[0xaa; VirtioNetHdr::BASE_LEN])
        .unwrap();
    mem.write(rx_payload_addr, &[0xbb; 32]).unwrap();

    write_desc(
        &mut mem,
        rx_desc,
        0,
        rx_hdr_addr,
        VirtioNetHdr::BASE_LEN as u32,
        VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
        1,
    );
    write_desc(
        &mut mem,
        rx_desc,
        1,
        rx_payload_addr,
        32,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_u16_le(&mut mem, rx_avail, 0).unwrap();
    write_u16_le(&mut mem, rx_avail + 2, 1).unwrap();
    write_u16_le(&mut mem, rx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, rx_used, 0).unwrap();
    write_u16_le(&mut mem, rx_used + 2, 0).unwrap();

    dev.bar0_write(caps.notify, &0u16.to_le_bytes());
    dev.process_notified_queues(&mut mem);

    // Provide a 60-byte frame. The device must drop it and must NOT consume the chain.
    backing.borrow_mut().rx_packets.push(vec![0u8; 60]);
    dev.poll(&mut mem);
    assert_eq!(read_u16_le(&mem, rx_used + 2).unwrap(), 0);

    // Provide a minimal 14-byte Ethernet frame; it should now be delivered using the same chain.
    let small_frame = b"\xaa\xbb\xcc\xdd\xee\xff\x00\x01\x02\x03\x04\x05\x08\x00".to_vec();
    backing.borrow_mut().rx_packets.push(small_frame.clone());
    dev.poll(&mut mem);
    assert_eq!(read_u16_le(&mem, rx_used + 2).unwrap(), 1);
    assert_eq!(
        read_u32_le(&mem, rx_used + 8).unwrap(),
        (VirtioNetHdr::BASE_LEN + small_frame.len()) as u32
    );
    assert_eq!(
        mem.get_slice(rx_payload_addr, small_frame.len()).unwrap(),
        small_frame.as_slice()
    );
}
