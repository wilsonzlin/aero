use aero_audio::hda::HdaController;
use aero_audio::mem::{GuestMemory, MemoryAccess};

const REG_GCTL: u64 = 0x08;

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | payload8 as u32
}

fn verb_4(group: u16, payload16: u16) -> u32 {
    let verb_id = (group << 8) | ((payload16 >> 8) as u16);
    ((verb_id as u32) << 8) | (payload16 as u8 as u32)
}

fn setup_basic_playback(mem: &mut GuestMemory, hda: &mut HdaController, frames: usize) {
    // Bring controller out of reset.
    hda.mmio_write(REG_GCTL, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // Stream format: 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(2, set_fmt);

    // Guest buffer layout.
    let bdl_base = 0x1000u64;
    let pcm_base = 0x2000u64;
    let bytes_per_frame = 4usize; // 16-bit stereo
    let pcm_len_bytes = frames * bytes_per_frame;

    // Fill PCM with a constant non-zero sample so tests can reason about exact silence.
    let left = 0x2000i16;
    let right = 0x3000i16;
    for n in 0..frames {
        let off = pcm_base + (n * bytes_per_frame) as u64;
        mem.write_u16(off, left as u16);
        mem.write_u16(off + 2, right as u16);
    }

    // One BDL entry pointing at the PCM buffer.
    mem.write_u64(bdl_base + 0, pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
    mem.write_u32(bdl_base + 12, 0);

    // Configure stream descriptor 0.
    let sd = hda.stream_mut(0);
    sd.bdpl = bdl_base as u32;
    sd.bdpu = 0;
    sd.cbl = pcm_len_bytes as u32;
    sd.lvi = 0;
    sd.fmt = fmt_raw;
    // RUN | stream number 1.
    sd.ctl = (1 << 1) | (1 << 20);
}

fn render(mem: &mut GuestMemory, hda: &mut HdaController, frames: usize) -> Vec<f32> {
    hda.process(mem, frames);
    hda.audio_out.pop_interleaved_stereo(frames)
}

#[test]
fn default_output_is_not_silenced() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    let out = render(&mut mem, &mut hda, 16);
    assert!(out[0].abs() > 0.001);
    assert!(out[1].abs() > 0.001);
}

#[test]
fn amp_gain_scales_output() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    // Set output gain to ~50% for both channels.
    hda.codec_mut()
        .execute_verb(2, verb_4(0x3, 0x40 /* gain */));

    let out = render(&mut mem, &mut hda, 16);
    // Baseline left sample is 0x2000 / 32768 = 0.25.
    assert!(out[0] > 0.0);
    assert!(out[0] < 0.25);
    // Baseline right sample is 0x3000 / 32768 = 0.375.
    assert!(out[1] > 0.0);
    assert!(out[1] < 0.375);
}

#[test]
fn mute_left_silences_left_channel() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    // Mute left channel; keep gain at unity.
    let payload = (1 << 13) | (1 << 7) | 0x7f;
    hda.codec_mut()
        .execute_verb(2, verb_4(0x3, payload as u16));

    let out = render(&mut mem, &mut hda, 16);
    assert_eq!(out[0], 0.0);
    assert!(out[1].abs() > 0.001);
}

#[test]
fn mute_right_silences_right_channel() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    // Mute right channel; keep gain at unity.
    let payload = (1 << 12) | (1 << 7) | 0x7f;
    hda.codec_mut()
        .execute_verb(2, verb_4(0x3, payload as u16));

    let out = render(&mut mem, &mut hda, 16);
    assert!(out[0].abs() > 0.001);
    assert_eq!(out[1], 0.0);
}

#[test]
fn afg_power_state_d3_silences_output() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    // SET_POWER_STATE (AFG nid=1) to D3.
    hda.codec_mut().execute_verb(1, verb_12(0x705, 0x03));

    let out = render(&mut mem, &mut hda, 16);
    assert!(out.iter().all(|&s| s == 0.0));
}

#[test]
fn pin_ctl_zero_silences_output() {
    let mut hda = HdaController::new();
    let mut mem = GuestMemory::new(0x20_000);
    setup_basic_playback(&mut mem, &mut hda, 16);

    // Disable line-out pin (nid=3). Minimal model treats pin_ctl==0 as disabled.
    hda.codec_mut().execute_verb(3, verb_12(0x707, 0x00));

    let out = render(&mut mem, &mut hda, 16);
    assert!(out.iter().all(|&s| s == 0.0));
}

