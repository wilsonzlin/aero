use aero_virtio::devices::snd::{
    VirtioSnd, CAPTURE_STREAM_ID, PLAYBACK_STREAM_ID, VIRTIO_SND_QUEUE_RX, VIRTIO_SND_QUEUE_TX,
    VIRTIO_SND_S_BAD_MSG,
};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG,
    VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE,
    VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

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

fn negotiate_features(dev: &mut VirtioPciDevice, mem: &mut GuestRam, caps: &Caps) {
    bar_write_u8(dev, mem, caps.common + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    bar_write_u8(
        dev,
        mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    bar_write_u32(dev, mem, caps.common, 0);
    let f0 = bar_read_u32(dev, caps.common + 0x04);
    bar_write_u32(dev, mem, caps.common + 0x08, 0);
    bar_write_u32(dev, mem, caps.common + 0x0c, f0);

    bar_write_u32(dev, mem, caps.common, 1);
    let f1 = bar_read_u32(dev, caps.common + 0x04);
    bar_write_u32(dev, mem, caps.common + 0x08, 1);
    bar_write_u32(dev, mem, caps.common + 0x0c, f1);

    bar_write_u8(
        dev,
        mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    bar_write_u8(
        dev,
        mem,
        caps.common + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );
}

fn configure_queue(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    queue_index: u16,
    desc: u64,
    avail: u64,
    used: u64,
) {
    bar_write_u16(dev, mem, caps.common + 0x16, queue_index);
    let qsz = bar_read_u16(dev, caps.common + 0x18);
    assert!(qsz >= 8);

    bar_write_u64(dev, mem, caps.common + 0x20, desc);
    bar_write_u64(dev, mem, caps.common + 0x28, avail);
    bar_write_u64(dev, mem, caps.common + 0x30, used);
    bar_write_u16(dev, mem, caps.common + 0x1c, 1);

    write_u16_le(mem, avail, 0).unwrap();
    write_u16_le(mem, avail + 2, 0).unwrap();
    write_u16_le(mem, used, 0).unwrap();
    write_u16_le(mem, used + 2, 0).unwrap();
}

fn kick_queue(dev: &mut VirtioPciDevice, mem: &mut GuestRam, caps: &Caps, queue: u16) {
    dev.bar0_write(
        caps.notify + u64::from(queue) * u64::from(caps.notify_mult),
        &queue.to_le_bytes(),
    );
    dev.process_notified_queues(mem);
}

#[test]
fn virtio_snd_tx_missing_status_descriptor_does_not_stall_queue() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x20000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let tx_desc = 0x1000;
    let tx_avail = 0x2000;
    let tx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_TX,
        tx_desc,
        tx_avail,
        tx_used,
    );

    let hdr_addr = 0x8000;
    write_u32_le(&mut mem, hdr_addr, PLAYBACK_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();

    // OUT header only, no IN descriptors for the status response.
    write_desc(&mut mem, tx_desc, 0, hdr_addr, 8, 0, 0);

    write_u16_le(&mut mem, tx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, tx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_TX);

    let used_idx = u16::from_le_bytes(mem.get_slice(tx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(tx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), 0);
}

#[test]
fn virtio_snd_tx_invalid_out_buffer_does_not_stall_queue() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x20000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let tx_desc = 0x1000;
    let tx_avail = 0x2000;
    let tx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_TX,
        tx_desc,
        tx_avail,
        tx_used,
    );

    let invalid_hdr_addr = 0x30000;
    let status_addr = 0x9000;
    mem.write(status_addr, &[0xffu8; 8]).unwrap();

    write_desc(
        &mut mem,
        tx_desc,
        0,
        invalid_hdr_addr,
        8,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut mem, tx_desc, 1, status_addr, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, tx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, tx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_TX);

    let used_idx = u16::from_le_bytes(mem.get_slice(tx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(tx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), 8);

    let status = u32::from_le_bytes(mem.get_slice(status_addr, 4).unwrap().try_into().unwrap());
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
}

#[test]
fn virtio_snd_rx_short_response_descriptor_does_not_stall_queue() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x20000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let rx_desc = 0x1000;
    let rx_avail = 0x2000;
    let rx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_RX,
        rx_desc,
        rx_avail,
        rx_used,
    );

    let hdr_addr = 0x8000;
    let resp_addr = 0x8100;
    write_u32_le(&mut mem, hdr_addr, CAPTURE_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();
    mem.write(resp_addr, &[0xffu8; 4]).unwrap();

    // OUT header + a single, too-short IN descriptor (should still complete).
    write_desc(&mut mem, rx_desc, 0, hdr_addr, 8, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut mem, rx_desc, 1, resp_addr, 4, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, rx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, rx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_RX);

    let used_idx = u16::from_le_bytes(mem.get_slice(rx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(rx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), 4);

    let status = u32::from_le_bytes(mem.get_slice(resp_addr, 4).unwrap().try_into().unwrap());
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
}

#[test]
fn virtio_snd_rx_invalid_header_buffer_does_not_stall_queue() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x20000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let rx_desc = 0x1000;
    let rx_avail = 0x2000;
    let rx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_RX,
        rx_desc,
        rx_avail,
        rx_used,
    );

    let invalid_hdr_addr = 0x30000;
    let payload_addr = 0x8100;
    let resp_addr = 0x8200;
    mem.write(payload_addr, &[0xffu8; 8]).unwrap();
    mem.write(resp_addr, &[0xffu8; 8]).unwrap();

    write_desc(
        &mut mem,
        rx_desc,
        0,
        invalid_hdr_addr,
        8,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &mut mem,
        rx_desc,
        1,
        payload_addr,
        8,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&mut mem, rx_desc, 2, resp_addr, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, rx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, rx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_RX);

    let used_idx = u16::from_le_bytes(mem.get_slice(rx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(rx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), 16);

    let payload = mem.get_slice(payload_addr, 8).unwrap();
    assert!(payload.iter().all(|&b| b == 0));

    let status = u32::from_le_bytes(mem.get_slice(resp_addr, 4).unwrap().try_into().unwrap());
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
}

#[test]
fn virtio_snd_tx_rejects_oversize_pcm_payload_without_stalling_queue() {
    // Keep this in sync with `aero_virtio::devices::snd`'s internal cap.
    const MAX_PCM_XFER_BYTES: u32 = 256 * 1024;

    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    // Large enough that the payload buffer is in-bounds (so the size cap, not OOB checks, drives
    // the error).
    let mut mem = GuestRam::new(0x1_00000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let tx_desc = 0x1000;
    let tx_avail = 0x2000;
    let tx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_TX,
        tx_desc,
        tx_avail,
        tx_used,
    );

    let hdr_addr = 0x8000;
    let pcm_addr = 0x9000;
    let status_addr = 0xa000;
    write_u32_le(&mut mem, hdr_addr, PLAYBACK_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();
    mem.write(status_addr, &[0xffu8; 8]).unwrap();

    // Header OUT descriptor, followed by an oversized PCM OUT descriptor, followed by an IN status
    // descriptor.
    write_desc(&mut mem, tx_desc, 0, hdr_addr, 8, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        tx_desc,
        1,
        pcm_addr,
        MAX_PCM_XFER_BYTES + 1,
        VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&mut mem, tx_desc, 2, status_addr, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, tx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, tx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_TX);

    let used_idx = u16::from_le_bytes(mem.get_slice(tx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(tx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), 8);

    let status = u32::from_le_bytes(mem.get_slice(status_addr, 4).unwrap().try_into().unwrap());
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
}

#[test]
fn virtio_snd_rx_rejects_oversize_payload_and_caps_silence_write() {
    // Keep this in sync with `aero_virtio::devices::snd`'s internal cap.
    const MAX_PCM_XFER_BYTES: u32 = 256 * 1024;

    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    // Large enough that the payload buffer is in-bounds.
    let mut mem = GuestRam::new(0x1_00000);
    negotiate_features(&mut dev, &mut mem, &caps);

    let rx_desc = 0x1000;
    let rx_avail = 0x2000;
    let rx_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_RX,
        rx_desc,
        rx_avail,
        rx_used,
    );

    let hdr_addr = 0x8000;
    let payload_addr = 0x9000;
    let payload_len = MAX_PCM_XFER_BYTES * 2; // oversized but still an even number of bytes
    let resp_addr = payload_addr + payload_len as u64;

    write_u32_le(&mut mem, hdr_addr, CAPTURE_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();

    // Seed payload/response with non-zero bytes so we can verify how much gets overwritten.
    mem.write(payload_addr, &vec![0xffu8; payload_len as usize])
        .unwrap();
    mem.write(resp_addr, &[0xffu8; 8]).unwrap();

    write_desc(&mut mem, rx_desc, 0, hdr_addr, 8, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &mut mem,
        rx_desc,
        1,
        payload_addr,
        payload_len,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&mut mem, rx_desc, 2, resp_addr, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, rx_avail + 4, 0).unwrap();
    write_u16_le(&mut mem, rx_avail + 2, 1).unwrap();

    kick_queue(&mut dev, &mut mem, &caps, VIRTIO_SND_QUEUE_RX);

    let used_idx = u16::from_le_bytes(mem.get_slice(rx_used + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);

    let elem = mem.get_slice(rx_used + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    // Payload write should be capped to MAX_PCM_XFER_BYTES, plus 8 bytes for the response.
    assert_eq!(
        u32::from_le_bytes(elem[4..8].try_into().unwrap()),
        MAX_PCM_XFER_BYTES + 8
    );

    let payload_head = mem.get_slice(payload_addr, 8).unwrap();
    assert!(payload_head.iter().all(|&b| b == 0));
    // Byte just past the cap should remain untouched.
    let payload_after_cap = mem
        .get_slice(payload_addr + u64::from(MAX_PCM_XFER_BYTES), 1)
        .unwrap();
    assert_eq!(payload_after_cap[0], 0xff);

    let status = u32::from_le_bytes(mem.get_slice(resp_addr, 4).unwrap().try_into().unwrap());
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
}
