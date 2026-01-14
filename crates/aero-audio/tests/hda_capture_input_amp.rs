use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};
use aero_audio::pcm::{decode_pcm_to_stereo_f32, StreamFormat};

const REG_GCTL: u64 = 0x08;

const SD_CTL_SRST: u32 = 1 << 0;
const SD_CTL_RUN: u32 = 1 << 1;
const SD_CTL_STRM_SHIFT: u32 = 20;

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | payload8 as u32
}

fn verb_4(group: u16, payload16: u16) -> u32 {
    let verb_id = (group << 8) | (payload16 >> 8);
    ((verb_id as u32) << 8) | (payload16 as u8 as u32)
}

fn setup_capture_stream(
    hda: &mut HdaController,
    mem: &mut GuestMemory,
    stream_id: u8,
    fmt_raw: u16,
    bdl_base: u64,
    buf_base: u64,
    buf_len: u32,
) {
    // Bring controller out of reset.
    hda.mmio_write(REG_GCTL, 4, 0x1);

    // Ensure the mic pin widget is enabled; the codec model gates capture when the pin is disabled.
    hda.codec_mut().execute_verb(5, verb_12(0x707, 0x40));
    hda.codec_mut().execute_verb(5, verb_12(0x705, 0x00));

    // Configure the codec ADC (NID 4) to use `stream_id`.
    hda.codec_mut()
        .execute_verb(4, verb_12(0x706, stream_id << 4));

    // Configure guest capture format.
    hda.codec_mut().execute_verb(4, verb_4(0x2, fmt_raw));

    // One-entry BDL.
    mem.write_u64(bdl_base, buf_base);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = buf_len;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        sd.ctl = SD_CTL_SRST | SD_CTL_RUN | ((stream_id as u32) << SD_CTL_STRM_SHIFT);
    }
}

fn run_capture_and_decode(
    hda: &mut HdaController,
    mem: &mut GuestMemory,
    capture: &mut VecDequeCaptureSource,
    output_frames: usize,
    fmt_raw: u16,
    buf_base: u64,
) -> Vec<[f32; 2]> {
    hda.process_with_capture(mem, output_frames, capture);

    let fmt = StreamFormat::from_hda_format(fmt_raw);
    let bytes_written = output_frames * fmt.bytes_per_frame();
    let mut pcm = vec![0u8; bytes_written];
    mem.read_physical(buf_base, &mut pcm);
    decode_pcm_to_stereo_f32(&pcm, fmt)
}

#[test]
fn capture_produces_non_zero_pcm_by_default() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x10_000);

    let stream_id = 2u8;
    let fmt_raw: u16 = 1 << 4; // 48kHz, 16-bit, mono.
    let bdl_base = 0x1000u64;
    let buf_base = 0x2000u64;
    let buf_len = 0x2000u32;

    setup_capture_stream(
        &mut hda, &mut mem, stream_id, fmt_raw, bdl_base, buf_base, buf_len,
    );

    let output_frames = 480usize; // 10ms @ 48kHz.
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&vec![0.25f32; output_frames]);

    let decoded = run_capture_and_decode(
        &mut hda,
        &mut mem,
        &mut capture,
        output_frames,
        fmt_raw,
        buf_base,
    );
    assert_eq!(decoded.len(), output_frames);
    let max_abs = decoded
        .iter()
        .map(|f| f[0].abs().max(f[1].abs()))
        .fold(0.0f32, f32::max);
    assert!(
        max_abs > 0.01,
        "expected non-zero capture PCM, got max_abs={max_abs}"
    );
}

#[test]
fn capture_is_silenced_when_input_amp_muted() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x10_000);

    let stream_id = 2u8;
    let fmt_raw: u16 = 1 << 4; // 48kHz, 16-bit, mono.
    let bdl_base = 0x1000u64;
    let buf_base = 0x2000u64;
    let buf_len = 0x2000u32;

    setup_capture_stream(
        &mut hda, &mut mem, stream_id, fmt_raw, bdl_base, buf_base, buf_len,
    );

    // Mute ADC input amp (direction=in, index=0, both channels, mute=1).
    let mute_payload: u16 = (1 << 15) | (1 << 7) | 0x7f;
    hda.codec_mut().execute_verb(4, verb_4(0x3, mute_payload));

    let output_frames = 480usize;
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&vec![0.25f32; output_frames]);

    let decoded = run_capture_and_decode(
        &mut hda,
        &mut mem,
        &mut capture,
        output_frames,
        fmt_raw,
        buf_base,
    );
    assert_eq!(decoded.len(), output_frames);
    let max_abs = decoded
        .iter()
        .map(|f| f[0].abs().max(f[1].abs()))
        .fold(0.0f32, f32::max);
    assert!(
        max_abs <= 1e-6,
        "expected muted capture to be silent, got max_abs={max_abs}"
    );
}

#[test]
fn capture_amplitude_is_scaled_by_input_amp_gain() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x10_000);

    let stream_id = 2u8;
    let fmt_raw: u16 = 1 << 4; // 48kHz, 16-bit, mono.
    let bdl_base = 0x1000u64;
    let buf_base = 0x2000u64;
    let buf_len = 0x2000u32;

    setup_capture_stream(
        &mut hda, &mut mem, stream_id, fmt_raw, bdl_base, buf_base, buf_len,
    );

    // Low gain, unmuted.
    let gain: u16 = 0x10;
    let set_gain_payload: u16 = (1 << 15) | gain;
    hda.codec_mut()
        .execute_verb(4, verb_4(0x3, set_gain_payload));

    let output_frames = 480usize;
    let input_sample = 0.5f32;
    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&vec![input_sample; output_frames]);

    let decoded = run_capture_and_decode(
        &mut hda,
        &mut mem,
        &mut capture,
        output_frames,
        fmt_raw,
        buf_base,
    );
    assert_eq!(decoded.len(), output_frames);

    let observed = decoded[0][0];
    let expected = input_sample * (gain as f32 / 0x7f as f32);
    let diff = (observed - expected).abs();
    // Allow some slack for 16-bit quantization.
    assert!(
        diff <= 5e-4,
        "expected first sample ~= {expected}, got {observed} (|diff|={diff})"
    );
    assert!(observed.abs() < input_sample.abs());
}
