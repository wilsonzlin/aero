use aero_cpu_core::assist::{handle_assist_decoded, AssistContext};
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::time::TimeSource;

#[test]
fn invlpg_log_is_capped_and_counts_drops() {
    let mut ctx = AssistContext::default();
    let mut time = TimeSource::default();
    let mut state = CpuState::new(CpuMode::Protected);
    // Ensure CPL0 so INVLPG is permitted.
    state.segments.cs.selector = 0x08;
    let mut bus = FlatTestBus::new(0);

    // invlpg [disp32]
    let bytes = [0x0F, 0x01, 0x3D, 0x00, 0x00, 0x00, 0x00];
    let decoded = aero_x86::decode(&bytes, 0, state.bitness()).expect("decode INVLPG");

    let extra = 32usize;
    for _ in 0..(AssistContext::INVLPG_LOG_CAP + extra) {
        handle_assist_decoded(&mut ctx, &mut time, &mut state, &mut bus, &decoded, false)
            .expect("assist invlpg");
    }

    assert_eq!(ctx.invlpg_log.len(), AssistContext::INVLPG_LOG_CAP);
    assert_eq!(ctx.invlpg_log_dropped(), extra as u64);
}
