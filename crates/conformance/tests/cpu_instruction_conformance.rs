#![cfg(not(target_arch = "wasm32"))]

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn instruction_conformance_host_reference() {
    let _guard = ENV_LOCK.lock().unwrap();
    if !cfg!(all(target_arch = "x86_64", unix)) {
        eprintln!("skipping instruction conformance tests on non-x86_64/unix host");
        return;
    }

    let report = conformance::run_from_env().expect("conformance run failed");
    assert_eq!(report.failures, 0, "conformance failures detected");
}

#[cfg(feature = "qemu-reference")]
#[test]
fn instruction_conformance_qemu_reference() {
    let _guard = ENV_LOCK.lock().unwrap();
    if !qemu_diff::qemu_available() {
        eprintln!("qemu-system-* not found; skipping qemu reference conformance tests");
        return;
    }

    // Keep the run small since each case spawns a QEMU instance.
    let old_reference = std::env::var("AERO_CONFORMANCE_REFERENCE").ok();
    let old_cases = std::env::var("AERO_CONFORMANCE_CASES").ok();

    std::env::set_var("AERO_CONFORMANCE_REFERENCE", "qemu");
    std::env::set_var("AERO_CONFORMANCE_CASES", "16");

    let report = conformance::run_from_env().expect("qemu conformance run failed");
    assert_eq!(report.failures, 0, "qemu conformance failures detected");

    match old_reference {
        Some(v) => std::env::set_var("AERO_CONFORMANCE_REFERENCE", v),
        None => std::env::remove_var("AERO_CONFORMANCE_REFERENCE"),
    }
    match old_cases {
        Some(v) => std::env::set_var("AERO_CONFORMANCE_CASES", v),
        None => std::env::remove_var("AERO_CONFORMANCE_CASES"),
    }
}
