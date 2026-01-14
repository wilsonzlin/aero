# Workstream A: CPU & JIT

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns the **CPU emulation engine**: the x86/x86-64 instruction decoder, interpreter (Tier 0), JIT compiler (Tier 1/2), and memory management unit (paging, TLB).

This is the **critical path** for performance. The emulator's speed is dominated by how fast the CPU can execute guest instructions.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-cpu-core/` | CPU state, decoder, interpreter, exception handling |
| `crates/aero-cpu-decoder/` | Instruction decoding (separate from core for modularity) |
| `crates/aero-jit-x86/` | JIT compiler: x86 → IR → WASM |
| `crates/aero-mmu/` | Memory management unit, paging |
| `crates/aero-mem/` | Physical memory, bus routing |
| `crates/aero-x86/` | x86 architecture constants, helpers |

---

## Essential Documentation

**Must read:**

- [`docs/02-cpu-emulation.md`](../docs/02-cpu-emulation.md) — CPU emulation design, tiered execution
- [`docs/03-memory-management.md`](../docs/03-memory-management.md) — Paging, TLB, address translation
- [`docs/10-performance-optimization.md`](../docs/10-performance-optimization.md) — JIT optimization strategies

**Reference:**

- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture
- [`docs/16-guest-cpu-benchmark-suite.md`](../docs/16-guest-cpu-benchmark-suite.md) — CPU benchmarks (PF-008)

---

## Interface Contract: CpuBus

The CPU communicates with memory and devices through `CpuBus`. This is the canonical interface:

```rust
// See: `crates/aero-cpu-core/src/mem.rs` (`aero_cpu_core::mem::CpuBus`)
//
// Notes:
// - Addresses are *linear* (paging translation is handled by the bus)
// - Operations return `Result` so the CPU can raise architectural faults (#PF, #GP)
// - The real trait also includes scalar reads/writes, bulk byte ops, atomic_rmw,
//   and preflight_write_bytes for fault-atomic multi-byte writes
pub trait CpuBus {
    fn sync(&mut self, state: &aero_cpu_core::state::CpuState) {}
    fn invlpg(&mut self, vaddr: u64) {}

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception>;
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception>;

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception>;
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception>;
    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), aero_cpu_core::Exception>;
}
```

**Do not change this interface without cross-workstream review.** It affects every device model.

---

## Tasks

### CPU-Decoder Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| CD-001 | Implement prefix parsing (legacy, REX, VEX) | P0 | None | Medium |
| CD-002 | Implement 1-byte opcode table | P0 | CD-001 | High |
| CD-003 | Implement 2-byte opcode table (0F xx) | P0 | CD-001 | High |
| CD-004 | Implement 3-byte opcode tables | P1 | CD-003 | Medium |
| CD-005 | Implement ModR/M + SIB parsing | P0 | None | Medium |
| CD-006 | Implement displacement/immediate parsing | P0 | CD-005 | Low |
| CD-007 | Implement VEX/EVEX prefix handling | P1 | CD-001 | Medium |
| CD-008 | SSE instruction decoding | P0 | CD-003 | High |
| CD-009 | AVX instruction decoding | P2 | CD-007 | High |
| CD-010 | Decoder test suite | P0 | CD-002 | Medium |

### CPU-Interpreter Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| CI-001 | Data movement instructions (MOV, PUSH, POP) | P0 | CD-002 | Medium |
| CI-002 | Arithmetic instructions (ADD, SUB, MUL, DIV) | P0 | CD-002 | High |
| CI-003 | Logical instructions (AND, OR, XOR, NOT) | P0 | CD-002 | Medium |
| CI-004 | Shift/rotate instructions | P0 | CD-002 | Medium |
| CI-005 | Control flow (JMP, CALL, RET, Jcc) | P0 | CD-002 | Medium |
| CI-006 | String instructions (MOVS, STOS, CMPS) | P0 | CD-002 | Medium |
| CI-007 | Bit manipulation (BT, BTS, BSF, BSR) | P1 | CD-002 | Medium |
| CI-008 | System instructions (INT, IRET, SYSCALL) | P0 | CD-002 | High |
| CI-009 | Privileged instructions (MOV CR/DR, LGDT) | P0 | CD-002 | Medium |
| CI-010 | x87 FPU instructions | P1 | CD-002 | Very High |
| CI-011 | SSE instructions (scalar) | P0 | CD-008 | High |
| CI-012 | SSE instructions (packed) | P0 | CD-008 | Very High |
| CI-013 | SSE2 instructions | P0 | CD-008 | High |
| CI-014 | SSE3/SSSE3 instructions | P1 | CI-012 | Medium |
| CI-015 | SSE4.1/4.2 instructions | P1 | CI-014 | Medium |
| CI-016 | Flag computation (lazy evaluation) | P0 | CI-002 | Medium |
| CI-017 | Interpreter test suite | P0 | CI-001..CI-009 | Very High |

### CPU-JIT Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| CJ-001 | Basic block detection | P0 | CD-010 | Medium |
| CJ-002 | IR (intermediate representation) design | P0 | None | High |
| CJ-003 | x86 → IR translation | P0 | CJ-001, CJ-002 | Very High |
| CJ-004 | IR → WASM code generation | P0 | CJ-002 | Very High |
| CJ-005 | Code cache management | P0 | CJ-004 | Medium |
| CJ-006 | Execution counter / hot path detection | P0 | None | Medium |
| CJ-007 | Baseline JIT (Tier 1) | P0 | CJ-003, CJ-004 | High |
| CJ-008 | Constant folding optimization | P1 | CJ-002 | Medium |
| CJ-009 | Dead code elimination | P1 | CJ-002 | Medium |
| CJ-010 | Common subexpression elimination | P1 | CJ-002 | Medium |
| CJ-011 | Flag elimination optimization | P1 | CJ-002 | Medium |
| CJ-012 | Register allocation | P1 | CJ-002 | High |
| CJ-013 | Optimizing JIT (Tier 2) | P1 | CJ-008..CJ-012 | Very High |
| CJ-014 | SIMD code generation | P1 | CJ-004 | High |
| CJ-015 | JIT test suite | P0 | CJ-007 | High |
| CJ-016 | Inline RAM loads/stores via JIT TLB fast-path | P0 | CJ-004, MM-012 | High |
| CJ-017 | MMIO/IO exits for JIT memory ops | P0 | CJ-016, MM-003 | Medium |

### Memory Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| MM-001 | Physical memory allocation | P0 | None | Low |
| MM-002 | Memory bus routing | P0 | MM-001 | Medium |
| MM-003 | MMIO region management | P0 | MM-002 | Medium |
| MM-004 | 32-bit paging | P0 | MM-002 | Medium |
| MM-005 | PAE paging | P0 | MM-004 | Medium |
| MM-006 | 4-level paging (long mode) | P0 | MM-005 | Medium |
| MM-007 | TLB implementation | P0 | MM-006 | High |
| MM-008 | TLB invalidation (INVLPG, CR3 write) | P0 | MM-007 | Medium |
| MM-009 | Page fault handling | P0 | MM-006 | Medium |
| MM-010 | Sparse memory allocation | P1 | MM-001 | Medium |
| MM-011 | Memory test suite | P0 | MM-006 | Medium |
| MM-012 | JIT-visible TLB layout (stable offsets, packed entries) | P0 | MM-007 | Medium |
| MM-013 | `mmu_translate` helper for JIT (page walk + fill TLB) | P0 | MM-006..MM-012 | High |
| MM-014 | MMIO classification/epoch for JIT fast-path safety | P0 | MM-003, MM-012 | Medium |

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Interpreter speed | Baseline (measure first) |
| Tier 1 JIT | ≥10x interpreter |
| Tier 2 JIT | ≥100 MIPS sustained |
| TLB hit rate | ≥95% for typical workloads |

---

## Coordination Points

### Dependencies on Other Workstreams

- **Graphics (B)**: VGA register access goes through `CpuBus::io_read/io_write`
- **I/O (D, E, F, G)**: All device I/O goes through `CpuBus`
- **Integration (H)**: BIOS and ACPI depend on CPU modes working

### What Other Workstreams Need From You

- Stable `CpuBus` interface
- Working protected mode and long mode
- Correct exception delivery (for device drivers)
- Interrupt injection API (for APIC)

---

## Testing

```bash
# Run CPU tests
bash ./scripts/safe-run.sh cargo test -p aero-cpu-core --locked
bash ./scripts/safe-run.sh cargo test -p aero-cpu-decoder --locked
# Note: `aero-jit-x86` can exceed safe-run's 10-minute default on cold caches.
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-jit-x86 --locked
bash ./scripts/safe-run.sh cargo test -p aero-mmu --locked

# Lint (treat warnings as errors)
bash ./scripts/safe-run.sh cargo clippy -p aero-jit-x86 --all-targets --all-features --locked -- -D warnings

# Focused smoke tests (fast, covers Tier-1 32-bit mode + PF-008 payloads)
bash ./scripts/safe-run.sh cargo test -p aero-x86 --locked
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-jit-x86 --locked --test pf008_tier1_32
AERO_TIMEOUT=1200 bash ./scripts/safe-run.sh cargo test -p aero-jit-x86 --locked --test pf008_tier1_32bit

# Run all tests (workspace-wide; first run can take >10 minutes in clean/contended sandboxes)
AERO_TIMEOUT=1800 bash ./scripts/safe-run.sh cargo test --locked
```

Use `cargo xtask conformance` (or `just test-conformance`) for differential instruction
semantics testing (`crates/conformance`, Aero vs reference backend).

```bash
# Recommended (uses scripts/safe-run.sh internally)
cargo xtask conformance --cases 512

# In contended/CI-like sandboxes, wrap the outer cargo invocation too (builds xtask under safe-run):
bash ./scripts/safe-run.sh cargo xtask conformance --cases 512 -- --nocapture
```

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/02-cpu-emulation.md`](../docs/02-cpu-emulation.md)
4. ☐ Read [`docs/03-memory-management.md`](../docs/03-memory-management.md)
5. ☐ Explore `crates/aero-cpu-core/src/` to understand current state
6. ☐ Run existing tests to establish baseline
7. ☐ Pick a task from the tables above and begin

---

*This workstream is the heart of the emulator. Performance here determines everything.*
