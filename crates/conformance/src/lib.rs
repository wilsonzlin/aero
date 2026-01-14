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
//!   `_` separators are allowed (e.g. `10_000`).
//! - `AERO_CONFORMANCE_SEED` (default: `0x52c6_71d9_a4f2_31b9`): RNG seed for deterministic runs.
//!   Supports decimal or `0x...` hex, and `_` separators.
//! - `AERO_CONFORMANCE_FILTER` (optional): template filter. Terms are split on commas, semicolons,
//!   and whitespace.
//!   - `key:<coverage_key>` / `coverage:<coverage_key>` / `coverage_key:<coverage_key>`: exact
//!     match on `coverage_key`
//!   - `name:<substring>`: substring match on template name (case-insensitive)
//!   - unprefixed: if it matches a known `coverage_key`, it selects that key exactly; otherwise it
//!     is treated as a case-insensitive substring match on template name.
//! - `AERO_CONFORMANCE_REFERENCE` (optional): select the reference backend.
//!   - `qemu`: use QEMU if the crate is built with the `qemu-reference` feature and a
//!     `qemu-system-*` binary is available.
//!   - any other value / unset: use the host reference backend (x86_64 + unix only).
//! - `AERO_CONFORMANCE_REFERENCE_ISOLATE` (default: `1`): run the host reference backend in a
//!   forked child process for isolation. Some templates intentionally fault and require isolation.
//! - `AERO_CONFORMANCE_REPORT_PATH` (optional): write a JSON conformance report to this path
//!   (on first failure and again at the end of the run).

mod aero;
mod corpus;
mod reference;
mod report;
mod signals;

pub use report::{ConformanceReport, CoverageSummary};

use crate::corpus::{InstructionTemplate, TestCase};
use crate::reference::ReferenceBackend;

pub(crate) const FLAG_CF: u64 = 1 << 0;
pub(crate) const FLAG_PF: u64 = 1 << 2;
pub(crate) const FLAG_AF: u64 = 1 << 4;
pub(crate) const FLAG_ZF: u64 = 1 << 6;
pub(crate) const FLAG_SF: u64 = 1 << 7;
pub(crate) const FLAG_DF: u64 = 1 << 10;
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
    let (radix, digits) = match cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
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
    let mut reference =
        ReferenceBackend::new().map_err(|e| format!("reference backend unavailable: {e}"))?;
    let mem_base = reference.memory_base();
    let isolate = std::env::var("AERO_CONFORMANCE_REFERENCE_ISOLATE")
        .map(|v| v != "0")
        .unwrap_or(true);
    let base_templates = {
        #[cfg(feature = "qemu-reference")]
        {
            if reference.is_qemu() {
                corpus::templates_qemu()
            } else {
                corpus::templates()
            }
        }
        #[cfg(not(feature = "qemu-reference"))]
        {
            corpus::templates()
        }
    };
    // Coverage "expected" must remain the full set even when a template filter is active.
    // Use the unfiltered template corpus for the selected reference backend, while only
    // incrementing counts for the executed templates.
    let mut report = ConformanceReport::new_for_templates(cases, &base_templates);

    // Determine which host signal to use for user-mode memory faults (SIGSEGV vs SIGBUS, etc).
    // This uses a known-faulting template and must only run when the host backend is isolated.
    let memory_fault_signal = if isolate {
        detect_memory_fault_signal(&mut reference, &base_templates)
    } else {
        signals::SIGSEGV
    };

    let templates = templates_for_run(base_templates)?;
    if !isolate && templates.iter().any(|t| t.kind.is_fault_template()) {
        // Fault templates intentionally crash in user mode on the host reference backend.
        // If isolation is disabled, fail fast with a clear message instead of taking down
        // the entire test runner process.
        return Err(
            "fault templates require AERO_CONFORMANCE_REFERENCE_ISOLATE=1 (fork isolation)"
                .to_string(),
        );
    }
    let mut aero = aero::AeroBackend::new(memory_fault_signal);

    let mut rng = corpus::XorShift64::new(seed);
    let reference_env = std::env::var("AERO_CONFORMANCE_REFERENCE")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let filter_env = std::env::var("AERO_CONFORMANCE_FILTER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());

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
            let isolate_setting = if isolate { 1 } else { 0 };
            let minimal_cases = case_idx + 1;

            let mut repro_lines = Vec::<String>::new();
            // Preserve reference backend selection in case the user is running with QEMU.
            // (This is ignored by the default host backend.)
            if let Some(v) = reference_env.as_deref() {
                repro_lines.push(format!(
                    "AERO_CONFORMANCE_REFERENCE={}",
                    shell_quote_single(v)
                ));
            }
            repro_lines.push(format!(
                "AERO_CONFORMANCE_REFERENCE_ISOLATE={isolate_setting}"
            ));
            repro_lines.push(format!("AERO_CONFORMANCE_CASES={minimal_cases}"));
            repro_lines.push(format!("AERO_CONFORMANCE_SEED={seed:#x}"));
            if let Some(filter) = filter_env.as_deref() {
                repro_lines.push(format!(
                    "AERO_CONFORMANCE_FILTER={}",
                    shell_quote_single(filter)
                ));
            }
            repro_lines
                .push("cargo test -p conformance --locked instruction_conformance".to_string());

            let repro = format!("repro:\n  {}", repro_lines.join(" \\\n  "));
            let hint = format!(
                "\nhint: to narrow the template set, try `AERO_CONFORMANCE_FILTER=key:{}` or `AERO_CONFORMANCE_FILTER=name:<substring>`\n\
(note: changing the filter changes the template order and may require increasing AERO_CONFORMANCE_CASES).",
                template.coverage_key
            );
            return Err(format!("{repro}{hint}\n\n{message}"));
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

fn shell_quote_single(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // In POSIX shells, single quotes can't be escaped inside single quotes, so we close the quote,
    // insert an escaped quote, and reopen:  'foo'\''bar'
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

fn templates_for_run(
    templates: Vec<InstructionTemplate>,
) -> Result<Vec<InstructionTemplate>, String> {
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
        .filter(|t| template_matches_filter(t, &terms, &coverage_keys))
        .collect();

    if filtered.is_empty() {
        let keys = coverage_keys.into_iter().collect::<Vec<_>>().join("\n  - ");
        return Err(format!(
            "AERO_CONFORMANCE_FILTER={filter:?} matched 0 templates.\n\
known coverage_key values:\n  - {keys}\n\
hint: if a term matches a `coverage_key`, it selects that `coverage_key` exactly.\n\
      otherwise, it matches substrings in the template name.\n\
      use `key:<coverage_key>` or `name:<substring>` to disambiguate explicitly."
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

fn template_matches_filter(
    template: &InstructionTemplate,
    terms: &[String],
    coverage_keys: &std::collections::BTreeSet<String>,
) -> bool {
    let name = template.name.to_ascii_lowercase();
    let coverage_key = template.coverage_key.to_ascii_lowercase();
    // Default behaviour:
    // - if a term matches a known `coverage_key`, select that `coverage_key` exactly
    // - otherwise match substrings in the template name (case-insensitive)
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
            _ => {
                if coverage_keys.contains(term) {
                    coverage_key == term
                } else {
                    name.contains(term)
                }
            }
        }
    })
}

fn detect_memory_fault_signal(
    reference: &mut ReferenceBackend,
    templates: &[InstructionTemplate],
) -> i32 {
    // Default to SIGSEGV; overridden if the host backend reports SIGBUS (or another signal) for
    // user-mode memory faults on this platform.
    let default = signals::SIGSEGV;
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
        return Some(report::format_failure(
            mem_base, template, case, expected, actual,
        ));
    }

    if expected.fault.is_some() {
        return None;
    }

    let expected_state = &expected.state;
    let actual_state = &actual.state;

    if !report::states_equal(expected_state, actual_state, template.flags_mask) {
        return Some(report::format_failure(
            mem_base, template, case, expected, actual,
        ));
    }

    if template.mem_compare_len > 0
        && !report::memory_equal(&expected.memory, &actual.memory, template.mem_compare_len)
    {
        return Some(report::format_failure(
            mem_base, template, case, expected, actual,
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let _lock = ENV_LOCK.lock().unwrap();
        let all = crate::corpus::templates();
        let _guard = EnvGuard::set("AERO_CONFORMANCE_FILTER", "add");

        let filtered =
            templates_for_run(all.clone()).expect("filter should match at least one template");

        assert!(
            filtered.len() < all.len(),
            "expected filter to reduce template count (all={}, filtered={})",
            all.len(),
            filtered.len()
        );
        assert!(!filtered.is_empty());
        for template in filtered {
            assert!(
                template.coverage_key.eq_ignore_ascii_case("add"),
                "template unexpectedly matched filter: {:?} (coverage_key={})",
                template.name,
                template.coverage_key
            );
        }
    }

    #[test]
    fn template_filter_can_force_name_substring() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set("AERO_CONFORMANCE_FILTER", "name:add");
        let filtered = templates_for_run(crate::corpus::templates())
            .expect("name:add should match at least one template");

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
    fn coverage_expected_is_full_set_even_when_filtered() {
        if !cfg!(all(target_arch = "x86_64", unix)) {
            // The conformance runner requires the host reference backend.
            return;
        }

        let _lock = ENV_LOCK.lock().unwrap();
        let all_expected = ConformanceReport::new(1).coverage.expected;

        let _isolate = EnvGuard::set("AERO_CONFORMANCE_REFERENCE_ISOLATE", "1");
        let _filter = EnvGuard::set("AERO_CONFORMANCE_FILTER", "add");
        let report = run(1, 0x1234_5678_9abc_def0, None).expect("run should succeed");

        assert_eq!(
            report.coverage.expected, all_expected,
            "expected coverage.expected to always be the full set even when filtering is enabled"
        );

        // Counts should only include executed templates.
        assert!(
            report
                .coverage
                .counts
                .keys()
                .all(|k| report.coverage.expected.contains(k)),
            "unexpected coverage key in counts"
        );
    }

    #[test]
    fn fault_templates_match_reference() {
        if !cfg!(all(target_arch = "x86_64", unix)) {
            eprintln!("skipping fault conformance test on non-x86_64/unix host");
            return;
        }

        let _lock = ENV_LOCK.lock().unwrap();
        // Fault templates must always run with reference isolation enabled so signals don't take
        // down the test runner.
        let _isolate = EnvGuard::set("AERO_CONFORMANCE_REFERENCE_ISOLATE", "1");

        let templates = corpus::templates();
        let mut reference = ReferenceBackend::new().expect("reference backend unavailable");
        let mem_fault_signal = detect_memory_fault_signal(&mut reference, &templates);
        let mut aero = aero::AeroBackend::new(mem_fault_signal);

        let fault_templates: Vec<&InstructionTemplate> = templates
            .iter()
            .filter(|t| t.kind.is_fault_template())
            .collect();

        let mut saw_ud2 = false;
        let mut saw_mem_abs0 = false;
        let mut saw_oob_load = false;
        let mut saw_oob_store = false;
        let mut saw_div0 = false;

        let mut rng = corpus::XorShift64::new(0x_0bad_f00d_f00d_f00d);
        for (idx, template) in fault_templates.into_iter().enumerate() {
            match template.kind {
                corpus::TemplateKind::Ud2 => saw_ud2 = true,
                corpus::TemplateKind::MovRaxM64Abs0 => saw_mem_abs0 = true,
                corpus::TemplateKind::GuardedOobLoad => saw_oob_load = true,
                corpus::TemplateKind::GuardedOobStore => saw_oob_store = true,
                corpus::TemplateKind::DivRbxByZero => saw_div0 = true,
                _ => {}
            }

            let expected_fault = match template.kind {
                corpus::TemplateKind::Ud2 => Fault::Signal(signals::SIGILL),
                corpus::TemplateKind::DivRbxByZero => Fault::Signal(signals::SIGFPE),
                corpus::TemplateKind::MovRaxM64Abs0
                | corpus::TemplateKind::GuardedOobLoad
                | corpus::TemplateKind::GuardedOobStore => Fault::Signal(mem_fault_signal),
                _ => unreachable!("filtered by is_fault_template"),
            };

            let case = TestCase::generate(idx, template, &mut rng, reference.memory_base());
            let expected = reference.execute(&case);
            let actual = aero.execute(&case);

            assert!(
                expected.fault.is_some(),
                "reference backend did not fault for template {}",
                template.name
            );
            assert_eq!(
                expected.fault,
                Some(expected_fault),
                "reference did not terminate with expected signal for template {}",
                template.name
            );
            assert_eq!(
                expected.fault, actual.fault,
                "fault mismatch for template {}",
                template.name
            );
        }

        assert!(saw_ud2, "missing ud2 template");
        assert!(saw_mem_abs0, "missing [0] memory-fault template");
        assert!(saw_oob_load, "missing guarded OOB load template");
        assert!(saw_oob_store, "missing guarded OOB store template");
        assert!(saw_div0, "missing div-by-zero template");
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

        let _lock = ENV_LOCK.lock().unwrap();
        let mut reference = ReferenceBackend::new().expect("reference backend unavailable");
        let mut aero = aero::AeroBackend::new(signals::SIGSEGV);

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
