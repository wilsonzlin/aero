//! Differential/conformance testing for instruction semantics.
//!
//! This crate runs small, deterministic instruction corpora in two backends:
//! - **Aero backend**: the real `aero_cpu_core` Tier-0 interpreter (one instruction per case)
//! - **Reference backend**: native host execution on `x86_64` (user-mode only)
//!
//! The host backend is intentionally limited to "safe" user-mode instructions and does not attempt
//! to cover privileged instructions, system registers, or architecturally-undefined behaviour.
//!
//! ## Environment variables
//!
//! When running via [`run_from_env`], the following environment variables are recognised:
//!
//! - `AERO_CONFORMANCE_CASES` (default: `512`): total number of generated test cases to execute.
//! - `AERO_CONFORMANCE_SEED` (default: `0x52c6_71d9_a4f2_31b9`): RNG seed for deterministic runs.
//! - `AERO_CONFORMANCE_FILTER` (optional): only run templates whose `name` or `coverage_key`
//!   contains this substring (case-insensitive).
//! - `AERO_CONFORMANCE_REPORT_PATH` (optional): write a JSON conformance report to this path
//!   (on first failure and again at the end of the run).

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
        .and_then(|v| parse_cases_env(&v))
        .unwrap_or(512);
    let seed = std::env::var("AERO_CONFORMANCE_SEED")
        .ok()
        .and_then(|v| parse_seed_env(&v))
        .unwrap_or(0x_52c6_71d9_a4f2_31b9);
    let report_path =
        std::env::var_os("AERO_CONFORMANCE_REPORT_PATH").map(std::path::PathBuf::from);
    run(cases, seed, report_path.as_deref())
}

fn parse_cases_env(input: &str) -> Option<usize> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let cleaned: String = trimmed.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<usize>().ok()
}

fn parse_seed_env(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Allow copying the in-code constant (which uses `_` separators) and allow common `0x...`
    // notation for seeds.
    let cleaned: String = trimmed.chars().filter(|c| *c != '_').collect();
    let cleaned = cleaned.as_str();
    let (radix, digits) = match cleaned.strip_prefix("0x").or_else(|| cleaned.strip_prefix("0X")) {
        Some(rest) => (16, rest),
        None => (10, cleaned),
    };
    if digits.is_empty() {
        return None;
    }

    u64::from_str_radix(digits, radix).ok()
}

pub fn run(
    cases: usize,
    seed: u64,
    report_path: Option<&std::path::Path>,
) -> Result<ConformanceReport, String> {
    let templates = templates_for_run()?;
    // Fault templates intentionally crash in user mode on the host reference backend.
    // If isolation is disabled, fail fast with a clear message instead of taking down
    // the entire test runner process.
    let isolate = std::env::var("AERO_CONFORMANCE_REFERENCE_ISOLATE")
        .map(|v| v != "0")
        .unwrap_or(true);
    if !isolate
        && templates.iter().any(|t| {
            matches!(
                t.kind,
                corpus::TemplateKind::Ud2 | corpus::TemplateKind::MovRaxM64Abs0
            )
        })
    {
        return Err(
            "fault templates require AERO_CONFORMANCE_REFERENCE_ISOLATE=1 (fork isolation)"
                .to_string(),
        );
    }

    let mut reference =
        ReferenceBackend::new().map_err(|e| format!("reference backend unavailable: {e}"))?;
    let memory_fault_signal = detect_memory_fault_signal(&mut reference, &templates);
    let mut aero = aero::AeroBackend::new(memory_fault_signal);

    let mem_base = reference.memory_base();
    let mut rng = corpus::XorShift64::new(seed);
    let mut report = ConformanceReport::new_for_templates(cases, &templates);

    for (case_idx, template) in templates.iter().cycle().take(cases).enumerate() {
        let test_case = TestCase::generate(case_idx, template, &mut rng, mem_base);
        report.coverage.increment(template.coverage_key);

        let expected = reference.execute(&test_case);
        let actual = aero.execute(&test_case);

        if let Some(message) = compare_outcomes(mem_base, template, &test_case, &expected, &actual)
        {
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

fn templates_for_run() -> Result<Vec<InstructionTemplate>, String> {
    let templates = corpus::templates();
    let filter = std::env::var("AERO_CONFORMANCE_FILTER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

    let Some(filter) = filter else {
        return Ok(templates);
    };

    let terms = parse_filter_terms(&filter);
    if terms.is_empty() {
        return Ok(templates);
    }

    let coverage_keys = templates
        .iter()
        .map(|t| t.coverage_key.to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();

    let filtered: Vec<InstructionTemplate> = templates
        .into_iter()
        .filter(|t| template_matches_filter(t, &terms))
        .collect();

    if filtered.is_empty() {
        let keys = coverage_keys.into_iter().collect::<Vec<_>>().join("\n  - ");
        return Err(format!(
            "AERO_CONFORMANCE_FILTER={filter:?} matched 0 templates.\n\
known coverage_key values:\n  - {keys}\n\
hint: filter terms match substrings in template name or coverage_key.\n\
      use `key:<coverage_key>` to match a coverage_key exactly, or `name:<substring>` to match template names only."
        ));
    }

    Ok(filtered)
}

fn parse_filter_terms(filter: &str) -> Vec<String> {
    filter
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn template_matches_filter(template: &InstructionTemplate, terms: &[String]) -> bool {
    let name = template.name.to_ascii_lowercase();
    let coverage_key = template.coverage_key.to_ascii_lowercase();
    // Default behaviour: substring match against template name or coverage_key (case-insensitive).
    //
    // Use `key:<coverage_key>` or `name:<substring>` to disambiguate explicitly.
    terms.iter().any(|term| {
        let (mode, term) = if let Some(term) = term.strip_prefix("key:") {
            ("key", term)
        } else if let Some(term) = term.strip_prefix("coverage_key:") {
            ("key", term)
        } else if let Some(term) = term.strip_prefix("coverage:") {
            ("key", term)
        } else if let Some(term) = term.strip_prefix("name:") {
            ("name", term)
        } else {
            ("auto", term.as_str())
        };

        if term.is_empty() {
            return false;
        }

        match mode {
            "key" => coverage_key == term,
            "name" => name.contains(term),
            _ => name.contains(term) || coverage_key.contains(term),
        }
    })
}

fn detect_memory_fault_signal(
    reference: &mut ReferenceBackend,
    templates: &[InstructionTemplate],
) -> i32 {
    // Default to SIGSEGV; overridden if the host backend reports SIGBUS (or another signal) for
    // user-mode memory faults on this platform.
    let default = libc::SIGSEGV;
    let Some(template) = templates
        .iter()
        .find(|t| matches!(t.kind, corpus::TemplateKind::MovRaxM64Abs0))
    else {
        return default;
    };

    let mut rng = corpus::XorShift64::new(0x_3a6b_2d2c_1d58_f011);
    let case = TestCase::generate(0, template, &mut rng, reference.memory_base());
    match reference.execute(&case).fault {
        Some(Fault::Signal(sig)) => sig,
        _ => default,
    }
}

fn compare_outcomes(
    mem_base: u64,
    template: &InstructionTemplate,
    case: &TestCase,
    expected: &ExecOutcome,
    actual: &ExecOutcome,
) -> Option<String> {
    if expected.fault != actual.fault {
        return Some(report::format_failure(mem_base, template, case, expected, actual));
    }

    if expected.fault.is_some() {
        return None;
    }

    let expected_state = &expected.state;
    let actual_state = &actual.state;

    if !report::states_equal(expected_state, actual_state, template.flags_mask) {
        return Some(report::format_failure(mem_base, template, case, expected, actual));
    }

    if template.mem_compare_len > 0
        && !report::memory_equal(&expected.memory, &actual.memory, template.mem_compare_len)
    {
        return Some(report::format_failure(mem_base, template, case, expected, actual));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, val);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                std::env::set_var(self.key, prev);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn template_filter_reduces_set() {
        let all = crate::corpus::templates();
        let _guard = EnvGuard::set("AERO_CONFORMANCE_FILTER", "add");

        let filtered = templates_for_run().expect("filter should match at least one template");

        assert!(
            filtered.len() < all.len(),
            "expected filter to reduce template count (all={}, filtered={})",
            all.len(),
            filtered.len()
        );
        assert!(!filtered.is_empty());
        for template in filtered {
            let name = template.name.to_ascii_lowercase();
            let coverage_key = template.coverage_key.to_ascii_lowercase();
            assert!(
                name.contains("add") || coverage_key.contains("add"),
                "template unexpectedly matched filter: {:?} (coverage_key={})",
                template.name,
                template.coverage_key
            );
        }
    }

    #[test]
    fn template_filter_can_force_name_substring() {
        let _guard = EnvGuard::set("AERO_CONFORMANCE_FILTER", "name:add");
        let filtered = templates_for_run().expect("name:add should match at least one template");

        assert!(
            filtered.iter().all(|t| t.name.contains("add")),
            "expected all filtered templates to contain 'add' in name"
        );
        assert!(
            filtered
                .iter()
                .any(|t| t.coverage_key == "add_mem" || t.coverage_key == "add32"),
            "expected name-based filtering to include non-\"add\" coverage keys"
        );
    }

    #[test]
    fn fault_templates_match_reference() {
        if !cfg!(all(target_arch = "x86_64", unix)) {
            eprintln!("skipping fault conformance test on non-x86_64/unix host");
            return;
        }

        // Fault templates must always run with reference isolation enabled so signals don't take
        // down the test runner.
        let _isolate = EnvGuard::set("AERO_CONFORMANCE_REFERENCE_ISOLATE", "1");

        let templates = corpus::templates();
        let mut reference = ReferenceBackend::new().expect("reference backend unavailable");
        let mem_fault_signal = detect_memory_fault_signal(&mut reference, &templates);
        let mut aero = aero::AeroBackend::new(mem_fault_signal);

        let fault_templates: Vec<&InstructionTemplate> = templates
            .iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    corpus::TemplateKind::Ud2 | corpus::TemplateKind::MovRaxM64Abs0
                )
            })
            .collect();
        assert!(
            fault_templates.len() >= 2,
            "expected at least ud2 + memory fault templates"
        );

        let mut rng = corpus::XorShift64::new(0x_0bad_f00d_f00d_f00d);
        for (idx, template) in fault_templates.into_iter().enumerate() {
            let case = TestCase::generate(idx, template, &mut rng, reference.memory_base());
            let expected = reference.execute(&case);
            let actual = aero.execute(&case);

            assert!(
                expected.fault.is_some(),
                "reference backend did not fault for template {}",
                template.name
            );
            assert_eq!(
                expected.fault, actual.fault,
                "fault mismatch for template {}",
                template.name
            );
        }
    }

    #[test]
    fn seed_env_parses_hex_and_underscores() {
        assert_eq!(parse_seed_env("123"), Some(123));
        assert_eq!(parse_seed_env("1_000"), Some(1000));
        assert_eq!(parse_seed_env("0x10"), Some(16));
        assert_eq!(parse_seed_env("0X10"), Some(16));
        assert_eq!(parse_seed_env("0x_10"), Some(16));
        assert_eq!(parse_seed_env("0x_52c6_71d9"), Some(0x52c6_71d9));
    }

    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn tier0_single_case_matches_reference() {
        use crate::corpus::TemplateKind;

        let mut reference = ReferenceBackend::new().expect("reference backend unavailable");
        let mut aero = aero::AeroBackend::new(libc::SIGSEGV);

        let template = corpus::templates()
            .into_iter()
            .find(|t| matches!(t.kind, TemplateKind::MovM64Rax))
            .expect("template missing");

        let mut rng = corpus::XorShift64::new(0x_1d7a_3d0c_7f3c_2a11);
        let case = TestCase::generate(0, &template, &mut rng, reference.memory_base());

        let expected = reference.execute(&case);
        let actual = aero.execute(&case);

        assert_eq!(expected.fault, actual.fault, "fault mismatch");
        assert!(
            report::states_equal(&expected.state, &actual.state, template.flags_mask),
            "state mismatch:\n{}",
            report::format_failure(case.mem_base, &template, &case, &expected, &actual)
        );
        if template.mem_compare_len > 0 {
            assert!(
                report::memory_equal(&expected.memory, &actual.memory, template.mem_compare_len),
                "memory mismatch:\n{}",
                report::format_failure(case.mem_base, &template, &case, &expected, &actual)
            );
        }
    }

    #[test]
    fn cases_env_parses_underscores() {
        assert_eq!(parse_cases_env("512"), Some(512));
        assert_eq!(parse_cases_env("10_000"), Some(10000));
    }
}
