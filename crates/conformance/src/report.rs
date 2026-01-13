use crate::corpus::{InstructionTemplate, TestCase};
use crate::{CpuState, ExecOutcome, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
use iced_x86::Formatter;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceReport {
    pub total_cases: usize,
    pub failures: usize,
    pub coverage: CoverageSummary,
}

impl ConformanceReport {
    pub fn new(total_cases: usize) -> Self {
        let expected: Vec<String> = {
            let mut keys = BTreeSet::new();
            for template in crate::corpus::templates() {
                keys.insert(template.coverage_key.to_string());
            }
            keys.into_iter().collect()
        };

        Self {
            total_cases,
            failures: 0,
            coverage: CoverageSummary::new(expected),
        }
    }

    pub fn print_summary(&self) {
        eprintln!(
            "conformance: {} cases, {} failures",
            self.total_cases, self.failures
        );

        let pct = self.coverage.percent();
        eprintln!(
            "coverage: {:.1}% ({} / {})",
            pct,
            self.coverage.covered(),
            self.coverage.expected.len()
        );

        let uncovered = self.coverage.uncovered();
        if !uncovered.is_empty() {
            eprintln!("uncovered:");
            for key in uncovered {
                eprintln!("  - {key}");
            }
        }
    }

    pub fn write_json(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(path, contents)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageSummary {
    pub expected: Vec<String>,
    pub counts: BTreeMap<String, usize>,
}

impl CoverageSummary {
    pub fn new(expected: Vec<String>) -> Self {
        Self {
            expected,
            counts: BTreeMap::new(),
        }
    }

    pub fn increment(&mut self, key: &str) {
        *self.counts.entry(key.to_string()).or_insert(0) += 1;
    }

    pub fn covered(&self) -> usize {
        self.expected
            .iter()
            .filter(|key| self.counts.get(*key).copied().unwrap_or(0) > 0)
            .count()
    }

    pub fn percent(&self) -> f64 {
        if self.expected.is_empty() {
            return 100.0;
        }
        (self.covered() as f64) * 100.0 / (self.expected.len() as f64)
    }

    pub fn uncovered(&self) -> Vec<String> {
        self.expected
            .iter()
            .filter(|key| self.counts.get(*key).copied().unwrap_or(0) == 0)
            .cloned()
            .collect()
    }
}

pub fn states_equal(expected: &CpuState, actual: &CpuState, flags_mask: u64) -> bool {
    expected.rax == actual.rax
        && expected.rbx == actual.rbx
        && expected.rcx == actual.rcx
        && expected.rdx == actual.rdx
        && expected.rsi == actual.rsi
        && expected.rdi == actual.rdi
        && expected.r8 == actual.r8
        && expected.r9 == actual.r9
        && expected.r10 == actual.r10
        && expected.r11 == actual.r11
        && expected.r12 == actual.r12
        && expected.r13 == actual.r13
        && expected.r14 == actual.r14
        && expected.r15 == actual.r15
        && expected.rip == actual.rip
        && ((expected.rflags ^ actual.rflags) & flags_mask) == 0
}

pub fn memory_equal(expected: &[u8], actual: &[u8], len: usize) -> bool {
    expected.get(..len) == actual.get(..len)
}

pub fn format_failure(
    template: &InstructionTemplate,
    case: &TestCase,
    expected: &ExecOutcome,
    actual: &ExecOutcome,
) -> String {
    let mut out = String::new();
    let bytes = template.bytes;
    let byte_hex = bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let effective_rip = case.init.rip;
    let decoded = decode_instruction_iced(bytes, effective_rip);
    let aero_decoded = decode_instruction_aero(bytes, effective_rip);

    let _ = writeln!(
        &mut out,
        "conformance mismatch (case {}): {} (coverage_key={})",
        case.case_idx, template.name, template.coverage_key
    );
    let _ = writeln!(&mut out, "bytes: {byte_hex}");
    let _ = writeln!(&mut out, "effective_rip: {effective_rip:#x}");
    if template.mem_compare_len > 0 {
        let mem_base = case.init.rdi;
        let _ = writeln!(
            &mut out,
            "mem_base: {mem_base:#x} (compare_len={} bytes)",
            template.mem_compare_len
        );
    }
    if let Some(decoded) = decoded {
        let _ = writeln!(&mut out, "iced-x86: {decoded}");
    }
    if let Some(aero) = aero_decoded {
        let _ = writeln!(&mut out, "aero-decoder: {aero}");
    }

    if expected.fault != actual.fault {
        let _ = writeln!(
            &mut out,
            "fault: expected={:?} actual={:?}",
            expected.fault, actual.fault
        );
    }

    let _ = writeln!(&mut out, "initial state:");
    format_state(&mut out, &case.init, template.flags_mask);

    let _ = writeln!(&mut out, "expected state:");
    format_state(&mut out, &expected.state, template.flags_mask);

    let _ = writeln!(&mut out, "actual state:");
    format_state(&mut out, &actual.state, template.flags_mask);

    let _ = writeln!(&mut out, "diff:");
    format_state_diff(
        &mut out,
        &expected.state,
        &actual.state,
        template.flags_mask,
    );
    if template.mem_compare_len > 0 {
        let mem_base = case.init.rdi;
        format_memory_dump(
            &mut out,
            "initial",
            &case.memory,
            mem_base,
            template.mem_compare_len,
        );
        format_memory_dump(
            &mut out,
            "expected",
            &expected.memory,
            mem_base,
            template.mem_compare_len,
        );
        format_memory_dump(
            &mut out,
            "actual",
            &actual.memory,
            mem_base,
            template.mem_compare_len,
        );
        format_memory_diff(
            &mut out,
            &expected.memory,
            &actual.memory,
            mem_base,
            template.mem_compare_len,
        );
    }

    out
}

fn decode_instruction_iced(bytes: &[u8], ip: u64) -> Option<String> {
    let mut decoder = iced_x86::Decoder::with_ip(64, bytes, ip, iced_x86::DecoderOptions::NONE);
    let instruction = decoder.decode();
    if instruction.is_invalid() {
        return None;
    }

    let mut formatter = iced_x86::IntelFormatter::new();
    let mut decoded = String::new();
    formatter.format(&instruction, &mut decoded);
    Some(format!(
        "{decoded} (len={} code={:?} mnemonic={:?})",
        instruction.len(),
        instruction.code(),
        instruction.mnemonic()
    ))
}

fn decode_instruction_aero(bytes: &[u8], ip: u64) -> Option<String> {
    // Optional: this is best-effort and intentionally avoids pulling in the full CPU core.
    let decoded = aero_cpu_decoder::decode_one(aero_cpu_decoder::DecodeMode::Bits64, ip, bytes)
        .ok()?;

    let mut formatter = iced_x86::IntelFormatter::new();
    let mut disasm = String::new();
    formatter.format(&decoded.instruction, &mut disasm);

    let prefixes = format_aero_prefixes(&decoded.prefixes);
    Some(format!(
        "{disasm} (len={} code={:?} mnemonic={:?}{prefixes})",
        decoded.instruction.len(),
        decoded.instruction.code(),
        decoded.instruction.mnemonic(),
    ))
}

fn format_aero_prefixes(prefixes: &aero_cpu_decoder::Prefixes) -> String {
    let mut parts = Vec::new();
    if prefixes.lock {
        parts.push("lock");
    }
    if prefixes.rep {
        parts.push("rep");
    }
    if prefixes.repne {
        parts.push("repne");
    }
    if let Some(seg) = prefixes.segment {
        parts.push(match seg {
            aero_cpu_decoder::Segment::Es => "es",
            aero_cpu_decoder::Segment::Cs => "cs",
            aero_cpu_decoder::Segment::Ss => "ss",
            aero_cpu_decoder::Segment::Ds => "ds",
            aero_cpu_decoder::Segment::Fs => "fs",
            aero_cpu_decoder::Segment::Gs => "gs",
        });
    }
    if prefixes.operand_size_override {
        parts.push("66");
    }
    if prefixes.address_size_override {
        parts.push("67");
    }
    if let Some(rex) = prefixes.rex {
        parts.push(if rex.w() { "rex.w" } else { "rex" });
    }
    if prefixes.vex.is_some() {
        parts.push("vex");
    }
    if prefixes.evex.is_some() {
        parts.push("evex");
    }
    if prefixes.xop.is_some() {
        parts.push("xop");
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!(" prefixes=[{}]", parts.join(" "))
    }
}

fn format_state(out: &mut String, state: &CpuState, flags_mask: u64) {
    let _ = writeln!(out, "  rax={:#018x} rbx={:#018x}", state.rax, state.rbx);
    let _ = writeln!(out, "  rcx={:#018x} rdx={:#018x}", state.rcx, state.rdx);
    let _ = writeln!(out, "  rsi={:#018x} rdi={:#018x}", state.rsi, state.rdi);
    let _ = writeln!(out, "  r8 ={:#018x} r9 ={:#018x}", state.r8, state.r9);
    let _ = writeln!(out, "  r10={:#018x} r11={:#018x}", state.r10, state.r11);
    let _ = writeln!(out, "  r12={:#018x} r13={:#018x}", state.r12, state.r13);
    let _ = writeln!(out, "  r14={:#018x} r15={:#018x}", state.r14, state.r15);
    let _ = writeln!(out, "  rip={:#x}", state.rip);
    let _ = writeln!(
        out,
        "  rflags={:#x} ({})",
        state.rflags,
        format_flags(state.rflags, flags_mask)
    );
}

fn format_state_diff(out: &mut String, expected: &CpuState, actual: &CpuState, flags_mask: u64) {
    diff_u64(out, "rax", expected.rax, actual.rax);
    diff_u64(out, "rbx", expected.rbx, actual.rbx);
    diff_u64(out, "rcx", expected.rcx, actual.rcx);
    diff_u64(out, "rdx", expected.rdx, actual.rdx);
    diff_u64(out, "rsi", expected.rsi, actual.rsi);
    diff_u64(out, "rdi", expected.rdi, actual.rdi);
    diff_u64(out, "r8", expected.r8, actual.r8);
    diff_u64(out, "r9", expected.r9, actual.r9);
    diff_u64(out, "r10", expected.r10, actual.r10);
    diff_u64(out, "r11", expected.r11, actual.r11);
    diff_u64(out, "r12", expected.r12, actual.r12);
    diff_u64(out, "r13", expected.r13, actual.r13);
    diff_u64(out, "r14", expected.r14, actual.r14);
    diff_u64(out, "r15", expected.r15, actual.r15);
    diff_u64(out, "rip", expected.rip, actual.rip);

    for (name, bit) in [
        ("CF", FLAG_CF),
        ("PF", FLAG_PF),
        ("AF", FLAG_AF),
        ("ZF", FLAG_ZF),
        ("SF", FLAG_SF),
        ("OF", FLAG_OF),
    ] {
        if (flags_mask & bit) == 0 {
            continue;
        }
        let exp = (expected.rflags & bit) != 0;
        let act = (actual.rflags & bit) != 0;
        if exp != act {
            let _ = writeln!(out, "  {name}: expected={exp} actual={act}");
        }
    }
}

fn diff_u64(out: &mut String, name: &str, expected: u64, actual: u64) {
    if expected != actual {
        let _ = writeln!(
            out,
            "  {name}: expected={:#018x} actual={:#018x}",
            expected, actual
        );
    }
}

fn format_flags(rflags: u64, mask: u64) -> String {
    let mut parts = Vec::new();
    for (name, bit) in [
        ("CF", FLAG_CF),
        ("PF", FLAG_PF),
        ("AF", FLAG_AF),
        ("ZF", FLAG_ZF),
        ("SF", FLAG_SF),
        ("OF", FLAG_OF),
    ] {
        if (mask & bit) == 0 {
            continue;
        }
        parts.push(format!(
            "{name}={}",
            if (rflags & bit) != 0 { 1 } else { 0 }
        ));
    }
    parts.join(" ")
}

fn format_memory_dump(out: &mut String, label: &str, memory: &[u8], base: u64, len: usize) {
    let len = len.min(64);
    let memory = memory.get(..len).unwrap_or(memory);

    let _ = writeln!(out, "{label} memory dump (first {} bytes):", memory.len());
    for (line_idx, chunk) in memory.chunks(16).enumerate() {
        let offset = line_idx * 16;
        let addr = base.wrapping_add(offset as u64);
        let hex = chunk
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii = chunk
            .iter()
            .map(|b| if b.is_ascii_graphic() { *b as char } else { '.' })
            .collect::<String>();
        let _ = writeln!(out, "  {addr:#018x}: {hex:<47} |{ascii}|");
    }
}

fn format_memory_diff(out: &mut String, expected: &[u8], actual: &[u8], base: u64, len: usize) {
    let expected = expected.get(..len).unwrap_or(expected);
    let actual = actual.get(..len).unwrap_or(actual);

    let mut diffs = Vec::new();
    for (idx, (&exp, &act)) in expected.iter().zip(actual.iter()).enumerate() {
        if exp != act {
            diffs.push((idx, exp, act));
        }
    }

    if diffs.is_empty() {
        return;
    }

    let _ = writeln!(out, "memory diff (first {} bytes):", len.min(64));
    for (idx, exp, act) in diffs.into_iter().take(16) {
        let addr = base.wrapping_add(idx as u64);
        let _ = writeln!(
            out,
            "  +0x{idx:02x} ({addr:#018x}): expected={exp:02x} actual={act:02x}"
        );
    }
}
