#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn conformance_help_prints_usage() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["conformance", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo xtask conformance"))
        .stdout(predicate::str::contains("--cases"))
        .stdout(predicate::str::contains("--report-path"));
}

#[test]
fn top_level_help_mentions_conformance() {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("conformance"));
}

#[test]
#[cfg(all(target_arch = "x86_64", unix))]
fn conformance_smoke_runs_small_corpus() {
    let tmp = tempfile::tempdir().unwrap();
    let report = tmp.path().join("report.json");

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "conformance",
            "--cases",
            "16",
            "--report-path",
            report.to_str().unwrap(),
            "--",
            "instruction_conformance_host_reference",
        ])
        .assert()
        .success();

    let json = std::fs::read_to_string(&report).expect("report.json should be written");
    assert!(
        json.contains("\"total_cases\": 16"),
        "report should include total_cases=16; got:\n{json}"
    );
    assert!(
        json.contains("\"failures\": 0"),
        "report should include failures=0; got:\n{json}"
    );
}
