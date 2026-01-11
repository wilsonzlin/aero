#[test]
fn register_classification_methods_exist() {
    use aero_x86::Register;

    assert!(Register::XMM0.is_xmm());
    assert!(!Register::XMM0.is_mm());

    assert!(Register::MM0.is_mm());
    assert!(!Register::MM0.is_xmm());

    assert!(Register::ST0.is_st());
    assert!(!Register::ST0.is_xmm());

    assert!(Register::CS.is_segment_register());
    assert!(!Register::CS.is_xmm());
}
