use aero_audio::capture::AudioCaptureSource;
use aero_audio::hda::{HdaCaptureTelemetry, HdaController};
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[derive(Debug)]
struct TestCaptureSource {
    dropped_delta: u64,
    samples_to_return: usize,
}

impl AudioCaptureSource for TestCaptureSource {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        let count = self.samples_to_return.min(out.len());
        for slot in &mut out[..count] {
            *slot = 0.5;
        }
        self.samples_to_return = 0;
        count
    }

    fn take_dropped_samples(&mut self) -> u64 {
        let v = self.dropped_delta;
        self.dropped_delta = 0;
        v
    }
}

#[test]
fn hda_capture_telemetry_tracks_drops_and_underruns() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec ADC (NID 4) to capture from stream 2, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = 1 << 4;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(4, set_fmt);

    // Enable mic pin capture so `capture_enabled()` is true and the controller consumes samples
    // from the host `AudioCaptureSource`.
    let enable_mic_pin = (0x707u32 << 8) | 0x20; // Pin Widget Control: IN enable.
    hda.codec_mut().execute_verb(5, enable_mic_pin);

    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let frames = 8usize;
    let bytes_per_frame = 2usize;
    let pcm_len_bytes = frames * bytes_per_frame;

    // One BDL entry pointing at the capture buffer.
    mem.write_u64(bdl_base, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 1 (capture).
    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes as u32;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    assert_eq!(hda.capture_telemetry(), HdaCaptureTelemetry::default());

    // The capture source reports a dropped-samples delta and under-produces samples.
    let dropped_delta = 7u64;
    let got_samples = 3usize;
    let mut capture = TestCaptureSource {
        dropped_delta,
        samples_to_return: got_samples,
    };

    hda.process_with_capture(&mut mem, frames, &mut capture);

    let telemetry = hda.capture_telemetry();
    assert_eq!(telemetry.dropped_samples, dropped_delta);
    assert_eq!(telemetry.underrun_samples, (frames - got_samples) as u64);
    assert_eq!(telemetry.underrun_ticks, 1);
}
