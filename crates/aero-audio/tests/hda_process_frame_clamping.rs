use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[test]
fn hda_process_clamps_extreme_frame_counts_to_avoid_oom() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // Guest buffer layout: a small circular buffer that the DMA engine will wrap.
    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let pcm_len_bytes = 4096u32; // Multiple of 4 (16-bit stereo frames).

    // One BDL entry pointing at the PCM buffer, IOC=0.
    mem.write_u64(bdl_base, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 0: 48kHz, 16-bit, stereo.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 20);
    }

    // Advancing by an extreme number of frames should not cause an OOM; the device should clamp
    // per-call work to a reasonable upper bound.
    hda.process(&mut mem, usize::MAX);

    let telemetry = hda.audio_out.telemetry();
    let produced_frames = telemetry.available_frames as u64 + telemetry.overrun_frames;

    // We expect the device to clamp to <=1s worth of frames (i.e. output sample rate).
    assert_eq!(produced_frames, hda.output_rate_hz() as u64);
    assert_eq!(telemetry.available_frames, hda.audio_out.capacity_frames());
    assert_eq!(
        telemetry.overrun_frames,
        hda.output_rate_hz() as u64 - hda.audio_out.capacity_frames() as u64
    );
}
