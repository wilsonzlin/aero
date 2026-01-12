use aero_virtio::devices::snd::{
    AudioCaptureSource, VirtioSnd, CAPTURE_STREAM_ID, VIRTIO_SND_PCM_FMT_S16,
    VIRTIO_SND_PCM_RATE_48000, VIRTIO_SND_QUEUE_CONTROL, VIRTIO_SND_QUEUE_RX,
    VIRTIO_SND_R_PCM_PREPARE, VIRTIO_SND_R_PCM_SET_PARAMS, VIRTIO_SND_R_PCM_START,
    VIRTIO_SND_S_IO_ERR, VIRTIO_SND_S_OK,
};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG,
    VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE,
    VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

#[derive(Debug, Clone)]
struct TestCaptureSource {
    samples: Vec<f32>,
    pos: usize,
    dropped: u64,
}

impl AudioCaptureSource for TestCaptureSource {
    fn read_mono_f32(&mut self, dst: &mut [f32]) -> usize {
        let remaining = self.samples.len().saturating_sub(self.pos);
        let take = remaining.min(dst.len());
        if take == 0 {
            return 0;
        }
        dst[..take].copy_from_slice(&self.samples[self.pos..self.pos + take]);
        self.pos += take;
        take
    }

    fn take_dropped_samples(&mut self) -> u64 {
        let dropped = self.dropped;
        self.dropped = 0;
        dropped
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

    // Initialise rings (flags/idx).
    write_u16_le(mem, avail, 0).unwrap();
    write_u16_le(mem, avail + 2, 0).unwrap();
    write_u16_le(mem, used, 0).unwrap();
    write_u16_le(mem, used + 2, 0).unwrap();
}

struct ChainSubmit {
    queue_index: u16,
    desc_table: u64,
    avail_addr: u64,
    avail_idx: u16,
    out_addr: u64,
    out_len: u32,
    in_addr: u64,
    in_len: u32,
}

struct RxChainSubmit {
    desc_table: u64,
    avail_addr: u64,
    avail_idx: u16,
    hdr_addr: u64,
    payload_addr: u64,
    payload_len: u32,
    resp_addr: u64,
}

fn submit_chain(dev: &mut VirtioPciDevice, mem: &mut GuestRam, caps: &Caps, submit: ChainSubmit) {
    write_desc(
        mem,
        submit.desc_table,
        0,
        submit.out_addr,
        submit.out_len,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        mem,
        submit.desc_table,
        1,
        submit.in_addr,
        submit.in_len,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // Add to avail ring.
    let elem_addr = submit.avail_addr + 4 + u64::from(submit.avail_idx) * 2;
    write_u16_le(mem, elem_addr, 0).unwrap();
    write_u16_le(mem, submit.avail_addr + 2, submit.avail_idx.wrapping_add(1)).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(submit.queue_index) * u64::from(caps.notify_mult),
        &submit.queue_index.to_le_bytes(),
    );
    dev.process_notified_queues(mem);
}

fn submit_rx_chain(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    submit: RxChainSubmit,
) {
    write_desc(
        mem,
        submit.desc_table,
        0,
        submit.hdr_addr,
        8,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        mem,
        submit.desc_table,
        1,
        submit.payload_addr,
        submit.payload_len,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(
        mem,
        submit.desc_table,
        2,
        submit.resp_addr,
        8,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    let elem_addr = submit.avail_addr + 4 + u64::from(submit.avail_idx) * 2;
    write_u16_le(mem, elem_addr, 0).unwrap();
    write_u16_le(mem, submit.avail_addr + 2, submit.avail_idx.wrapping_add(1)).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(VIRTIO_SND_QUEUE_RX) * u64::from(caps.notify_mult),
        &VIRTIO_SND_QUEUE_RX.to_le_bytes(),
    );
    dev.process_notified_queues(mem);
}

#[test]
fn virtio_snd_rx_captures_samples() {
    let capture = TestCaptureSource {
        samples: vec![0.0, 0.5, -0.5, 2.0, 0.25],
        pos: 0,
        dropped: 7,
    };

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x40000);

    // Feature negotiation: accept everything the device offers.
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

    // Configure control and RX queues.
    let ctrl_desc = 0x1000;
    let ctrl_avail = 0x2000;
    let ctrl_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_CONTROL,
        ctrl_desc,
        ctrl_avail,
        ctrl_used,
    );

    let rx_desc = 0x4000;
    let rx_avail = 0x5000;
    let rx_used = 0x6000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_RX,
        rx_desc,
        rx_avail,
        rx_used,
    );

    // Drive the capture state machine: SET_PARAMS -> PREPARE -> START.
    let ctrl_req = 0x7000;
    let ctrl_resp = 0x7100;
    let mut ctrl_avail_idx = 0u16;

    let mut set_params = [0u8; 24];
    set_params[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    set_params[4..8].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
    set_params[8..12].copy_from_slice(&4096u32.to_le_bytes());
    set_params[12..16].copy_from_slice(&1024u32.to_le_bytes());
    set_params[16..20].copy_from_slice(&0u32.to_le_bytes());
    set_params[20] = 1;
    set_params[21] = VIRTIO_SND_PCM_FMT_S16;
    set_params[22] = VIRTIO_SND_PCM_RATE_48000;
    mem.write(ctrl_req, &set_params).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: set_params.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    ctrl_avail_idx += 1;
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    let prepare = [
        VIRTIO_SND_R_PCM_PREPARE.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    mem.write(ctrl_req, &prepare).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: prepare.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    ctrl_avail_idx += 1;
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    // RX request while stream is Prepared (not started yet) should return IO_ERR and write silence.
    let rx_hdr = 0x8000;
    let rx_payload = 0x8100;
    let rx_resp = 0x8200;

    let hdr = [CAPTURE_STREAM_ID.to_le_bytes(), 0u32.to_le_bytes()].concat();
    mem.write(rx_hdr, &hdr).unwrap();

    let mut rx_avail_idx = 0u16;
    mem.write(rx_payload, &[0xffu8; 8]).unwrap();
    mem.write(rx_resp, &[0xffu8; 8]).unwrap();
    submit_rx_chain(
        &mut dev,
        &mut mem,
        &caps,
        RxChainSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: rx_avail_idx,
            hdr_addr: rx_hdr,
            payload_addr: rx_payload,
            payload_len: 8,
            resp_addr: rx_resp,
        },
    );
    rx_avail_idx += 1;

    let status_bytes = mem.get_slice(rx_resp, 8).unwrap();
    assert_eq!(
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        VIRTIO_SND_S_IO_ERR
    );
    assert_eq!(
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
        0
    );

    let payload_bytes = mem.get_slice(rx_payload, 8).unwrap();
    let mut got = [0i16; 4];
    for (i, slot) in got.iter_mut().enumerate() {
        let off = i * 2;
        *slot = i16::from_le_bytes(payload_bytes[off..off + 2].try_into().unwrap());
    }
    assert_eq!(got, [0, 0, 0, 0]);

    // Start capture stream.
    let start = [
        VIRTIO_SND_R_PCM_START.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    mem.write(ctrl_req, &start).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: start.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    // RX request: capture 4 mono samples (8 bytes).
    mem.write(rx_payload, &[0xffu8; 8]).unwrap();
    mem.write(rx_resp, &[0xffu8; 8]).unwrap();
    submit_rx_chain(
        &mut dev,
        &mut mem,
        &caps,
        RxChainSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: rx_avail_idx,
            hdr_addr: rx_hdr,
            payload_addr: rx_payload,
            payload_len: 8,
            resp_addr: rx_resp,
        },
    );
    rx_avail_idx += 1;

    let status_bytes = mem.get_slice(rx_resp, 8).unwrap();
    assert_eq!(
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        VIRTIO_SND_S_OK
    );
    assert_eq!(
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
        0
    );

    let payload_bytes = mem.get_slice(rx_payload, 8).unwrap();
    for (i, slot) in got.iter_mut().enumerate() {
        let off = i * 2;
        *slot = i16::from_le_bytes(payload_bytes[off..off + 2].try_into().unwrap());
    }
    assert_eq!(got, [0, 16_384, -16_384, 32_767]);

    // Second RX request: request 4 samples again, but the capture source only has
    // one sample remaining. The rest should be silence, and telemetry should
    // count the underrun + dropped samples.
    mem.write(rx_payload, &[0xffu8; 8]).unwrap();
    mem.write(rx_resp, &[0xffu8; 8]).unwrap();
    submit_rx_chain(
        &mut dev,
        &mut mem,
        &caps,
        RxChainSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: rx_avail_idx,
            hdr_addr: rx_hdr,
            payload_addr: rx_payload,
            payload_len: 8,
            resp_addr: rx_resp,
        },
    );

    let status_bytes = mem.get_slice(rx_resp, 8).unwrap();
    assert_eq!(
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        VIRTIO_SND_S_OK
    );
    assert_eq!(
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
        0
    );

    let payload_bytes = mem.get_slice(rx_payload, 8).unwrap();
    for (i, slot) in got.iter_mut().enumerate() {
        let off = i * 2;
        *slot = i16::from_le_bytes(payload_bytes[off..off + 2].try_into().unwrap());
    }
    assert_eq!(got, [8_192, 0, 0, 0]);

    let telem = dev
        .device_mut::<VirtioSnd<aero_audio::ring::AudioRingBuffer, TestCaptureSource>>()
        .unwrap()
        .capture_telemetry();
    assert_eq!(telem.dropped_samples, 7);
    assert_eq!(telem.underrun_samples, 3);
    assert_eq!(telem.underrun_responses, 1);
}

#[test]
fn virtio_snd_rx_resamples_capture_rate_to_guest_48k() {
    // Provide fewer capture-rate samples than the guest is requesting. When the
    // host capture sample rate is lower than the guest contract (44.1kHz -> 48kHz),
    // the RX path must upsample rather than underrun.
    let capture = TestCaptureSource {
        samples: vec![1.0; 45],
        pos: 0,
        dropped: 0,
    };

    let snd = VirtioSnd::new_with_capture_and_host_sample_rate(
        aero_audio::ring::AudioRingBuffer::new_stereo(8),
        capture,
        48_000,
    );
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    dev.device_mut::<VirtioSnd<aero_audio::ring::AudioRingBuffer, TestCaptureSource>>()
        .unwrap()
        .set_capture_sample_rate_hz(44_100);
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x40000);

    // Feature negotiation: accept everything the device offers.
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

    // Configure control and RX queues.
    let ctrl_desc = 0x1000;
    let ctrl_avail = 0x2000;
    let ctrl_used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_CONTROL,
        ctrl_desc,
        ctrl_avail,
        ctrl_used,
    );

    let rx_desc = 0x4000;
    let rx_avail = 0x5000;
    let rx_used = 0x6000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_RX,
        rx_desc,
        rx_avail,
        rx_used,
    );

    // Drive the capture state machine: SET_PARAMS -> PREPARE -> START.
    let ctrl_req = 0x7000;
    let ctrl_resp = 0x7100;
    let mut ctrl_avail_idx = 0u16;

    let mut set_params = [0u8; 24];
    set_params[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    set_params[4..8].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
    set_params[8..12].copy_from_slice(&4096u32.to_le_bytes());
    set_params[12..16].copy_from_slice(&1024u32.to_le_bytes());
    set_params[16..20].copy_from_slice(&0u32.to_le_bytes());
    set_params[20] = 1;
    set_params[21] = VIRTIO_SND_PCM_FMT_S16;
    set_params[22] = VIRTIO_SND_PCM_RATE_48000;
    mem.write(ctrl_req, &set_params).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: set_params.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    ctrl_avail_idx += 1;
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    let prepare = [
        VIRTIO_SND_R_PCM_PREPARE.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    mem.write(ctrl_req, &prepare).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: prepare.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    ctrl_avail_idx += 1;
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    let start = [
        VIRTIO_SND_R_PCM_START.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    mem.write(ctrl_req, &start).unwrap();
    mem.write(ctrl_resp, &[0xffu8; 64]).unwrap();
    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_CONTROL,
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            out_addr: ctrl_req,
            out_len: start.len() as u32,
            in_addr: ctrl_resp,
            in_len: 64,
        },
    );
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    // RX request: capture 48 mono samples (96 bytes).
    let rx_hdr = 0x8000;
    let rx_payload = 0x8100;
    let rx_resp = 0x8200;

    let hdr = [CAPTURE_STREAM_ID.to_le_bytes(), 0u32.to_le_bytes()].concat();
    mem.write(rx_hdr, &hdr).unwrap();

    let rx_avail_idx = 0u16;
    mem.write(rx_payload, &[0xffu8; 96]).unwrap();
    mem.write(rx_resp, &[0xffu8; 8]).unwrap();
    submit_rx_chain(
        &mut dev,
        &mut mem,
        &caps,
        RxChainSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: rx_avail_idx,
            hdr_addr: rx_hdr,
            payload_addr: rx_payload,
            payload_len: 96,
            resp_addr: rx_resp,
        },
    );

    let status_bytes = mem.get_slice(rx_resp, 8).unwrap();
    assert_eq!(
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        VIRTIO_SND_S_OK
    );
    assert_eq!(
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
        0
    );

    let payload_bytes = mem.get_slice(rx_payload, 96).unwrap();
    let mut got = [0i16; 48];
    for (i, slot) in got.iter_mut().enumerate() {
        let off = i * 2;
        *slot = i16::from_le_bytes(payload_bytes[off..off + 2].try_into().unwrap());
    }
    assert_eq!(got, [32_767; 48]);

    let telem = dev
        .device_mut::<VirtioSnd<aero_audio::ring::AudioRingBuffer, TestCaptureSource>>()
        .unwrap()
        .capture_telemetry();
    assert_eq!(telem.dropped_samples, 0);
    assert_eq!(telem.underrun_samples, 0);
    assert_eq!(telem.underrun_responses, 0);
}
