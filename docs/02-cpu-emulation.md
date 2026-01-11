# 02 - CPU Emulation Engine

## Canonical implementation (post-refactor)

The canonical CPU core lives in `crates/aero-cpu-core` (`aero_cpu_core`). The public API is intentionally centered around a **single architectural state struct** that is shared by all execution tiers.

**Primary components:**

- **Canonical CPU state / JIT ABI:** `aero_cpu_core::state::CpuState`  
  Source: [`crates/aero-cpu-core/src/state.rs`](../crates/aero-cpu-core/src/state.rs)
- **Canonical interpreter (Tier 0):** `aero_cpu_core::interp::tier0`  
  Source: [`crates/aero-cpu-core/src/interp/tier0/mod.rs`](../crates/aero-cpu-core/src/interp/tier0/mod.rs)
- **Paging integration:** `aero_cpu_core::PagingBus` (adapter wrapping `aero_mmu`)  
  Source: [`crates/aero-cpu-core/src/paging_bus.rs`](../crates/aero-cpu-core/src/paging_bus.rs)
- **Architectural interrupt/exception delivery:** `aero_cpu_core::CpuCore` (`CpuState` + `PendingEventState` + deterministic time)  
  Source: [`crates/aero-cpu-core/src/interrupts.rs`](../crates/aero-cpu-core/src/interrupts.rs)
- **Tiered exec glue:** `aero_cpu_core::exec` (`Vcpu`, `Tier0Interpreter`, `ExecDispatcher`)  
  Source: [`crates/aero-cpu-core/src/exec/mod.rs`](../crates/aero-cpu-core/src/exec/mod.rs)

**Legacy CPU stacks:**

- The old interpreter stack (`aero_cpu_core::cpu` + `aero_cpu_core::bus`) is **feature-gated** behind `legacy-interp` (default-off). It is not the primary path and should not be used for new work.

---

## CPU state = JIT ABI (`CpuState`)

`CpuState` is the **canonical in-memory ABI** between:

- the Tier-0 interpreter (`interp::tier0`), and
- dynamically generated JIT blocks (WASM codegen; Tier-1+).

It is:

- `#[repr(C, align(16))]` (layout is intentional)
- validated by compile-time asserts and unit tests
- accessed by JIT code via exported byte offsets

### ABI stability rules

When modifying `CpuState`, treat it like a public C ABI:

1. **Do not reorder fields casually.** Reordering changes offsets and will break any JIT backend that assumes them.
2. **Update offsets + tests together.** The crate exposes offset constants and tests that freeze them.
3. **Add new offset constants when new fields become JIT-visible.** (The JIT can’t safely “guess” Rust layout.)

The currently exported offsets (see `state.rs`) include:

- `CPU_GPR_BASE_OFF` (intentionally 0)
- `CPU_GPR_OFF[i]` for `RAX..R15`
- `CPU_RIP_OFF`
- `CPU_RFLAGS_OFF`
- `CPU_XMM_OFF[i]` for `XMM0..XMM15`
- `CPU_STATE_SIZE`, `CPU_STATE_ALIGN`

The corresponding “do not regress this” tests live alongside the type:

- `jit_offsets_are_stable` in [`state.rs`](../crates/aero-cpu-core/src/state.rs)

### What lives outside the ABI?

`CpuState` contains **architectural CPU state** (GPRs, RIP/RFLAGS, segment caches, control/debug registers, MSRs, FPU/SSE state, etc). Runtime/bookkeeping state lives outside the ABI so it can evolve without breaking JIT code:

- CPUID policy, deterministic time knobs, INVLPG logging: `assist::AssistContext`
- Pending interrupts/exceptions, interrupt shadow bookkeeping, IRET frame stack: `interrupts::PendingEventState`
- JIT runtime caches/profiling/hotness counters: `jit::*`

---

## Execution loop shape (Tier-0 + tiered runtime)

At a high level, the CPU runs in a loop that:

1. **delivers any pending architectural events** (exceptions, software interrupts, external interrupts), then
2. **executes a “block”** using either Tier-0 or the JIT, then
3. repeats.

In `aero_cpu_core`, this is modeled by:

- `exec::ExecDispatcher` (chooses Tier-0 vs JIT per block)
- `exec::Vcpu` (bundles `CpuCore` + a bus and implements `ExecCpu`)

`ExecDispatcher::step()` always gives interrupts a chance at *instruction boundaries* via `ExecCpu::maybe_deliver_interrupt()` before running the next block.

---

## Tier-0 interpreter (`interp::tier0`)

Tier-0 is the canonical interpreter and executes directly on:

- `&mut state::CpuState` (architectural state / JIT ABI), and
- `&mut impl mem::CpuBus` (abstract memory + IO).

Key entry points:

- `interp::tier0::exec::step` (single instruction)
- `interp::tier0::exec::run_batch` (execute up to N instructions)
- `interp::tier0::exec::run_batch_with_assists` (same, but resolves assist exits)

Tier-0 intentionally keeps the “core” instruction set tight. When it encounters instructions that depend on additional platform/system state, it returns an **assist exit**:

- `AssistReason::Io` (`IN/OUT/INS*/OUTS*`)
- `AssistReason::Cpuid` (`CPUID`)
- `AssistReason::Msr` (`RDMSR/WRMSR`)
- `AssistReason::Privileged` (descriptor tables, `INVLPG`, far control transfers, etc.)
- `AssistReason::Interrupt` (`INT*`, `IRET*`, `CLI/STI`, `INTO`)

Assists are resolved by the `assist` layer (below). For interrupt-related semantics, the *canonical* delivery logic lives in `interrupts` (see next section), so most integrations should prefer the glue in `exec::Tier0Interpreter` instead of calling `assist` directly for `AssistReason::Interrupt`.

---

## Assist handling (`assist.rs`)

The assist layer emulates instructions that Tier-0 does not implement directly.

API:

- `assist::handle_assist` (fetch + decode + execute)
- `assist::handle_assist_decoded` (execute already-decoded instruction)

Runtime state for assists is carried in `assist::AssistContext`:

- `features`: CPUID feature policy (also used to mask MSR writes coherently)
- `tsc_step`: deterministic increment for `RDTSC/RDTSCP`
- `invlpg_log`: optional log of invalidated linear addresses (useful for tests)

Important integration detail: assists may modify paging-related state (`CR0/CR3/CR4/EFER`, CPL via segment loads, etc.). The CPU bus contract supports this via `CpuBus::sync(state)`:

- Tier-0 calls `bus.sync(state)` once per instruction boundary.
- `handle_assist` also calls `bus.sync(state)` before and after executing the assist to keep translation state coherent even when used outside the Tier-0 loop.

---

## Paging integration (`PagingBus` + `CpuBus::sync/invlpg`)

### `CpuBus` contract

Tier-0 reads/writes memory through `mem::CpuBus`, which is intentionally *linear-address based*:

- Tier-0 passes **linear addresses** (after segmentation/A20 masking) to the bus.
- A paging-aware bus is responsible for translating linear → physical.

`CpuBus` also includes two hooks that matter for paging:

- `sync(&CpuState)`: called at instruction boundaries so the bus can observe changes to CR0/CR3/CR4/EFER and CPL.
- `invlpg(vaddr)`: called by the `INVLPG` assist to invalidate a single translation.

See: [`crates/aero-cpu-core/src/mem.rs`](../crates/aero-cpu-core/src/mem.rs)

### `PagingBus`

`PagingBus<B>` is the canonical adapter that implements `CpuBus` by wrapping:

- an `aero_mmu::Mmu` (page table walker + TLB), and
- an underlying **physical** bus `B: aero_mmu::MemoryBus`.

It translates every access using `aero-mmu`, and it updates cached MMU state on `sync()` by calling `CpuState::sync_mmu(&mut mmu)`.

See: [`crates/aero-cpu-core/src/paging_bus.rs`](../crates/aero-cpu-core/src/paging_bus.rs)

---

## Interrupts/exceptions (`interrupts.rs`)

Architectural delivery (IVT/IDT, privilege stack switching, IST, IRET) lives in `aero_cpu_core::interrupts`.

Key types:

- `CpuCore` (re-exported as `aero_cpu_core::CpuCore`) = `{ state: CpuState, pending: PendingEventState, time: time::TimeSource }`
- `interrupts::PendingEventState` tracks:
  - deferred pending faults/traps/interrupts
  - external interrupt FIFO
  - interrupt-shadow state (`STI`, `MOV SS`, `POP SS`)
  - exception nesting / double-fault escalation
  - an internal IRET frame stack (so `IRET*` can validate/match the correct frame)

Execution engines are responsible for aging the interrupt shadow state by calling
`PendingEventState::retire_instruction()` after each successfully executed instruction.
The `exec::Tier0Interpreter` glue handles this for you.

### How execution glue should deliver faults

Tier-0’s `step()` returns `Result<StepExit, aero_cpu_core::Exception>`. An `Err(e)` indicates a **synchronous fault** at the current instruction boundary (e.g. `#PF`, `#GP(0)`).

The canonical pattern is:

1. Let Tier-0 set architectural side effects (today: CR2 for `#PF`) via `CpuState::apply_exception_side_effects`.
2. Convert the error into an architectural pending event (`exceptions::Exception`) using
   `PendingEventState::raise_exception_fault(...)`.
3. Deliver it via `CpuCore::deliver_pending_event(&mut bus)` at the next boundary.

Most integration code should use `exec::Vcpu` and deliver through `Vcpu::maybe_deliver_interrupt()`, which already routes to `CpuCore::{deliver_pending_event, deliver_external_interrupt}` and records a sticky `CpuExit` on triple fault.

---

## Developer quickstart (tests)

This is the smallest “bring-up” loop for unit tests using the canonical types:

- `exec::Vcpu<FlatTestBus>`
- `exec::Tier0Interpreter`
- external interrupt injection via `PendingEventState`

```rust
use aero_cpu_core::exec::{Interpreter as _, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::{CpuBus as _, FlatTestBus};
use aero_cpu_core::state::CpuMode;
use aero_x86::Register;

// A tiny real-mode program: NOP; HLT
let mut bus = FlatTestBus::new(0x10000);
let code_base = 0x0100u64;
bus.load(code_base, &[0x90, 0xF4]);

// IVT[0x20] -> 0000:0500, handler = HLT
let vector = 0x20u8;
let handler_off = 0x0500u16;
let ivt_addr = (vector as u64) * 4;
bus.write_u16(ivt_addr, handler_off).unwrap();
bus.write_u16(ivt_addr + 2, 0).unwrap();
bus.load(handler_off as u64, &[0xF4]);

let mut vcpu = Vcpu::new_with_mode(CpuMode::Real, bus);
vcpu.cpu.state.write_reg(Register::CS, 0);
vcpu.cpu.state.write_reg(Register::SS, 0);
vcpu.cpu.state.write_reg(Register::SP, 0x8000);
vcpu.cpu.state.set_rflags(0x0202); // IF=1, bit1=1
vcpu.cpu.state.set_rip(code_base);

let mut interp = Tier0Interpreter::new(1024);

// Run until the first HLT.
interp.exec_block(&mut vcpu);
assert!(vcpu.cpu.state.halted);

// Inject an external interrupt (e.g. PIC/APIC) and run again.
vcpu.cpu.pending.inject_external_interrupt(vector);
interp.exec_block(&mut vcpu);

// The interrupt handler ran and halted.
assert!(vcpu.cpu.state.halted);
assert_eq!(vcpu.cpu.state.segments.cs.selector, 0x0000);
assert_eq!(vcpu.cpu.state.rip(), handler_off as u64 + 1); // HLT advances RIP
assert_eq!(vcpu.cpu.state.read_reg(Register::SP) as u16, 0x7FFA); // 3 pushes in real mode
```

### Optional: paging-enabled tests

If you want Tier-0 to run with paging enabled, wrap a physical memory bus (`aero_mmu::MemoryBus`) in `PagingBus`:

```rust
use aero_cpu_core::{exec::Vcpu, state::CpuMode, PagingBus};
use aero_mmu::MemoryBus;

struct MyPhysBus { /* ... */ }
impl MemoryBus for MyPhysBus { /* ... */ }

let phys = MyPhysBus { /* ... */ };
let bus = PagingBus::new(phys);
let mut vcpu = Vcpu::new_with_mode(CpuMode::Long, bus);
```

For a concrete `MemoryBus` implementation, see the unit tests under
[`crates/aero-cpu-core/tests/paging.rs`](../crates/aero-cpu-core/tests/paging.rs).

---

## CPUID / feature model

See [`docs/cpu/README.md`](./cpu/README.md) for current CPUID leaf coverage and feature profiles.
