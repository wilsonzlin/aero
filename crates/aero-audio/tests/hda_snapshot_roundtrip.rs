#![cfg(feature = "io-snapshot")]

use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};
use aero_io_snapshot::io::audio::state::AudioWorkletRingState;

const REG_GCTL: u64 = 0x08;
const REG_WAKEEN: u64 = 0x0c;
const REG_STATESTS: u64 = 0x0e;
const REG_INTCTL: u64 = 0x20;
const REG_INTSTS: u64 = 0x24;
const REG_CORBLBASE: u64 = 0x40;
const REG_CORBUBASE: u64 = 0x44;
const REG_CORBWP: u64 = 0x48;
const REG_CORBRP: u64 = 0x4a;
const REG_CORBCTL: u64 = 0x4c;
const REG_CORBSTS: u64 = 0x4d;
const REG_CORBSIZE: u64 = 0x4e;
const REG_RIRBLBASE: u64 = 0x50;
const REG_RIRBUBASE: u64 = 0x54;
const REG_RIRBWP: u64 = 0x58;
const REG_RINTCNT: u64 = 0x5a;
const REG_RIRBCTL: u64 = 0x5c;
const REG_RIRBSTS: u64 = 0x5d;
const REG_RIRBSIZE: u64 = 0x5e;
const REG_DPLBASE: u64 = 0x70;
const REG_DPUBASE: u64 = 0x74;

const REG_SD0CTL: u64 = 0x80;
const REG_SD0LPIB: u64 = 0x84;
const REG_SD0CBL: u64 = 0x88;
const REG_SD0LVI: u64 = 0x8c;
const REG_SD0FIFOW: u64 = 0x8e;
const REG_SD0FIFOS: u64 = 0x90;
const REG_SD0FMT: u64 = 0x92;
const REG_SD0BDPL: u64 = 0x98;
const REG_SD0BDPU: u64 = 0x9c;

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | payload8 as u32
}

fn verb_4(group: u16, payload16: u16) -> u32 {
    let verb_id = (group << 8) | (payload16 >> 8);
    ((verb_id as u32) << 8) | (payload16 as u8 as u32)
}

#[test]
fn hda_snapshot_restore_preserves_guest_visible_state_and_dma_progress() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x40_000);

    // Bring controller out of reset.
    hda.mmio_write(REG_GCTL, 4, 0x1);

    // Program a few global regs and CORB/RIRB regs (even though we won't execute verbs via CORB).
    hda.mmio_write(REG_WAKEEN, 2, 0x00aa);
    hda.mmio_write(REG_INTCTL, 4, (1u64 << 31) | 1u64); // GIE + stream0 enable
    hda.mmio_write(REG_DPUBASE, 4, 0);
    hda.mmio_write(REG_DPLBASE, 4, 0x1800); // disabled posbuf (bit0=0)

    hda.mmio_write(REG_CORBLBASE, 4, 0x2000);
    hda.mmio_write(REG_CORBUBASE, 4, 0);
    hda.mmio_write(REG_CORBRP, 2, 0x00ff);
    hda.mmio_write(REG_CORBWP, 2, 0x0003);
    // Keep CORB/RIRB DMA engines stopped; we only care about restoring the guest-visible
    // register state, not replaying queued commands during this unit test.
    hda.mmio_write(REG_CORBCTL, 1, 0x0);
    hda.mmio_write(REG_CORBSTS, 1, 0x1); // RW1C clears nothing (already 0), but exercises restore.
    hda.mmio_write(REG_CORBSIZE, 1, 0x2); // 256 entries

    hda.mmio_write(REG_RIRBLBASE, 4, 0x3000);
    hda.mmio_write(REG_RIRBUBASE, 4, 0);
    hda.mmio_write(REG_RIRBWP, 2, 0x00ff);
    hda.mmio_write(REG_RINTCNT, 2, 1);
    hda.mmio_write(REG_RIRBCTL, 1, 0x0);
    hda.mmio_write(REG_RIRBSTS, 1, 0x1);
    hda.mmio_write(REG_RIRBSIZE, 1, 0x2);

    // Configure codec state that Windows cares about (stream id + format + amp).
    // Set converter stream/channel to stream 1, channel 0.
    hda.codec_mut().execute_verb(2, verb_12(0x706, 0x10));
    // Set converter format to 44.1kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 14) | (1 << 4) | 0x1;
    hda.codec_mut().execute_verb(2, verb_4(0x2, fmt_raw));
    // Mute right channel, set gain to 0x22 on both.
    let set_amp_both = (1 << 7) | 0x22;
    hda.codec_mut().execute_verb(2, verb_4(0x3, set_amp_both));
    let set_amp_right = (1 << 12) | (1 << 7) | 0x11;
    hda.codec_mut().execute_verb(2, verb_4(0x3, set_amp_right));
    // Pin widget control.
    hda.codec_mut().execute_verb(3, verb_12(0x707, 0x40));
    // Pin power state (NID 3).
    hda.codec_mut().execute_verb(3, verb_12(0x705, 0x02));
    // AFG power state.
    hda.codec_mut().execute_verb(1, verb_12(0x705, 0x00));

    // Two-entry BDL:
    // - entry 0: small, IOC=1 (to raise an interrupt)
    // - entry 1: larger, IOC=0 (to keep bdl_offset non-zero)
    let bdl_base = 0x1000u64;
    let buf0 = 0x2000u64;
    let buf1 = 0x3000u64;

    mem.write_u64(bdl_base, buf0);
    mem.write_u32(bdl_base + 8, 256);
    mem.write_u32(bdl_base + 12, 1);
    mem.write_u64(bdl_base + 16, buf1);
    mem.write_u32(bdl_base + 24, 4096);
    mem.write_u32(bdl_base + 28, 0);

    // Fill PCM buffers with a simple pattern.
    for i in 0..256u32 {
        mem.write_u8(buf0 + i as u64, (i & 0xff) as u8);
    }
    for i in 0..4096u32 {
        mem.write_u8(buf1 + i as u64, (i & 0xff) as u8);
    }

    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = 4096;
        sd.lvi = 1;
        sd.fifow = 0x1234;
        sd.fifos = 0x40;
        sd.fmt = fmt_raw;
        // SRST | RUN | IOCE | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
    }

    // Process some host time to advance DMA and trigger IOC on entry 0.
    let frames_0 = 240usize;
    hda.process(&mut mem, frames_0);

    // Capture guest-visible state at snapshot time.
    let regs = [
        (REG_GCTL, 4),
        (REG_WAKEEN, 2),
        (REG_STATESTS, 2),
        (REG_INTCTL, 4),
        (REG_INTSTS, 4),
        (REG_DPLBASE, 4),
        (REG_DPUBASE, 4),
        (REG_CORBLBASE, 4),
        (REG_CORBUBASE, 4),
        (REG_CORBWP, 2),
        (REG_CORBRP, 2),
        (REG_CORBCTL, 1),
        (REG_CORBSTS, 1),
        (REG_CORBSIZE, 1),
        (REG_RIRBLBASE, 4),
        (REG_RIRBUBASE, 4),
        (REG_RIRBWP, 2),
        (REG_RINTCNT, 2),
        (REG_RIRBCTL, 1),
        (REG_RIRBSTS, 1),
        (REG_RIRBSIZE, 1),
        (REG_SD0CTL, 4),
        (REG_SD0LPIB, 4),
        (REG_SD0CBL, 4),
        (REG_SD0LVI, 2),
        (REG_SD0FIFOW, 2),
        (REG_SD0FIFOS, 2),
        (REG_SD0FMT, 2),
        (REG_SD0BDPL, 4),
        (REG_SD0BDPU, 4),
    ];
    let mut reg_vals = Vec::with_capacity(regs.len());
    for (off, size) in regs {
        reg_vals.push((off, size, hda.mmio_read(off, size)));
    }

    let codec_stream_ch_snapshot = hda.codec_mut().execute_verb(2, verb_12(0xF06, 0));
    let codec_fmt_snapshot = hda.codec_mut().execute_verb(2, verb_12(0xA00, 0));
    let amp_left_snapshot = hda.codec_mut().execute_verb(2, verb_4(0xB, 1 << 13));
    let amp_right_snapshot = hda.codec_mut().execute_verb(2, verb_4(0xB, 1 << 12));
    let pin_ctl_snapshot = hda.codec_mut().execute_verb(3, verb_12(0xF07, 0));
    let pin_power_snapshot = hda.codec_mut().execute_verb(3, verb_12(0xF05, 0));
    let afg_power_snapshot = hda.codec_mut().execute_verb(1, verb_12(0xF05, 0));

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 123,
        read_pos: 100,
    };
    let snap = hda.snapshot_state(worklet_ring.clone());
    assert_eq!(snap.worklet_ring, worklet_ring);

    // Baseline: clone the controller at snapshot time and continue running it.
    let mut expected = hda.clone();
    let frames_1 = 240usize;
    expected.process(&mut mem, frames_1);
    let expected_lpib = expected.mmio_read(REG_SD0LPIB, 4) as u32;

    // Restore into a fresh controller and continue from the snapshot.
    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    for (off, size, val) in reg_vals {
        assert_eq!(
            restored.mmio_read(off, size),
            val,
            "mmio {off:#x} size {size} mismatch"
        );
    }

    assert_eq!(
        restored.codec_mut().execute_verb(2, verb_12(0xF06, 0)),
        codec_stream_ch_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(2, verb_12(0xA00, 0)),
        codec_fmt_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(2, verb_4(0xB, 1 << 13)),
        amp_left_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(2, verb_4(0xB, 1 << 12)),
        amp_right_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(3, verb_12(0xF07, 0)),
        pin_ctl_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(3, verb_12(0xF05, 0)),
        pin_power_snapshot
    );
    assert_eq!(
        restored.codec_mut().execute_verb(1, verb_12(0xF05, 0)),
        afg_power_snapshot
    );

    restored.process(&mut mem, frames_1);
    assert_eq!(restored.mmio_read(REG_SD0LPIB, 4) as u32, expected_lpib);
    assert_eq!(restored.audio_out.available_frames(), frames_1);
}

#[test]
fn hda_capture_snapshot_restore_preserves_lpib_and_frame_accum() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x40_000);

    hda.mmio_write(REG_GCTL, 4, 0x1);

    // Configure codec ADC (NID 4) to use stream 2, channel 0.
    hda.codec_mut().execute_verb(4, verb_12(0x706, 0x20));

    // 44.1kHz, 16-bit, mono.
    let fmt_raw: u16 = (1 << 14) | (1 << 4);
    hda.codec_mut().execute_verb(4, verb_4(0x2, fmt_raw));

    // Give mic pin (NID 5) a non-default control value so we can verify codec capture state restores.
    hda.codec_mut().execute_verb(5, verb_12(0x701, 1));
    hda.codec_mut().execute_verb(5, verb_12(0x707, 0x55));
    hda.codec_mut().execute_verb(5, verb_12(0x705, 0x03));

    // Two-entry BDL so we exercise both bdl_offset and bdl_index restoration.
    let bdl_base = 0x1000u64;
    let buf0 = 0x2000u64;
    let buf1 = 0x3000u64;

    mem.write_u64(bdl_base, buf0);
    mem.write_u32(bdl_base + 8, 512);
    mem.write_u32(bdl_base + 12, 0);

    mem.write_u64(bdl_base + 16, buf1);
    mem.write_u32(bdl_base + 24, 512);
    mem.write_u32(bdl_base + 28, 0);

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = 4096;
        sd.lvi = 1;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    // Use a non-integer ratio step to ensure the capture-frame accumulator is non-zero at snapshot time.
    // With output_rate=48k and capture_rate=44.1k, output_frames=240 => 220 frames and a remainder.
    let output_frames = 240usize;

    let mut capture = VecDequeCaptureSource::new();
    let samples: Vec<f32> = (0..2000).map(|i| (i as f32 / 2000.0) * 2.0 - 1.0).collect();
    capture.push_samples(&samples);

    hda.process_with_capture(&mut mem, output_frames, &mut capture);

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let snap = hda.snapshot_state(worklet_ring);

    // Clone state at snapshot time so baseline and restored runs see identical guest RAM + capture source state.
    let mut mem_expected = mem.clone();
    let mut mem_restored = mem.clone();
    let capture_expected = capture.clone();
    let capture_restored = capture.clone();

    let mut expected = hda.clone();
    let mut expected_capture = capture_expected;
    expected.process_with_capture(&mut mem_expected, output_frames, &mut expected_capture);
    let expected_lpib = expected.stream_mut(1).lpib;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);
    let mut restored_capture = capture_restored;
    restored.process_with_capture(&mut mem_restored, output_frames, &mut restored_capture);

    // If capture-frame accumulator isn't restored, this will typically be off by one frame (2 bytes).
    assert_eq!(restored.stream_mut(1).lpib, expected_lpib);
    assert_eq!(expected_lpib, (220 + 221) as u32 * 2);

    // Verify codec capture state round-tripped via verbs.
    assert_eq!(
        restored.codec_mut().execute_verb(4, verb_12(0xF06, 0)),
        0x20
    );
    assert_eq!(
        restored.codec_mut().execute_verb(4, verb_12(0xA00, 0)),
        fmt_raw as u32
    );
    assert_eq!(restored.codec_mut().execute_verb(5, verb_12(0xF01, 0)), 1);
    assert_eq!(
        restored.codec_mut().execute_verb(5, verb_12(0xF07, 0)),
        0x55
    );
    assert_eq!(
        restored.codec_mut().execute_verb(5, verb_12(0xF05, 0)),
        0x03
    );
}

#[test]
fn hda_snapshot_restore_clamps_corb_pointers_to_selected_ring_size() {
    let hda = HdaController::new();
    let mut mem = GuestMemory::new(0x10_000);

    // Place a simple CORB verb at entry 1. After clamping CORBWP=3 -> 1 and CORBRP=0, the
    // controller should process exactly this entry (and must not spin forever).
    let corb_base = 0x1000u64;
    let rirb_base = 0x2000u64;
    let cmd = verb_12(0xF00, 0x00);
    mem.write_u32(corb_base + 4, cmd);

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let mut state = hda.snapshot_state(worklet_ring);

    // Force the CORB size selector to 2 entries, then set an out-of-range CORBWP.
    state.gctl = 0x1; // out of reset
    state.corblbase = corb_base as u32;
    state.rirblbase = rirb_base as u32;
    state.corbctl = 0x2; // RUN
    state.rirbctl = 0x2; // RUN
    state.corbsize &= !0x3; // 2 entries
    state.corbwp = 3; // out of range for 2-entry ring
    state.corbrp = 0;

    let mut restored = HdaController::new();
    restored.restore_state(&state);

    // Pointers must be clamped immediately on restore.
    assert_eq!(restored.mmio_read(REG_CORBWP, 2), 1);
    assert_eq!(restored.mmio_read(REG_CORBRP, 2), 0);

    restored.process(&mut mem, 0);

    // After processing, CORBRP should catch up to CORBWP.
    assert_eq!(
        restored.mmio_read(REG_CORBRP, 2),
        restored.mmio_read(REG_CORBWP, 2)
    );

    // Sanity check that a response was emitted into guest memory (entry index isn't important).
    let any_resp = mem.read_u32(rirb_base + 8);
    assert_ne!(any_resp, 0);
}

#[test]
fn hda_snapshot_restore_restores_output_rate_hz_for_resampler_determinism() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x40_000);

    // Use a non-default host sample rate. Safari/iOS commonly runs AudioContext at 44.1kHz, so
    // snapshot/restore must keep this rate stable to preserve guest-visible DMA progress.
    hda.set_output_rate_hz(44_100);

    hda.mmio_write(REG_GCTL, 4, 0x1);

    // Configure codec output converter to listen on stream 1, channel 0.
    hda.codec_mut().execute_verb(2, verb_12(0x706, 0x10));

    // Guest stream format: 48kHz, 16-bit, stereo. This forces the device to resample 48k -> 44.1k.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    hda.codec_mut().execute_verb(2, verb_4(0x2, fmt_raw));

    let bdl_base = 0x1000u64;
    let buf = 0x2000u64;
    let buf_len = 4096u32;

    mem.write_u64(bdl_base, buf);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    for i in 0..buf_len {
        mem.write_u8(buf + i as u64, (i & 0xff) as u8);
    }

    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = buf_len;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 20);
    }

    // Advance DMA a bit so the resampler has non-trivial state at snapshot time.
    let frames_0 = 256usize;
    hda.process(&mut mem, frames_0);

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let snap = hda.snapshot_state(worklet_ring);
    assert_eq!(snap.output_rate_hz, 44_100);

    // Baseline: continue running from the live controller at snapshot time.
    let mut expected = hda.clone();
    let frames_1 = 256usize;
    expected.process(&mut mem, frames_1);
    let expected_lpib = expected.mmio_read(REG_SD0LPIB, 4);

    // Restore into a fresh controller. We intentionally do not call `set_output_rate_hz` here;
    // the snapshot must carry the output rate so restore is deterministic.
    let mut restored = HdaController::new();
    restored.restore_state(&snap);
    assert_eq!(restored.output_rate_hz(), 44_100);

    restored.process(&mut mem, frames_1);
    assert_eq!(restored.mmio_read(REG_SD0LPIB, 4), expected_lpib);
}

#[test]
fn hda_snapshot_restore_restores_capture_sample_rate_hz_for_capture_resampler_determinism() {
    let mut hda = HdaController::new();
    // Simulate a microphone capture graph running at 44.1kHz while the output time base is 48kHz.
    hda.set_capture_sample_rate_hz(44_100);
    let mut mem = GuestMemory::new(0x40_000);

    hda.mmio_write(REG_GCTL, 4, 0x1);

    // Configure codec ADC (NID 4) to use stream 2, channel 0.
    hda.codec_mut().execute_verb(4, verb_12(0x706, 0x20));

    // Guest capture format: 48kHz, 16-bit, mono. This forces resampling 44.1k -> 48k.
    let fmt_raw: u16 = 1 << 4;
    hda.codec_mut().execute_verb(4, verb_4(0x2, fmt_raw));

    let bdl_base = 0x1000u64;
    let buf = 0x2000u64;
    let buf_len = 4096u32;

    mem.write_u64(bdl_base, buf);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = buf_len;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    let output_frames = 256usize;
    let mut capture = VecDequeCaptureSource::new();
    let samples: Vec<f32> = (0..5000).map(|i| (i as f32 / 5000.0) * 2.0 - 1.0).collect();
    capture.push_samples(&samples);

    // Advance capture so the resampler has non-trivial queued/pos state at snapshot time.
    hda.process_with_capture(&mut mem, output_frames, &mut capture);

    let snap = hda.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });
    assert_eq!(snap.capture_sample_rate_hz, 44_100);

    // Clone guest memory + capture source at snapshot time so baseline and restored runs see
    // identical host input and guest RAM.
    let mut mem_expected = mem.clone();
    let mut mem_restored = mem.clone();
    let capture_expected = capture.clone();
    let capture_restored = capture.clone();

    let mut expected = hda.clone();
    let mut expected_capture = capture_expected;
    expected.process_with_capture(&mut mem_expected, output_frames, &mut expected_capture);

    // Restore into a fresh controller. We intentionally do not call `set_capture_sample_rate_hz`;
    // the snapshot must carry the capture rate so the capture resampler state restores deterministically.
    let mut restored = HdaController::new();
    restored.restore_state(&snap);
    assert_eq!(restored.capture_sample_rate_hz(), 44_100);

    let mut restored_capture = capture_restored;
    restored.process_with_capture(&mut mem_restored, output_frames, &mut restored_capture);

    // The snapshot does not preserve the resampler's queued microphone samples (host resource),
    // but it should preserve how many *new* samples are consumed from the deterministic capture
    // source after restore.
    assert_eq!(restored_capture.len(), expected_capture.len());
}

#[test]
fn hda_snapshot_restore_clamps_corrupt_resampler_state_to_avoid_oom() {
    let hda = HdaController::new();
    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let mut snap = hda.snapshot_state(worklet_ring);
    assert!(!snap.stream_runtime.is_empty());

    // Simulate a corrupted snapshot that would otherwise attempt to allocate an
    // enormous resampler queue.
    snap.stream_runtime[0].resampler_queued_frames = u32::MAX;
    snap.stream_runtime[0].resampler_src_pos_bits = f64::NAN.to_bits();

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    let post = restored.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });

    assert!(post.stream_runtime[0].resampler_queued_frames <= 65_536);
    assert_eq!(
        post.stream_runtime[0].resampler_src_pos_bits,
        0.0f64.to_bits()
    );
}

#[test]
fn hda_snapshot_restore_clamps_snapshot_sample_rates_to_avoid_oom() {
    let hda = HdaController::new();
    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let mut snap = hda.snapshot_state(worklet_ring);

    snap.output_rate_hz = u32::MAX;
    snap.capture_sample_rate_hz = u32::MAX;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    assert_eq!(
        restored.output_rate_hz(),
        aero_audio::MAX_HOST_SAMPLE_RATE_HZ
    );
    assert_eq!(
        restored.capture_sample_rate_hz(),
        aero_audio::MAX_HOST_SAMPLE_RATE_HZ
    );
    assert_eq!(
        restored.audio_out.capacity_frames(),
        (aero_audio::MAX_HOST_SAMPLE_RATE_HZ / 10) as usize
    );
}

#[test]
fn hda_snapshot_restore_clamps_bdl_index_to_lvi() {
    let hda = HdaController::new();
    let mut mem = GuestMemory::new(0x4000);

    let bdl_base = 0x3f80u64;
    let buf = 0x1000u64;
    let buf_len = 512u32;

    // One BDL entry at index 0. (If restore doesn't clamp the snapshot-provided
    // bdl_index, the device would attempt to read an out-of-bounds entry and
    // panic in this unit test harness.)
    mem.write_u64(bdl_base, buf);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    for i in 0..buf_len {
        mem.write_u8(buf + i as u64, (i & 0xff) as u8);
    }

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let mut snap = hda.snapshot_state(worklet_ring);

    snap.gctl = 0x1; // out of reset
    snap.codec.output_stream_id = 1;
    snap.codec.output_channel = 0;

    // Guest stream format: 48kHz, 16-bit, stereo.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    snap.codec.output_format = fmt_raw;

    snap.streams[0].bdpl = bdl_base as u32;
    snap.streams[0].bdpu = 0;
    snap.streams[0].cbl = buf_len;
    snap.streams[0].lvi = 0;
    snap.streams[0].fmt = fmt_raw;
    // SRST | RUN | stream number 1.
    snap.streams[0].ctl = (1 << 0) | (1 << 1) | (1 << 20);

    // Corrupt runtime state: bdl_index is out of range for lvi=0.
    snap.stream_runtime[0].bdl_index = 10;
    snap.stream_runtime[0].bdl_offset = 0;
    snap.stream_runtime[0].last_fmt_raw = fmt_raw;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    // Must be clamped immediately on restore.
    let post = restored.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });
    assert_eq!(post.stream_runtime[0].bdl_index, 0);
    assert_eq!(post.stream_runtime[0].bdl_offset, 0);

    // Processing should not panic or read out of bounds.
    restored.process(&mut mem, 128);
}

#[test]
fn hda_snapshot_restore_clamps_capture_frame_accum_to_avoid_huge_capture_steps() {
    let hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    let worklet_ring = AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    };
    let mut snap = hda.snapshot_state(worklet_ring);
    assert!(snap.stream_capture_frame_accum.len() >= 2);

    snap.gctl = 0x1;

    // Configure codec ADC (NID 4) to use stream 2, channel 0 so capture stream is active.
    snap.codec_capture.input_stream_id = 2;
    snap.codec_capture.input_channel = 0;

    // Guest capture format: 48kHz, 16-bit, mono.
    let fmt_raw: u16 = 1 << 4;
    snap.codec_capture.input_format = fmt_raw;

    // One BDL entry so capture DMA has a valid target.
    let bdl_base = 0x1000u64;
    let buf = 0x2000u64;
    let buf_len = 512u32;
    mem.write_u64(bdl_base, buf);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    snap.streams[1].bdpl = bdl_base as u32;
    snap.streams[1].bdpu = 0;
    snap.streams[1].cbl = buf_len;
    snap.streams[1].lvi = 0;
    snap.streams[1].fmt = fmt_raw;
    // SRST | RUN | stream number 2.
    snap.streams[1].ctl = (1 << 0) | (1 << 1) | (2 << 20);

    // Corrupt accumulator to an enormous value; without clamping this would produce an absurd
    // `dst_frames` count on the next capture tick.
    snap.stream_capture_frame_accum[1] = u64::MAX;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    let post = restored.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });
    assert!(post.stream_capture_frame_accum[1] < restored.output_rate_hz() as u64);

    // Smoke test: processing capture should not attempt to allocate/run for an enormous frame count.
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.0; 128]);
    restored.process_with_capture(&mut mem, 1, &mut capture);
}

#[test]
fn hda_snapshot_restore_masks_stream_lvi_to_8bit() {
    let hda = HdaController::new();
    let mut snap = hda.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });

    snap.streams[0].lvi = 0x1234;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    assert_eq!(restored.stream_mut(0).lvi, 0x34);
}

#[test]
fn hda_snapshot_restore_masks_dplbase_reserved_bits() {
    let hda = HdaController::new();
    let mut snap = hda.snapshot_state(AudioWorkletRingState {
        capacity_frames: 256,
        write_pos: 0,
        read_pos: 0,
    });

    // DPLBASE: bit0 is enable, bits 6:1 are reserved and must read as 0, and the base is 128-byte aligned.
    snap.dplbase = 0x1234_567f;

    let mut restored = HdaController::new();
    restored.restore_state(&snap);

    let expected = (snap.dplbase & 1) | (snap.dplbase & !0x7f);
    assert_eq!(restored.mmio_read(REG_DPLBASE, 4) as u32, expected);
}
