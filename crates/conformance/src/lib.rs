//! Differential/conformance testing for instruction semantics.
//!
//! This crate runs small, deterministic instruction corpora in two backends:
//! - **Aero backend**: a tiny interpreter used as a stand-in for the real Aero CPU core
//! - **Reference backend**: native host execution on `x86_64` (user-mode only)
//!
//! The host backend is intentionally limited to "safe" user-mode instructions and does not attempt
//! to cover privileged instructions, system registers, or architecturally-undefined behaviour.

mod aero;
mod corpus;
mod reference;
mod report;

pub use report::{ConformanceReport, CoverageSummary};

use crate::corpus::{InstructionTemplate, TestCase};
use crate::reference::ReferenceBackend;

pub(crate) const FLAG_CF: u64 = 1 << 0;
pub(crate) const FLAG_PF: u64 = 1 << 2;
pub(crate) const FLAG_AF: u64 = 1 << 4;
pub(crate) const FLAG_ZF: u64 = 1 << 6;
pub(crate) const FLAG_SF: u64 = 1 << 7;
pub(crate) const FLAG_OF: u64 = 1 << 11;
pub(crate) const FLAG_FIXED_1: u64 = 1 << 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct CpuState {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
    pub rip: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub state: CpuState,
    pub memory: Vec<u8>,
    pub fault: Option<Fault>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Signal(i32),
    Unsupported(&'static str),
    MemoryOutOfBounds,
}

pub fn run_from_env() -> Result<ConformanceReport, String> {
    let cases = std::env::var("AERO_CONFORMANCE_CASES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(512);
    let seed = std::env::var("AERO_CONFORMANCE_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0x_52c6_71d9_a4f2_31b9);
    let report_path =
        std::env::var_os("AERO_CONFORMANCE_REPORT_PATH").map(std::path::PathBuf::from);

    let report = run(cases, seed, report_path.as_deref())?;
    Ok(report)
}

pub fn run(
    cases: usize,
    seed: u64,
    report_path: Option<&std::path::Path>,
) -> Result<ConformanceReport, String> {
    let mut reference =
        ReferenceBackend::new().map_err(|e| format!("reference backend unavailable: {e}"))?;
    let mut aero = aero::AeroBackend::new();

    let templates = corpus::templates();
    let mut rng = corpus::XorShift64::new(seed);
    let mut report = ConformanceReport::new(cases);

    for (case_idx, template) in templates.iter().cycle().take(cases).enumerate() {
        let test_case = TestCase::generate(case_idx, template, &mut rng, reference.memory_base());
        report.coverage.increment(template.coverage_key);

        let expected = reference.execute(&test_case);
        let actual = aero.execute(&test_case);

        if let Some(message) = compare_outcomes(template, &test_case, &expected, &actual) {
            report.failures += 1;
            if let Some(report_path) = report_path {
                let _ = report.write_json(report_path);
            }
            return Err(message);
        }
    }

    if let Some(report_path) = report_path {
        report
            .write_json(report_path)
            .map_err(|e| format!("failed to write report: {e}"))?;
    }

    report.print_summary();
    Ok(report)
}

fn compare_outcomes(
    template: &InstructionTemplate,
    case: &TestCase,
    expected: &ExecOutcome,
    actual: &ExecOutcome,
) -> Option<String> {
    if expected.fault != actual.fault {
        return Some(report::format_failure(template, case, expected, actual));
    }

    if expected.fault.is_some() {
        return None;
    }

    let expected_state = &expected.state;
    let actual_state = &actual.state;

    if !report::states_equal(expected_state, actual_state, template.flags_mask) {
        return Some(report::format_failure(template, case, expected, actual));
    }

    if template.mem_compare_len > 0
        && !report::memory_equal(&expected.memory, &actual.memory, template.mem_compare_len)
    {
        return Some(report::format_failure(template, case, expected, actual));
    }

    None
}
