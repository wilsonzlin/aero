use crate::corpus::{InstructionTemplate, TestCase};
use crate::{CpuState, ExecOutcome, FLAG_AF, FLAG_CF, FLAG_DF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
use iced_x86::{Formatter, OpKind, Register};
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
        Self::new_for_templates(total_cases, &crate::corpus::templates())
    }

    pub fn new_for_templates(total_cases: usize, templates: &[InstructionTemplate]) -> Self {
        Self::new_with_expected(total_cases, expected_coverage_keys_for_templates(templates))
    }

    pub fn new_with_expected(total_cases: usize, expected: Vec<String>) -> Self {
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

fn expected_coverage_keys_for_templates(templates: &[InstructionTemplate]) -> Vec<String> {
    let mut keys = BTreeSet::new();
    for template in templates {
        keys.insert(template.coverage_key.to_string());
    }
    keys.into_iter().collect()
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
    if expected.len() < len || actual.len() < len {
        return false;
    }
    expected[..len] == actual[..len]
}

pub fn format_failure(
    mem_base: u64,
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
    let failure_kind = failure_kind(template, expected, actual);
    let decoded_instruction = decode_instruction_raw(bytes, effective_rip);

    let _ = writeln!(
        &mut out,
        "conformance mismatch (case {}): {} (coverage_key={})",
        case.case_idx, template.name, template.coverage_key
    );
    let _ = writeln!(&mut out, "kind: {failure_kind}");
    let _ = writeln!(&mut out, "template_kind: {:?}", template.kind);
    let _ = writeln!(&mut out, "bytes: {byte_hex}");
    let _ = writeln!(&mut out, "effective_rip: {effective_rip:#x}");
    if template.mem_compare_len > 0 {
        let _ = writeln!(
            &mut out,
            "mem_base: {mem_base:#x} (compare_len={} bytes)",
            template.mem_compare_len
        );
    } else {
        let _ = writeln!(&mut out, "mem_base: {mem_base:#x}");
    }
    let _ = writeln!(&mut out, "flags_mask: {:#x}", template.flags_mask);
    let mem_end = mem_base.wrapping_add(case.memory.len() as u64);
    let _ = writeln!(
        &mut out,
        "memory_range: [{mem_base:#x}..{mem_end:#x}) (len={} bytes)",
        case.memory.len()
    );
    if let Some(code_off) = effective_rip.checked_sub(mem_base) {
        if let Ok(code_off) = usize::try_from(code_off) {
            let _ = writeln!(&mut out, "rip_offset: 0x{code_off:x}");
            if let Some(bytes_at_rip) = case.memory.get(code_off..code_off + bytes.len()) {
                let bytes_at_rip = bytes_at_rip
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let _ = writeln!(&mut out, "bytes@rip: {bytes_at_rip}");
            }
        }
    }
    if let Some(rdi_off) = case.init.rdi.checked_sub(mem_base) {
        if let Ok(rdi_off) = usize::try_from(rdi_off) {
            let in_range = rdi_off < case.memory.len();
            let _ = writeln!(
                &mut out,
                "rdi_offset: 0x{rdi_off:x}{}",
                if in_range { "" } else { " (out of range)" }
            );
        }
    }
    if let Some(decoded) = decoded {
        let _ = writeln!(&mut out, "iced-x86: {decoded}");
    }
    if let Some(aero) = aero_decoded {
        let _ = writeln!(&mut out, "aero-decoder: {aero}");
    }
    if let Some(instr) = decoded_instruction.as_ref() {
        format_memory_operand_info(&mut out, instr, &case.init, mem_base, case.memory.len());
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

    let _ = writeln!(
        &mut out,
        "FAIL kind={} case={} template_kind={:?} coverage_key={} name=\"{}\"",
        failure_kind, case.case_idx, template.kind, template.coverage_key, template.name
    );
    out
}

fn failure_kind(
    template: &InstructionTemplate,
    expected: &ExecOutcome,
    actual: &ExecOutcome,
) -> &'static str {
    if expected.fault != actual.fault {
        return "fault";
    }
    if expected.fault.is_some() {
        // Should be unreachable: callers only invoke `format_failure` on mismatches.
        return "fault";
    }
    if !states_equal(&expected.state, &actual.state, template.flags_mask) {
        return "state";
    }
    if template.mem_compare_len > 0
        && !memory_equal(&expected.memory, &actual.memory, template.mem_compare_len)
    {
        return "memory";
    }
    "unknown"
}

fn decode_instruction_raw(bytes: &[u8], ip: u64) -> Option<iced_x86::Instruction> {
    let mut decoder = iced_x86::Decoder::with_ip(64, bytes, ip, iced_x86::DecoderOptions::NONE);
    let instruction = decoder.decode();
    if instruction.is_invalid() {
        None
    } else {
        Some(instruction)
    }
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

fn format_memory_operand_info(
    out: &mut String,
    instruction: &iced_x86::Instruction,
    init: &CpuState,
    mem_base: u64,
    mem_len: usize,
) {
    let has_mem = (0..instruction.op_count()).any(|i| instruction.op_kind(i) == OpKind::Memory);
    if !has_mem {
        return;
    }

    let base_reg = instruction.memory_base();
    let index_reg = instruction.memory_index();
    let scale = instruction.memory_index_scale();
    let disp = instruction.memory_displacement64();
    let size = instruction.memory_size().size().max(1) as u64;

    let ip_rel = instruction.is_ip_rel_memory_operand();

    let (eff, base_val, index_val) = if ip_rel {
        // Iced computes RIP-relative addresses from the instruction metadata (IP + disp).
        (instruction.ip_rel_memory_address(), None, None)
    } else {
        let base_val = if base_reg == Register::None {
            Some(0)
        } else {
            read_register_for_addr(init, base_reg)
        };
        let index_val = if index_reg == Register::None {
            Some(0)
        } else {
            read_register_for_addr(init, index_reg)
        };

        let Some(base) = base_val else {
            let _ = writeln!(
                out,
                "mem_operand: <unknown> (base reg {base_reg:?} not captured in CpuState)"
            );
            return;
        };
        let Some(index) = index_val else {
            let _ = writeln!(
                out,
                "mem_operand: <unknown> (index reg {index_reg:?} not captured in CpuState)"
            );
            return;
        };

        let eff = base
            .wrapping_add(index.wrapping_mul(scale as u64))
            .wrapping_add(disp);
        (eff, base_val, index_val)
    };

    if ip_rel {
        let next_ip = instruction.next_ip();
        let _ = writeln!(
            out,
            "mem_operand: size={size} ip_rel=1 next_ip={next_ip:#x} disp={disp:#x}"
        );
    } else {
        let base_val = base_val.expect("base_val checked above");
        let index_val = index_val.expect("index_val checked above");
        let _ = writeln!(
            out,
            "mem_operand: size={size} base={base_reg:?}({base_val:#x}) index={index_reg:?}({index_val:#x}) scale={scale} disp={disp:#x}"
        );
    }

    let mem_end = mem_base.wrapping_add(mem_len as u64);
    let in_range = eff >= mem_base && eff.checked_add(size).is_some_and(|end| end <= mem_end);

    let diff = eff as i128 - mem_base as i128;
    let off = if diff >= 0 {
        format!("+0x{:x}", diff as u128)
    } else {
        format!("-0x{:x}", (-diff) as u128)
    };

    let _ = writeln!(
        out,
        "mem_effective: {eff:#x} (offset={off}){}",
        if in_range { "" } else { " (out of range)" }
    );
}

fn read_register_for_addr(state: &CpuState, reg: Register) -> Option<u64> {
    use Register::*;

    let (full, part) = match reg {
        RAX | EAX | AX | AL | AH => (state.rax, reg),
        RBX | EBX | BX | BL | BH => (state.rbx, reg),
        RCX | ECX | CX | CL | CH => (state.rcx, reg),
        RDX | EDX | DX | DL | DH => (state.rdx, reg),
        RSI | ESI | SI | SIL => (state.rsi, reg),
        RDI | EDI | DI | DIL => (state.rdi, reg),
        R8 | R8D | R8W | R8L => (state.r8, reg),
        R9 | R9D | R9W | R9L => (state.r9, reg),
        R10 | R10D | R10W | R10L => (state.r10, reg),
        R11 | R11D | R11W | R11L => (state.r11, reg),
        R12 | R12D | R12W | R12L => (state.r12, reg),
        R13 | R13D | R13W | R13L => (state.r13, reg),
        R14 | R14D | R14W | R14L => (state.r14, reg),
        R15 | R15D | R15W | R15L => (state.r15, reg),
        RIP | EIP => (state.rip, reg),
        None => return std::option::Option::None,
        _ => return std::option::Option::None,
    };

    Some(match part {
        EAX | EBX | ECX | EDX | ESI | EDI | EIP | R8D | R9D | R10D | R11D | R12D | R13D | R14D
        | R15D => full & 0xFFFF_FFFF,
        AX | BX | CX | DX | SI | DI | R8W | R9W | R10W | R11W | R12W | R13W | R14W | R15W => {
            full & 0xFFFF
        }
        AL | BL | CL | DL | SIL | DIL | R8L | R9L | R10L | R11L | R12L | R13L | R14L | R15L => {
            full & 0xFF
        }
        AH | BH | CH | DH => (full >> 8) & 0xFF,
        _ => full,
    })
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
        ("DF", FLAG_DF),
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
        ("DF", FLAG_DF),
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
    let exp_len = expected.len().min(len);
    let act_len = actual.len().min(len);
    let compare_len = exp_len.min(act_len);
    let expected = &expected[..exp_len];
    let actual = &actual[..act_len];

    let mut diffs = Vec::new();
    for (idx, (&exp, &act)) in expected[..compare_len]
        .iter()
        .zip(actual[..compare_len].iter())
        .enumerate()
    {
        if exp != act {
            diffs.push((idx, exp, act));
        }
    }

    let len_mismatch = exp_len != act_len;
    if diffs.is_empty() && !len_mismatch {
        return;
    }

    let len_note = if len_mismatch {
        format!(" (len mismatch: expected={} actual={})", exp_len, act_len)
    } else {
        String::new()
    };

    let _ = writeln!(
        out,
        "memory diff (compared {compare_len} bytes; showing first 16 mismatches):{len_note}"
    );
    for (idx, exp, act) in diffs.iter().copied().take(16) {
        let addr = base.wrapping_add(idx as u64);
        let _ = writeln!(
            out,
            "  +0x{idx:04x} ({addr:#018x}): expected={exp:02x} actual={act:02x}"
        );
    }

    if len_mismatch {
        let idx = compare_len;
        let addr = base.wrapping_add(idx as u64);
        let exp = expected.get(idx).copied();
        let act = actual.get(idx).copied();
        let _ = writeln!(
            out,
            "  +0x{idx:04x} ({addr:#018x}): expected={} actual={}",
            exp.map(|b| format!("{b:02x}"))
                .unwrap_or_else(|| "<eof>".to_string()),
            act.map(|b| format!("{b:02x}"))
                .unwrap_or_else(|| "<eof>".to_string()),
        );
    }

    let first_diff_idx = diffs
        .first()
        .map(|(idx, _, _)| *idx)
        .unwrap_or(compare_len);
    format_memory_window(out, expected, actual, base, len, first_diff_idx);
}

fn format_memory_window(
    out: &mut String,
    expected: &[u8],
    actual: &[u8],
    base: u64,
    len: usize,
    first_diff_idx: usize,
) {
    let exp = expected.get(first_diff_idx).copied();
    let act = actual.get(first_diff_idx).copied();
    let addr = base.wrapping_add(first_diff_idx as u64);
    let _ = writeln!(
        out,
        "memory window around first mismatch at +0x{first_diff_idx:04x} ({addr:#018x}) (expected={} actual={}):",
        exp.map(|b| format!("{b:02x}"))
            .unwrap_or_else(|| "<eof>".to_string()),
        act.map(|b| format!("{b:02x}"))
            .unwrap_or_else(|| "<eof>".to_string()),
    );

    // Show up to 32 bytes (two 16-byte lines) around the first mismatch.
    let window = 32usize;
    let row = 16usize;

    let mut start = first_diff_idx.saturating_sub(window / 2);
    start -= start % row;
    let end = (start + window).min(len);

    for row_start in (start..end).step_by(row) {
        let addr = base.wrapping_add(row_start as u64);
        let _ = write!(out, "  +0x{row_start:04x} ({addr:#018x}) exp:");
        for i in row_start..(row_start + row) {
            if i >= end {
                let _ = write!(out, "   ");
                continue;
            }
            match expected.get(i).copied() {
                Some(b) => {
                    let _ = write!(out, " {b:02x}");
                }
                None => {
                    let _ = write!(out, " --");
                }
            }
        }
        let _ = write!(out, " | act:");
        for i in row_start..(row_start + row) {
            if i >= end {
                let _ = write!(out, "   ");
                continue;
            }
            match actual.get(i).copied() {
                Some(b) => {
                    let _ = write!(out, " {b:02x}");
                }
                None => {
                    let _ = write!(out, " --");
                }
            }
        }
        let _ = writeln!(out);
    }
}
