use aero_audio::sink::AudioSink;
use aero_io_snapshot::io::state::IoSnapshot;
use aero_virtio::devices::snd::{
    VirtioSnd, VIRTIO_SND_PCM_FMT_S16, VIRTIO_SND_PCM_RATE_48000, VIRTIO_SND_QUEUE_CONTROL,
    VIRTIO_SND_QUEUE_EVENT, VIRTIO_SND_QUEUE_RX, VIRTIO_SND_QUEUE_TX, VIRTIO_SND_R_PCM_PREPARE,
    VIRTIO_SND_R_PCM_SET_PARAMS, VIRTIO_SND_R_PCM_START, VIRTIO_SND_S_OK,
};
use aero_virtio::memory::{write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam};
use aero_virtio::pci::{
    InterruptLog, InterruptSink, VirtioPciDevice, PCI_VENDOR_ID_VIRTIO,
    VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1, VIRTIO_PCI_CAP_COMMON_CFG,
    VIRTIO_PCI_CAP_DEVICE_CFG, VIRTIO_PCI_CAP_ISR_CFG, VIRTIO_PCI_CAP_NOTIFY_CFG,
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_platform::interrupts::msi::MsiMessage;
use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};

use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone)]
struct CaptureSink(Rc<RefCell<Vec<f32>>>);

impl AudioSink for CaptureSink {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        self.0.borrow_mut().extend_from_slice(samples);
    }
}

#[derive(Default)]
struct LegacyIrqState {
    raised: u32,
    lowered: u32,
    asserted: bool,
}

#[derive(Clone)]
struct SharedLegacyIrq {
    state: Rc<RefCell<LegacyIrqState>>,
}

impl SharedLegacyIrq {
    fn new() -> (Self, Rc<RefCell<LegacyIrqState>>) {
        let state = Rc::new(RefCell::new(LegacyIrqState::default()));
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl InterruptSink for SharedLegacyIrq {
    fn raise_legacy_irq(&mut self) {
        let mut state = self.state.borrow_mut();
        state.raised = state.raised.saturating_add(1);
        state.asserted = true;
    }

    fn lower_legacy_irq(&mut self) {
        let mut state = self.state.borrow_mut();
        state.lowered = state.lowered.saturating_add(1);
        state.asserted = false;
    }

    fn signal_msix(&mut self, _message: MsiMessage) {}
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

#[test]
fn virtio_snd_pci_contract_v1_features_and_queue_sizes() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x4000);

    bar_write_u32(&mut dev, &mut mem, caps.common, 0);
    let f0 = bar_read_u32(&mut dev, caps.common + 0x04);
    bar_write_u32(&mut dev, &mut mem, caps.common, 1);
    let f1 = bar_read_u32(&mut dev, caps.common + 0x04);

    let features = u64::from(f0) | (u64::from(f1) << 32);
    assert_eq!(features, VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC);

    bar_write_u16(
        &mut dev,
        &mut mem,
        caps.common + 0x16,
        VIRTIO_SND_QUEUE_CONTROL,
    );
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), 64);

    bar_write_u16(
        &mut dev,
        &mut mem,
        caps.common + 0x16,
        VIRTIO_SND_QUEUE_EVENT,
    );
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), 64);

    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, VIRTIO_SND_QUEUE_TX);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), 256);

    bar_write_u16(&mut dev, &mut mem, caps.common + 0x16, VIRTIO_SND_QUEUE_RX);
    assert_eq!(bar_read_u16(&mut dev, caps.common + 0x18), 64);
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

#[test]
fn virtio_snd_eventq_buffers_are_not_completed_without_events() {
    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let (irq, irq_state) = SharedLegacyIrq::new();
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(irq));

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);

    let mut mem = GuestRam::new(0x20000);

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

    // Configure event queue 1.
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_EVENT,
        desc,
        avail,
        used,
    );

    // Post one writable buffer.
    let buf = 0x4000;
    mem.write(buf, &[0u8; 8]).unwrap();
    write_desc(&mut mem, desc, 0, buf, 8, VIRTQ_DESC_F_WRITE, 0);
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();

    // Kicking the event queue should not complete the buffer (no events are defined in contract v1).
    dev.bar0_write(
        caps.notify + u64::from(VIRTIO_SND_QUEUE_EVENT) * u64::from(caps.notify_mult),
        &VIRTIO_SND_QUEUE_EVENT.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);
    assert_eq!(
        u16::from_le_bytes(mem.get_slice(used + 2, 2).unwrap().try_into().unwrap()),
        0
    );
    assert_eq!(irq_state.borrow().raised, 0);

    // Polling should remain silent and should not generate used entries.
    dev.poll(&mut mem);
    assert_eq!(
        u16::from_le_bytes(mem.get_slice(used + 2, 2).unwrap().try_into().unwrap()),
        0
    );
    assert_eq!(irq_state.borrow().raised, 0);

    let mut isr = [0u8; 1];
    dev.bar0_read(caps.isr, &mut isr);
    assert_eq!(isr[0], 0);
}

#[test]
fn virtio_snd_snapshot_restore_can_rewind_eventq_progress_to_recover_buffers() {
    // The virtio-snd event queue can pop guest-provided buffers without completing them (no used
    // entries) because Aero's contract currently defines no events. The device model caches those
    // descriptor chains internally. Since snapshot state does not serialize the cached chains, a
    // restore must rewind `next_avail` to `next_used` so the transport can re-pop them.

    let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());
    let caps = parse_caps(&mut dev);

    // Keep PCI memory decoding enabled while enabling bus mastering.
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let mut mem = GuestRam::new(0x20000);

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

    // Configure event queue 1.
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_EVENT,
        desc,
        avail,
        used,
    );

    // Post one writable buffer and have the device pop it (but not complete it).
    let buf = 0x4000;
    mem.write(buf, &[0u8; 8]).unwrap();
    write_desc(&mut mem, desc, 0, buf, 8, VIRTQ_DESC_F_WRITE, 0);
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();

    dev.bar0_write(
        caps.notify + u64::from(VIRTIO_SND_QUEUE_EVENT) * u64::from(caps.notify_mult),
        &VIRTIO_SND_QUEUE_EVENT.to_le_bytes(),
    );
    dev.process_notified_queues(&mut mem);
    assert_eq!(
        dev.debug_queue_progress(VIRTIO_SND_QUEUE_EVENT),
        Some((1, 0, false))
    );

    let snap = dev.save_state();
    let mem_snap = mem.clone();

    // Restore into a fresh device instance (same guest memory image).
    let snd2 = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
    let mut restored = VirtioPciDevice::new(Box::new(snd2), Box::new(InterruptLog::default()));
    restored.load_state(&snap).unwrap();
    let mut mem2 = mem_snap.clone();

    // Without rewinding, the transport believes it has already consumed the avail entry.
    assert_eq!(
        restored.debug_queue_progress(VIRTIO_SND_QUEUE_EVENT),
        Some((1, 0, false))
    );
    restored.process_notified_queues(&mut mem2);
    assert_eq!(
        restored.debug_queue_progress(VIRTIO_SND_QUEUE_EVENT),
        Some((1, 0, false))
    );

    // Rewind `next_avail` to `next_used` so the device can re-pop the guest buffer.
    restored.rewind_queue_next_avail_to_next_used(VIRTIO_SND_QUEUE_EVENT);
    assert_eq!(
        restored.debug_queue_progress(VIRTIO_SND_QUEUE_EVENT),
        Some((0, 0, false))
    );
    restored.process_notified_queues(&mut mem2);
    assert_eq!(
        restored.debug_queue_progress(VIRTIO_SND_QUEUE_EVENT),
        Some((1, 0, false))
    );
}

#[test]
fn virtio_snd_tx_pushes_samples_to_backend() {
    let samples = Rc::new(RefCell::new(Vec::<f32>::new()));
    let output = CaptureSink(samples.clone());
    let snd = VirtioSnd::new(output);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));

    // Basic PCI identification.
    let mut id = [0u8; 4];
    dev.config_read(0, &mut id);
    let vendor = u16::from_le_bytes(id[0..2].try_into().unwrap());
    assert_eq!(vendor, PCI_VENDOR_ID_VIRTIO);

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
    assert_ne!(caps.notify, 0);
    assert_ne!(caps.isr, 0);
    assert_ne!(caps.device, 0);
    assert_ne!(caps.notify_mult, 0);

    let mut mem = GuestRam::new(0x20000);

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

    // Configure control and TX queues.
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

    let tx_desc = 0x4000;
    let tx_avail = 0x5000;
    let tx_used = 0x6000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_TX,
        tx_desc,
        tx_avail,
        tx_used,
    );

    // Drive the minimal control state machine: SET_PARAMS -> PREPARE -> START.
    let ctrl_req = 0x7000;
    let ctrl_resp = 0x7100;
    let mut ctrl_avail_idx = 0u16;

    let mut set_params = [0u8; 24];
    set_params[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    set_params[4..8].copy_from_slice(&0u32.to_le_bytes()); // stream_id
    set_params[8..12].copy_from_slice(&4096u32.to_le_bytes()); // buffer_bytes
    set_params[12..16].copy_from_slice(&1024u32.to_le_bytes()); // period_bytes
    set_params[16..20].copy_from_slice(&0u32.to_le_bytes()); // features
    set_params[20] = 2; // channels
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

    let prepare = [VIRTIO_SND_R_PCM_PREPARE.to_le_bytes(), 0u32.to_le_bytes()].concat();
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

    let start = [VIRTIO_SND_R_PCM_START.to_le_bytes(), 0u32.to_le_bytes()].concat();
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

    // PCM payload: 2 frames of 16-bit stereo, preceded by the virtio-snd TX header.
    let tx_payload = 0x8000;
    let tx_status = 0x9000;
    let pcm: [i16; 4] = [0, 16_384, -16_384, 0];
    let mut tx_bytes = Vec::new();
    tx_bytes.extend_from_slice(&0u32.to_le_bytes()); // stream_id
    tx_bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved
    for v in pcm {
        tx_bytes.extend_from_slice(&v.to_le_bytes());
    }
    mem.write(tx_payload, &tx_bytes).unwrap();
    mem.write(tx_status, &[0xffu8; 8]).unwrap();

    submit_chain(
        &mut dev,
        &mut mem,
        &caps,
        ChainSubmit {
            queue_index: VIRTIO_SND_QUEUE_TX,
            desc_table: tx_desc,
            avail_addr: tx_avail,
            avail_idx: 0,
            out_addr: tx_payload,
            out_len: tx_bytes.len() as u32,
            in_addr: tx_status,
            in_len: 8,
        },
    );

    let status_bytes = mem.get_slice(tx_status, 8).unwrap();
    assert_eq!(
        u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
        VIRTIO_SND_S_OK
    );
    assert_eq!(
        u32::from_le_bytes(status_bytes[4..8].try_into().unwrap()),
        0
    );

    let got = samples.borrow().clone();
    assert_eq!(got.len(), 4);
    let expect = [0.0f32, 16_384.0f32 / 32_768.0, -16_384.0f32 / 32_768.0, 0.0];
    for (g, e) in got.iter().zip(expect.iter()) {
        assert!((g - e).abs() < 1e-6, "got {g} expected {e}");
    }
}

#[test]
fn virtio_snd_tx_resamples_to_host_rate_and_is_stateful() {
    // Simulate a host AudioContext running at 44.1kHz. TX PCM is always 48kHz, so the
    // device must resample at the host boundary to avoid pitch shift.
    let host_rate_hz = 44_100u32;

    let samples = Rc::new(RefCell::new(Vec::<f32>::new()));
    let output = CaptureSink(samples.clone());
    let snd = VirtioSnd::new_with_host_sample_rate(output, host_rate_hz);
    let mut dev = VirtioPciDevice::new(Box::new(snd), Box::new(InterruptLog::default()));

    // Enable PCI memory decoding (BAR0 MMIO) + bus mastering (DMA). The virtio-pci transport gates
    // all guest-memory access on `PCI COMMAND.BME` (bit 2).
    dev.config_write(0x04, &0x0006u16.to_le_bytes());

    let caps = parse_caps(&mut dev);
    let mut mem = GuestRam::new(0x40000);

    // Feature negotiation: accept everything the device offers.
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

    // Configure control and TX queues.
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

    let tx_desc = 0x4000;
    let tx_avail = 0x5000;
    let tx_used = 0x6000;
    configure_queue(
        &mut dev,
        &mut mem,
        &caps,
        VIRTIO_SND_QUEUE_TX,
        tx_desc,
        tx_avail,
        tx_used,
    );

    // Drive the minimal control state machine: SET_PARAMS -> PREPARE -> START.
    let ctrl_req = 0x7000;
    let ctrl_resp = 0x7100;
    let mut ctrl_avail_idx = 0u16;

    let mut set_params = [0u8; 24];
    set_params[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    set_params[4..8].copy_from_slice(&0u32.to_le_bytes()); // stream_id
    set_params[8..12].copy_from_slice(&4096u32.to_le_bytes()); // buffer_bytes
    set_params[12..16].copy_from_slice(&1024u32.to_le_bytes()); // period_bytes
    set_params[16..20].copy_from_slice(&0u32.to_le_bytes()); // features
    set_params[20] = 2; // channels
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

    let prepare = [VIRTIO_SND_R_PCM_PREPARE.to_le_bytes(), 0u32.to_le_bytes()].concat();
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

    let start = [VIRTIO_SND_R_PCM_START.to_le_bytes(), 0u32.to_le_bytes()].concat();
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

    // Submit two TX chunks, each 5ms at 48kHz.
    //
    // This is a useful statefulness check: 5ms at 44.1kHz is 220.5 frames. If the resampler is
    // reset per-chunk, we'd likely get 220+220=440 frames. A stateful resampler should output
    // 441 frames total over the full 10ms interval.
    let tx_payload = 0x8000;
    let tx_status = 0x9000;
    let src_frames_per_chunk = 240usize;
    let total_src_frames = src_frames_per_chunk * 2;

    for (tx_avail_idx, chunk) in (0..2usize).enumerate() {
        let base_frame = chunk * src_frames_per_chunk;
        let tx_avail_idx = u16::try_from(tx_avail_idx).unwrap();

        let mut tx_bytes = Vec::new();
        tx_bytes.extend_from_slice(&0u32.to_le_bytes()); // stream_id
        tx_bytes.extend_from_slice(&0u32.to_le_bytes()); // reserved

        for i in 0..src_frames_per_chunk {
            let sample = ((base_frame + i) as i16).saturating_mul(64);
            tx_bytes.extend_from_slice(&sample.to_le_bytes()); // L
            tx_bytes.extend_from_slice(&sample.to_le_bytes()); // R
        }

        mem.write(tx_payload, &tx_bytes).unwrap();
        mem.write(tx_status, &[0xffu8; 8]).unwrap();

        submit_chain(
            &mut dev,
            &mut mem,
            &caps,
            ChainSubmit {
                queue_index: VIRTIO_SND_QUEUE_TX,
                desc_table: tx_desc,
                avail_addr: tx_avail,
                avail_idx: tx_avail_idx,
                out_addr: tx_payload,
                out_len: tx_bytes.len() as u32,
                in_addr: tx_status,
                in_len: 8,
            },
        );

        let status_bytes = mem.get_slice(tx_status, 8).unwrap();
        assert_eq!(
            u32::from_le_bytes(status_bytes[0..4].try_into().unwrap()),
            VIRTIO_SND_S_OK
        );
    }

    let got = samples.borrow().clone();
    assert_eq!(got.len() % 2, 0);
    let got_frames = got.len() / 2;
    let expected_frames = (total_src_frames * host_rate_hz as usize) / 48_000;
    assert_eq!(expected_frames, 441);
    assert_eq!(got_frames, expected_frames);
    assert!(got.iter().any(|s| s.abs() > 0.001));
}
