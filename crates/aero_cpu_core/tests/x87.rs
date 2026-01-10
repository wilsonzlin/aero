use aero_cpu_core::fpu::FpuState;
use aero_cpu_core::interp::x87::{Fault, Tag, X87};

const FSW_IE: u16 = 1 << 0;
const FSW_ZE: u16 = 1 << 2;
const FSW_SF: u16 = 1 << 6;
const FSW_ES: u16 = 1 << 7;
const FSW_C0: u16 = 1 << 8;
const FSW_C2: u16 = 1 << 10;
const FSW_C3: u16 = 1 << 14;

#[test]
fn simple_arithmetic_sequence() {
    let mut fpu = X87::default();
    fpu.fld_f64(1.5).unwrap();
    fpu.fld_f64(2.0).unwrap(); // ST0=2.0, ST1=1.5
    fpu.fadd_st0_sti(1).unwrap(); // ST0 = 3.5
    fpu.fmul_m64(10.0).unwrap(); // ST0 = 35.0
    fpu.fchs().unwrap(); // ST0 = -35.0
    fpu.fabs().unwrap(); // ST0 = 35.0
    assert_eq!(fpu.fstp_f64().unwrap(), 35.0);
}

#[test]
fn stack_push_pop_and_top_updates() {
    let mut fpu = X87::default();
    assert_eq!(fpu.top(), 0);

    fpu.fld_f64(1.0).unwrap();
    assert_eq!(fpu.top(), 7);

    fpu.fld_f64(2.0).unwrap();
    assert_eq!(fpu.top(), 6);

    assert_eq!(fpu.fstp_f64().unwrap(), 2.0);
    assert_eq!(fpu.top(), 7);

    assert_eq!(fpu.fstp_f64().unwrap(), 1.0);
    assert_eq!(fpu.top(), 0);
}

#[test]
fn tag_word_updates_with_push_and_pop() {
    let mut fpu = X87::default();
    assert_eq!(fpu.tag_word(), 0xFFFF);

    fpu.fld_f64(1.0).unwrap();
    // Physical register 7 holds the first push.
    assert_eq!(fpu.tag_word(), 0x3FFF);

    fpu.fld_f64(0.0).unwrap();
    // Physical register 6 is now a zero, register 7 is valid.
    assert_eq!(fpu.tag_word(), 0x1FFF);
    assert_eq!(fpu.st_tag(0).unwrap(), Tag::Zero);
    assert_eq!(fpu.st_tag(1).unwrap(), Tag::Valid);

    fpu.fstp_f64().unwrap();
    fpu.fstp_f64().unwrap();
    assert_eq!(fpu.tag_word(), 0xFFFF);
}

#[test]
fn control_word_roundtrip() {
    let mut fpu = X87::default();
    assert_eq!(fpu.fnstcw(), 0x037F);
    fpu.fldcw(0x1234);
    assert_eq!(fpu.fnstcw(), 0x1234);
}

#[test]
fn compare_sets_condition_codes_and_signals_unordered() {
    let mut fpu = X87::default();
    fpu.fld_f64(1.0).unwrap();
    fpu.fld_f64(2.0).unwrap(); // ST0=2.0, ST1=1.0

    fpu.fcom_sti(1).unwrap();
    assert_eq!(fpu.status_word() & (FSW_C0 | FSW_C2 | FSW_C3), 0);

    fpu.fcom_m64(3.0).unwrap();
    assert_eq!(fpu.status_word() & (FSW_C0 | FSW_C2 | FSW_C3), FSW_C0);

    fpu.fcom_m64(2.0).unwrap();
    assert_eq!(fpu.status_word() & (FSW_C0 | FSW_C2 | FSW_C3), FSW_C3);

    // Default FCW masks invalid operation, so unordered does not fault but sets flags.
    fpu.fcom_m64(f64::NAN).unwrap();
    assert_eq!(
        fpu.status_word() & (FSW_C0 | FSW_C2 | FSW_C3),
        FSW_C0 | FSW_C2 | FSW_C3
    );
    assert_ne!(fpu.status_word() & FSW_IE, 0);
    assert_eq!(fpu.status_word() & FSW_ES, 0);
}

#[test]
fn unmasked_exceptions_return_mathfault() {
    let mut fpu = X87::default();

    // Unmask invalid operation (IM=0).
    fpu.fldcw(fpu.control_word() & !1);
    assert_eq!(fpu.fst_f64().unwrap_err(), Fault::MathFault);
    assert_ne!(fpu.status_word() & FSW_IE, 0);
    assert_ne!(fpu.status_word() & FSW_ES, 0);
    assert_ne!(fpu.status_word() & FSW_SF, 0);

    // Unmask zero divide (ZM=0) and ensure we fault without consuming the stack.
    let mut fpu = X87::default();
    fpu.fld_f64(1.0).unwrap();
    fpu.fldcw(fpu.control_word() & !(1 << 2));
    assert_eq!(fpu.fdiv_m64(0.0).unwrap_err(), Fault::MathFault);
    assert_ne!(fpu.status_word() & FSW_ZE, 0);
    assert_eq!(fpu.st(0), Some(1.0));
}

#[test]
fn fxsave_bridge_roundtrips_stack_registers() {
    let mut x87 = X87::default();
    x87.fld_f64(1.0).unwrap();
    x87.fld_f64(2.0).unwrap(); // ST0=2.0, ST1=1.0

    let mut state = FpuState::default();
    x87.store_to_fpu_state(&mut state);

    assert_eq!(state.fcw, x87.control_word());
    assert_eq!(state.top, x87.top());
    // Physical regs 6 and 7 are occupied after two pushes.
    assert_eq!(state.ftw, 0xC0);

    let mut restored = X87::default();
    restored.load_from_fpu_state(&state);
    assert_eq!(restored.top(), x87.top());
    assert_eq!(restored.st(0), Some(2.0));
    assert_eq!(restored.st(1), Some(1.0));
}

#[test]
fn reverse_sub_and_div_variants_behave_plausibly() {
    let mut fpu = X87::default();
    fpu.fld_f64(2.0).unwrap();
    fpu.fsubr_m64(10.0).unwrap(); // ST0 = 10 - 2
    assert_eq!(fpu.st(0), Some(8.0));

    fpu.fld_f64(3.0).unwrap(); // ST0=3, ST1=8
    fpu.fsubr_st0_sti(1).unwrap(); // ST0 = ST1 - ST0
    assert_eq!(fpu.st(0), Some(5.0));
    fpu.fsubrp_sti_st0(1).unwrap(); // ST1 = ST0 - ST1; pop
    assert_eq!(fpu.st(0), Some(-3.0));
    assert_eq!(fpu.fstp_f64().unwrap(), -3.0);

    let mut fpu = X87::default();
    fpu.fld_f64(2.0).unwrap();
    fpu.fdivr_m64(10.0).unwrap(); // ST0 = 10 / 2
    assert_eq!(fpu.st(0), Some(5.0));

    fpu.fld_f64(10.0).unwrap(); // ST0=10, ST1=5
    fpu.fdivr_sti_st0(1).unwrap(); // ST1 = ST0 / ST1
    assert_eq!(fpu.st(1), Some(2.0));
    fpu.fdivrp_sti_st0(1).unwrap(); // ST1 = ST0 / ST1; pop
    assert_eq!(fpu.st(0), Some(5.0));
}
