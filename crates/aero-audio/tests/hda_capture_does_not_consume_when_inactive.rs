use aero_audio::capture::AudioCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | payload8 as u32
}

#[derive(Debug, Default)]
struct CountingCaptureSource {
    read_calls: u64,
    samples_read: u64,
}

impl AudioCaptureSource for CountingCaptureSource {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        self.read_calls += 1;
        out.fill(1.0);
        self.samples_read += out.len() as u64;
        out.len()
    }
}

#[test]
fn hda_capture_does_not_consume_capture_source_when_stream_inactive() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Configure the codec ADC (NID 4) to capture from stream 2, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = 1 << 4;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u32);
    hda.codec_mut().execute_verb(4, set_fmt);

    // Satisfy mic pin/power gating so capture works when the stream is active. This keeps the test
    // robust against future codec modeling changes.
    hda.codec_mut().execute_verb(1, verb_12(0x705, 0x00)); // AFG power state D0.
    hda.codec_mut().execute_verb(5, verb_12(0x705, 0x00)); // Mic pin power state D0.
    hda.codec_mut().execute_verb(5, verb_12(0x707, 1 << 5)); // Pin widget control: IN_EN.

    // Guest buffer layout (one BDL entry).
    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 8usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    mem.write_u64(bdl_base, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 1 (capture), but leave it inactive (SRST=1, RUN=0).
    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST=1, stream number 2, RUN=0.
        sd.ctl = (1 << 0) | (2 << 20);
    }

    let mut capture = CountingCaptureSource::default();

    // 1) Stream inactive: do not consume host mic samples.
    hda.process_with_capture(&mut mem, frames, &mut capture);
    assert_eq!(capture.read_calls, 0);
    assert_eq!(capture.samples_read, 0);

    // 2) Stream active: once RUN is set and the stream tag matches the codec, samples should be
    // consumed.
    hda.stream_mut(1).ctl = (1 << 0) | (1 << 1) | (2 << 20);
    hda.process_with_capture(&mut mem, frames, &mut capture);
    assert!(capture.read_calls > 0);
    assert!(capture.samples_read > 0);
}
