#[test]
fn instruction_conformance() {
    if !cfg!(all(target_arch = "x86_64", unix)) {
        eprintln!("skipping conformance tests on non-x86_64/unix host");
        return;
    }

    let report = conformance::run_from_env().expect("conformance run failed");
    assert_eq!(report.failures, 0, "conformance failures detected");
}
