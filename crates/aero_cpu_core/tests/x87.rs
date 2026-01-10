use aero_cpu_core::interp::x87::{Tag, X87};

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
