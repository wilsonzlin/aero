use aero_audio::hda::HdaController;

#[test]
fn hda_clamps_host_sample_rates_to_avoid_oom() {
    // Constructor should clamp.
    let hda = HdaController::new_with_output_rate(u32::MAX);
    assert_eq!(hda.output_rate_hz(), aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
    assert_eq!(
        hda.audio_out.capacity_frames(),
        (aero_audio::MAX_HOST_SAMPLE_RATE_HZ / 10) as usize
    );

    // Setters should clamp too.
    let mut hda2 = HdaController::new();
    hda2.set_output_rate_hz(u32::MAX);
    assert_eq!(hda2.output_rate_hz(), aero_audio::MAX_HOST_SAMPLE_RATE_HZ);

    hda2.set_capture_sample_rate_hz(u32::MAX);
    assert_eq!(
        hda2.capture_sample_rate_hz(),
        aero_audio::MAX_HOST_SAMPLE_RATE_HZ
    );
}
