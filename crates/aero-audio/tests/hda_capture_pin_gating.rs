use aero_audio::capture::VecDequeCaptureSource;
use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;

fn setup_basic_capture(
    hda: &mut HdaController,
    mem: &mut GuestMemory,
    frames: usize,
) -> (u64, usize) {
    // Bring controller out of reset.
    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Configure codec ADC (NID 4) to capture from stream 2, channel 0.
    hda.codec_mut().execute_verb(4, (0x706u32 << 8) | 0x20);

    // 48kHz, 16-bit, mono.
    let fmt_raw: u16 = 1 << 4;
    hda.codec_mut()
        .execute_verb(4, (0x200u32 << 8) | (fmt_raw as u8 as u32));

    // Guest buffer layout.
    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
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

    (pcm_base, pcm_len_bytes)
}

#[test]
fn capture_pin_ctl_zero_writes_silence_without_consuming() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    let frames = 8usize;
    let (pcm_base, pcm_len_bytes) = setup_basic_capture(&mut hda, &mut mem, frames);

    // Disable the mic pin via pin widget control (NID 5).
    hda.codec_mut().execute_verb(5, (0x707u32 << 8) | 0x00);

    // Fill guest buffer with non-zero bytes so we can observe the DMA overwrite.
    mem.write_physical(pcm_base, &vec![0xAA; pcm_len_bytes]);

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.5; 64]);
    let before_len = capture.len();

    hda.process_with_capture(&mut mem, frames, &mut capture);

    // Capture is gated, so we must DMA-write all-zero PCM and not consume any host samples.
    let mut out = vec![0u8; pcm_len_bytes];
    mem.read_physical(pcm_base, &mut out);
    assert!(out.iter().all(|&b| b == 0));
    assert_eq!(capture.len(), before_len);
    assert_eq!(hda.stream_mut(1).lpib, 0);
}

#[test]
fn capture_pin_power_state_d3_writes_silence_without_consuming() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    let frames = 8usize;
    let (pcm_base, pcm_len_bytes) = setup_basic_capture(&mut hda, &mut mem, frames);

    // Enable pin widget control, but power the mic pin down to D3.
    hda.codec_mut().execute_verb(5, (0x707u32 << 8) | 0x20);
    hda.codec_mut().execute_verb(5, (0x705u32 << 8) | 0x03);

    mem.write_physical(pcm_base, &vec![0xAA; pcm_len_bytes]);

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.5; 64]);
    let before_len = capture.len();

    hda.process_with_capture(&mut mem, frames, &mut capture);

    let mut out = vec![0u8; pcm_len_bytes];
    mem.read_physical(pcm_base, &mut out);
    assert!(out.iter().all(|&b| b == 0));
    assert_eq!(capture.len(), before_len);
    assert_eq!(hda.stream_mut(1).lpib, 0);
}

#[test]
fn capture_afg_power_state_d3_writes_silence_without_consuming() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);

    let frames = 8usize;
    let (pcm_base, pcm_len_bytes) = setup_basic_capture(&mut hda, &mut mem, frames);

    // Ensure the mic pin is enabled so capture gating is solely due to the AFG power state.
    hda.codec_mut().execute_verb(5, (0x707u32 << 8) | 0x20);

    // Power the Audio Function Group down to D3.
    hda.codec_mut().execute_verb(1, (0x705u32 << 8) | 0x03);

    mem.write_physical(pcm_base, &vec![0xAA; pcm_len_bytes]);

    let mut capture = VecDequeCaptureSource::new();
    capture.push_samples(&[0.5; 64]);
    let before_len = capture.len();

    hda.process_with_capture(&mut mem, frames, &mut capture);

    let mut out = vec![0u8; pcm_len_bytes];
    mem.read_physical(pcm_base, &mut out);
    assert!(out.iter().all(|&b| b == 0));
    assert_eq!(capture.len(), before_len);
    assert_eq!(hda.stream_mut(1).lpib, 0);
}
