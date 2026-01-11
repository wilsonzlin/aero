use aero_cpu_core::interp::tier0::exec::step_with_config;
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{
    CpuMode, CpuState, CR4_OSFXSR, CR4_OSXMMEXCPT, MXCSR_IE, MXCSR_IM, MXCSR_ZE, MXCSR_ZM,
};
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x4000;
const CODE_BASE: u64 = 0x1000;

fn new_sse_state(mode: CpuMode) -> CpuState {
    let mut state = CpuState::new(mode);
    state.control.cr4 |= CR4_OSFXSR | CR4_OSXMMEXCPT;
    state
}

fn exec_once(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut FlatTestBus,
    code: &[u8],
) -> Result<(), Exception> {
    bus.load(CODE_BASE, code);
    state.set_rip(CODE_BASE);
    step_with_config(cfg, state, bus).map(|_| ())
}

#[test]
fn divss_unmasked_ze_raises_xm() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Unmask divide-by-zero.
    state.sse.mxcsr &= !MXCSR_ZM;

    let xmm0_old = 0x1111_2222_3333_4444_5555_6666_3f80_0000u128; // 1.0f32
    state.sse.xmm[0] = xmm0_old;
    state.sse.xmm[1] = 0x9999_aaaa_bbbb_cccc_dddd_eeee_0000_0000u128; // 0.0f32

    // divss xmm0, xmm1
    let err = exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x5E, 0xC1]).unwrap_err();
    assert_eq!(err, Exception::SimdFloatingPointException);

    // Fault semantics: no destination write + RIP not advanced.
    assert_eq!(state.sse.xmm[0], xmm0_old);
    assert_eq!(state.rip(), CODE_BASE);

    // MXCSR exception flags are sticky even on #XM.
    assert_ne!(state.sse.mxcsr & MXCSR_ZE, 0);
}

#[test]
fn divss_masked_ze_sets_flag_and_continues() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Keep divide-by-zero masked (default MXCSR has all exception masks set).
    state.sse.mxcsr |= MXCSR_ZM;

    let xmm0_old = 0x1111_2222_3333_4444_5555_6666_3f80_0000u128; // 1.0f32
    state.sse.xmm[0] = xmm0_old;
    state.sse.xmm[1] = 0x9999_aaaa_bbbb_cccc_dddd_eeee_0000_0000u128; // 0.0f32

    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x5E, 0xC1]).unwrap(); // divss xmm0, xmm1

    assert_ne!(state.sse.mxcsr & MXCSR_ZE, 0);
    assert_eq!(state.rip(), CODE_BASE + 4);

    // 1.0 / 0.0 = +inf
    let expected = (xmm0_old & !0xFFFF_FFFFu128) | (0x7f80_0000u128);
    assert_eq!(state.sse.xmm[0], expected);
}

#[test]
fn cvtss2si_unmasked_ie_raises_xm() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // Unmask invalid operation.
    state.sse.mxcsr &= !MXCSR_IM;

    state.sse.xmm[0] = 0x1111_2222_3333_4444_5555_6666_7fc0_0000u128; // qNaN

    let rax_old = 0xDEAD_BEEF_F00D_CAFE;
    state.write_reg(Register::RAX, rax_old);

    // cvtss2si eax, xmm0
    let err = exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x2D, 0xC0]).unwrap_err();
    assert_eq!(err, Exception::SimdFloatingPointException);

    assert_eq!(state.read_reg(Register::RAX), rax_old);
    assert_eq!(state.rip(), CODE_BASE);
    assert_ne!(state.sse.mxcsr & MXCSR_IE, 0);
}
