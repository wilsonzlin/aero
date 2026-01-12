use aero_virtio::devices::snd::{
    AudioCaptureSource, VirtioSnd, CAPTURE_STREAM_ID, VIRTIO_SND_PCM_FMT_S16,
    VIRTIO_SND_PCM_RATE_48000, VIRTIO_SND_QUEUE_CONTROL, VIRTIO_SND_QUEUE_RX,
    VIRTIO_SND_R_PCM_PREPARE, VIRTIO_SND_R_PCM_SET_PARAMS, VIRTIO_SND_R_PCM_START,
    VIRTIO_SND_S_BAD_MSG, VIRTIO_SND_S_IO_ERR, VIRTIO_SND_S_OK,
};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, VirtioPciDevice, VIRTIO_PCI_CAP_COMMON_CFG, VIRTIO_PCI_CAP_DEVICE_CFG,
    VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG, VIRTIO_STATUS_ACKNOWLEDGE,
    VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Debug, Default)]
struct CaptureState {
    next_sample: i32,
    max_samples: Option<usize>,
    produced_samples: usize,
    read_calls: usize,
    dropped_samples: u64,
    dropped_calls: usize,
    underrun_samples: usize,
    underrun_calls: usize,
}

/// Deterministic capture source that produces a simple ramp.
///
/// The ramp uses integer `i16` values scaled into `[-1.0, 1.0]` via `/ 32768.0`,
/// which guarantees exact round-trips through the device's `f32 -> i16` conversion for the
/// values used in these tests.
#[derive(Clone, Debug)]
struct RampCaptureSource(Rc<RefCell<CaptureState>>);

impl RampCaptureSource {
    fn new(next_sample: i32, dropped_samples: u64, max_samples: Option<usize>) -> Self {
        Self(Rc::new(RefCell::new(CaptureState {
            next_sample,
            dropped_samples,
            max_samples,
            ..CaptureState::default()
        })))
    }

    fn state(&self) -> Rc<RefCell<CaptureState>> {
        self.0.clone()
    }
}

impl AudioCaptureSource for RampCaptureSource {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        let requested = out.len();
        let mut state = self.0.borrow_mut();
        state.read_calls += 1;

        let remaining = state
            .max_samples
            .map(|max| max.saturating_sub(state.produced_samples))
            .unwrap_or(requested);
        let take = requested.min(remaining);

        for slot in out[..take].iter_mut() {
            let sample_i16 = state.next_sample as i16;
            *slot = f32::from(sample_i16) / 32768.0;
            state.next_sample += 1;
        }

        if take < requested {
            state.underrun_calls += 1;
            state.underrun_samples += requested - take;
        }

        state.produced_samples += take;
        take
    }

    fn take_dropped_samples(&mut self) -> u64 {
        let mut state = self.0.borrow_mut();
        state.dropped_calls += 1;
        let dropped = state.dropped_samples;
        state.dropped_samples = 0;
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

fn negotiate_features(dev: &mut VirtioPciDevice, mem: &mut GuestRam, caps: &Caps) {
    // Enable PCI bus mastering (DMA). The virtio-pci transport gates all guest-memory access on
    // `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0004u16.to_le_bytes());

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

    // Initialise rings (flags/idx).
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

fn read_pcm_status(mem: &GuestRam, addr: u64) -> (u32, u32) {
    let status_bytes = mem.get_slice(addr, 8).unwrap();
    (
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
    )
}

struct CtrlSubmit<'a> {
    desc_table: u64,
    avail_addr: u64,
    avail_idx: u16,
    req_addr: u64,
    resp_addr: u64,
    req: &'a [u8],
}

struct RxSubmit<'a> {
    desc_table: u64,
    avail_addr: u64,
    avail_idx: u16,
    out_addr: u64,
    out_len: u32,
    payload_descs: &'a [(u64, u32)],
    resp_addr: u64,
}

#[allow(clippy::too_many_arguments)]
fn submit_ctrl(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    submit: CtrlSubmit<'_>,
) -> u16 {
    mem.write(submit.req_addr, submit.req).unwrap();
    mem.write(submit.resp_addr, &[0xffu8; 64]).unwrap();

    write_desc(
        mem,
        submit.desc_table,
        0,
        submit.req_addr,
        submit.req.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        mem,
        submit.desc_table,
        1,
        submit.resp_addr,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    let mut avail_idx = submit.avail_idx;
    let elem_addr = submit.avail_addr + 4 + u64::from(avail_idx) * 2;
    write_u16_le(mem, elem_addr, 0).unwrap();
    avail_idx = avail_idx.wrapping_add(1);
    write_u16_le(mem, submit.avail_addr + 2, avail_idx).unwrap();

    kick_queue(dev, mem, caps, VIRTIO_SND_QUEUE_CONTROL);
    avail_idx
}

fn drive_capture_to_prepared(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    ctrl_desc: u64,
    ctrl_avail: u64,
    ctrl_req: u64,
    ctrl_resp: u64,
) -> u16 {
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

    ctrl_avail_idx = submit_ctrl(
        dev,
        mem,
        caps,
        CtrlSubmit {
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            req_addr: ctrl_req,
            resp_addr: ctrl_resp,
            req: &set_params,
        },
    );
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    let prepare = [
        VIRTIO_SND_R_PCM_PREPARE.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    ctrl_avail_idx = submit_ctrl(
        dev,
        mem,
        caps,
        CtrlSubmit {
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            req_addr: ctrl_req,
            resp_addr: ctrl_resp,
            req: &prepare,
        },
    );
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );

    ctrl_avail_idx
}

fn drive_capture_to_running(
    dev: &mut VirtioPciDevice,
    mem: &mut GuestRam,
    caps: &Caps,
    ctrl_desc: u64,
    ctrl_avail: u64,
    ctrl_req: u64,
    ctrl_resp: u64,
) {
    let mut ctrl_avail_idx =
        drive_capture_to_prepared(dev, mem, caps, ctrl_desc, ctrl_avail, ctrl_req, ctrl_resp);

    let start = [
        VIRTIO_SND_R_PCM_START.to_le_bytes(),
        CAPTURE_STREAM_ID.to_le_bytes(),
    ]
    .concat();
    ctrl_avail_idx = submit_ctrl(
        dev,
        mem,
        caps,
        CtrlSubmit {
            desc_table: ctrl_desc,
            avail_addr: ctrl_avail,
            avail_idx: ctrl_avail_idx,
            req_addr: ctrl_req,
            resp_addr: ctrl_resp,
            req: &start,
        },
    );
    assert_eq!(
        u32::from_le_bytes(mem.get_slice(ctrl_resp, 4).unwrap().try_into().unwrap()),
        VIRTIO_SND_S_OK
    );
    let _ = ctrl_avail_idx;
}

fn submit_rx(dev: &mut VirtioPciDevice, mem: &mut GuestRam, caps: &Caps, submit: RxSubmit<'_>) {
    let resp_index = submit.payload_descs.len() as u16 + 1;

    write_desc(
        mem,
        submit.desc_table,
        0,
        submit.out_addr,
        submit.out_len,
        VIRTQ_DESC_F_NEXT,
        1,
    );

    for (i, &(addr, len)) in submit.payload_descs.iter().enumerate() {
        let idx = 1 + i as u16;
        let next = if idx == resp_index - 1 {
            resp_index
        } else {
            idx + 1
        };
        write_desc(
            mem,
            submit.desc_table,
            idx,
            addr,
            len,
            VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
            next,
        );
    }

    write_desc(
        mem,
        submit.desc_table,
        resp_index,
        submit.resp_addr,
        8,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    let elem_addr = submit.avail_addr + 4 + u64::from(submit.avail_idx) * 2;
    write_u16_le(mem, elem_addr, 0).unwrap();
    write_u16_le(mem, submit.avail_addr + 2, submit.avail_idx.wrapping_add(1)).unwrap();

    kick_queue(dev, mem, caps, VIRTIO_SND_QUEUE_RX);
}

fn assert_used_len(mem: &GuestRam, used_addr: u64, expected: u32) {
    let used_idx = u16::from_le_bytes(mem.get_slice(used_addr + 2, 2).unwrap().try_into().unwrap());
    assert_eq!(used_idx, 1);
    let elem = mem.get_slice(used_addr + 4, 8).unwrap();
    assert_eq!(u32::from_le_bytes(elem[0..4].try_into().unwrap()), 0);
    assert_eq!(u32::from_le_bytes(elem[4..8].try_into().unwrap()), expected);
}

#[test]
fn virtio_snd_rx_bad_msg_when_header_missing() {
    let capture = RampCaptureSource::new(0, 123, None);
    let capture_state = capture.state();

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
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
    // Missing reserved u32 -> header is too short.
    write_u32_le(&mut mem, hdr_addr, CAPTURE_STREAM_ID).unwrap();

    let payload0_addr = 0x8100;
    let payload1_addr = 0x8200;
    let resp_addr = 0x8300;
    mem.write(payload0_addr, &[0xaau8; 5]).unwrap();
    mem.write(payload1_addr, &[0xaau8; 7]).unwrap();
    mem.write(resp_addr, &[0xaau8; 8]).unwrap();

    submit_rx(
        &mut dev,
        &mut mem,
        &caps,
        RxSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: 0,
            out_addr: hdr_addr,
            out_len: 4,
            payload_descs: &[(payload0_addr, 5), (payload1_addr, 7)],
            resp_addr,
        },
    );

    assert_used_len(&mem, rx_used, 12 + 8);

    let (status, latency) = read_pcm_status(&mem, resp_addr);
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
    assert_eq!(latency, 0);

    assert!(mem
        .get_slice(payload0_addr, 5)
        .unwrap()
        .iter()
        .all(|&b| b == 0));
    assert!(mem
        .get_slice(payload1_addr, 7)
        .unwrap()
        .iter()
        .all(|&b| b == 0));

    let state = capture_state.borrow();
    assert_eq!(state.read_calls, 0);
    assert_eq!(state.dropped_calls, 0);
    assert_eq!(state.produced_samples, 0);
}

#[test]
fn virtio_snd_rx_bad_msg_when_header_has_extra_bytes() {
    let capture = RampCaptureSource::new(0, 0, None);
    let capture_state = capture.state();

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
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
    // Header is correct but has trailing extra bytes (must be rejected with BAD_MSG).
    let mut hdr = [0u8; 12];
    hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
    hdr[4..8].copy_from_slice(&0u32.to_le_bytes());
    hdr[8..12].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    mem.write(hdr_addr, &hdr).unwrap();

    let payload_addr = 0x8100;
    let resp_addr = 0x8200;
    mem.write(payload_addr, &[0xaau8; 4]).unwrap();
    mem.write(resp_addr, &[0xaau8; 8]).unwrap();

    submit_rx(
        &mut dev,
        &mut mem,
        &caps,
        RxSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: 0,
            out_addr: hdr_addr,
            out_len: 12,
            payload_descs: &[(payload_addr, 4)],
            resp_addr,
        },
    );

    assert_used_len(&mem, rx_used, 4 + 8);

    let (status, latency) = read_pcm_status(&mem, resp_addr);
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
    assert_eq!(latency, 0);

    assert!(mem
        .get_slice(payload_addr, 4)
        .unwrap()
        .iter()
        .all(|&b| b == 0));

    let state = capture_state.borrow();
    assert_eq!(state.read_calls, 0);
    assert_eq!(state.dropped_calls, 0);
}

#[test]
fn virtio_snd_rx_bad_msg_when_stream_id_not_capture() {
    let capture = RampCaptureSource::new(0, 0, None);
    let capture_state = capture.state();

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
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
    // Use the playback stream id in the RX queue (must be rejected).
    write_u32_le(&mut mem, hdr_addr, 0).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();

    let payload_addr = 0x8100;
    let resp_addr = 0x8200;
    mem.write(payload_addr, &[0xaau8; 4]).unwrap();
    mem.write(resp_addr, &[0xaau8; 8]).unwrap();

    submit_rx(
        &mut dev,
        &mut mem,
        &caps,
        RxSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: 0,
            out_addr: hdr_addr,
            out_len: 8,
            payload_descs: &[(payload_addr, 4)],
            resp_addr,
        },
    );

    assert_used_len(&mem, rx_used, 4 + 8);

    let (status, latency) = read_pcm_status(&mem, resp_addr);
    assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
    assert_eq!(latency, 0);

    assert!(mem
        .get_slice(payload_addr, 4)
        .unwrap()
        .iter()
        .all(|&b| b == 0));

    let state = capture_state.borrow();
    assert_eq!(state.read_calls, 0);
    assert_eq!(state.dropped_calls, 0);
}

#[test]
fn virtio_snd_rx_io_err_when_capture_stream_not_running() {
    let capture = RampCaptureSource::new(0, 99, None);
    let capture_state = capture.state();

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x40000);
    negotiate_features(&mut dev, &mut mem, &caps);

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

    let ctrl_req = 0x7000;
    let ctrl_resp = 0x7100;
    drive_capture_to_prepared(
        &mut dev, &mut mem, &caps, ctrl_desc, ctrl_avail, ctrl_req, ctrl_resp,
    );

    let hdr_addr = 0x8000;
    write_u32_le(&mut mem, hdr_addr, CAPTURE_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();

    let payload_addr = 0x8100;
    let resp_addr = 0x8200;
    mem.write(payload_addr, &[0xaau8; 8]).unwrap();
    mem.write(resp_addr, &[0xaau8; 8]).unwrap();

    submit_rx(
        &mut dev,
        &mut mem,
        &caps,
        RxSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: 0,
            out_addr: hdr_addr,
            out_len: 8,
            payload_descs: &[(payload_addr, 8)],
            resp_addr,
        },
    );

    assert_used_len(&mem, rx_used, 8 + 8);

    let (status, latency) = read_pcm_status(&mem, resp_addr);
    assert_eq!(status, VIRTIO_SND_S_IO_ERR);
    assert_eq!(latency, 0);

    assert!(mem
        .get_slice(payload_addr, 8)
        .unwrap()
        .iter()
        .all(|&b| b == 0));

    // IO_ERR must not consume capture samples or take dropped sample deltas.
    let state = capture_state.borrow();
    assert_eq!(state.read_calls, 0);
    assert_eq!(state.dropped_calls, 0);
    assert_eq!(state.dropped_samples, 99);
}

#[test]
fn virtio_snd_rx_ok_writes_full_payload_and_pcm_status_in_last_descriptor() {
    let capture = RampCaptureSource::new(0, 7, None);
    let capture_state = capture.state();

    let snd =
        VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), capture);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));
    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x40000);
    negotiate_features(&mut dev, &mut mem, &caps);

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

    drive_capture_to_running(
        &mut dev, &mut mem, &caps, ctrl_desc, ctrl_avail, 0x7000, 0x7100,
    );

    let hdr_addr = 0x8000;
    write_u32_le(&mut mem, hdr_addr, CAPTURE_STREAM_ID).unwrap();
    write_u32_le(&mut mem, hdr_addr + 4, 0).unwrap();

    // Two payload descriptors whose combined length is even (12 bytes -> 6 samples) but the first
    // desc ends mid-sample (5 bytes) to validate cross-descriptor PCM packing.
    let payload0_addr = 0x8100;
    let payload1_addr = 0x8200;
    let resp_addr = 0x8300;
    mem.write(payload0_addr, &[0xaau8; 5]).unwrap();
    mem.write(payload1_addr, &[0xaau8; 7]).unwrap();
    mem.write(resp_addr, &[0xaau8; 8]).unwrap();

    submit_rx(
        &mut dev,
        &mut mem,
        &caps,
        RxSubmit {
            desc_table: rx_desc,
            avail_addr: rx_avail,
            avail_idx: 0,
            out_addr: hdr_addr,
            out_len: 8,
            payload_descs: &[(payload0_addr, 5), (payload1_addr, 7)],
            resp_addr,
        },
    );

    assert_used_len(&mem, rx_used, 12 + 8);

    let (status, latency) = read_pcm_status(&mem, resp_addr);
    assert_eq!(status, VIRTIO_SND_S_OK);
    assert_eq!(latency, 0);

    let expected: [u8; 12] = [
        0x00, 0x00, // 0
        0x01, 0x00, // 1
        0x02, 0x00, // 2
        0x03, 0x00, // 3
        0x04, 0x00, // 4
        0x05, 0x00, // 5
    ];
    assert_eq!(mem.get_slice(payload0_addr, 5).unwrap(), &expected[..5]);
    assert_eq!(mem.get_slice(payload1_addr, 7).unwrap(), &expected[5..]);

    let state = capture_state.borrow();
    assert_eq!(state.read_calls, 1);
    assert_eq!(state.produced_samples, 6);
    assert_eq!(state.dropped_calls, 1);
    assert_eq!(state.dropped_samples, 0);

    let telem = dev
        .device_mut::<VirtioSnd<aero_audio::ring::AudioRingBuffer, RampCaptureSource>>()
        .unwrap()
        .capture_telemetry();
    assert_eq!(telem.dropped_samples, 7);
    assert_eq!(telem.underrun_samples, 0);
    assert_eq!(telem.underrun_responses, 0);
}
