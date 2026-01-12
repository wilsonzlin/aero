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
- **Architectural interrupt/exception delivery:** `aero_cpu_core::CpuCore` (`CpuState` + `PendingEventState` + `time::TimeSource`)  
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

- CPUID policy, INVLPG logging: `assist::AssistContext`
- Virtual time / TSC model: `time::TimeSource` (typically stored in `CpuCore.time`)
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

### Instruction retirement accounting (time + interrupt-shadow)

“Instruction retirement” is the canonical unit used by:

- virtual time / TSC progression (`time::TimeSource`), and
- interrupt-shadow aging (`interrupts::PendingEventState`).

Tier-0 already performs these updates once per retired guest instruction. **Tiered/JIT execution must preserve the exact same semantics.**

In particular:

- **Committed JIT exits:** a compiled block must retire exactly `block_instruction_count` guest instructions.
  - `block_instruction_count` is provided out-of-band by the compilation pipeline via
    `CompiledBlockMeta.instruction_count` (stored alongside the cached block handle).
- **Rollback JIT exits:** if a block exits via an MMIO/page-fault/runtime bailout that restores the pre-block
  architectural state, it must retire **0** guest instructions.
- **Interrupt shadow:** `PendingEventState` must be aged by the **same retired-instruction count** as virtual time.
  - Do **not** age the shadow on rollback exits, and do **not** “fake” retirement by counting blocks instead of
    guest instructions.

This retirement count is also the right unit for embedding-facing instruction counters (see “Perf counters
integration” below).

### Tiered/JIT block exit signaling (Tasks 5/6)

To make the above unambiguous and hard to regress, the tiered runtime uses explicit API surfaces:

- `CompiledBlockMeta.instruction_count`: number of guest architectural instructions in the compiled block.
- `JitBlockExit`: includes committed vs rollback signaling in addition to the next RIP / “exit to interpreter”
  decision.
- `ExecDispatcher::StepOutcome::Block { instructions_retired, .. }` (Task 6): reports the exact number of guest
  instructions retired by the step, regardless of which tier executed.

---

## Browser Tier-1 JIT compilation pipeline (browser workers)

The project includes a browser-only Tier-1 JIT integration that compiles x86 basic blocks into standalone
WASM modules **inside a worker**:

- **Tiered runtime (Tier-0 + dispatch + cache):** `crates/aero-wasm/src/tiered_vm.rs` (`WasmTieredVm`)
  - The WASM runtime exports:
    - `drain_compile_requests()` → entry RIPs that became hot
    - `install_tier1_block(entry_rip, table_index, code_paddr, byte_len)` → installs compiled blocks
- **Tier-1 compiler (x86 → WASM):** `crates/aero-jit-wasm` (`aero-jit-wasm`)
  - Built as a separate wasm-bindgen package (`web/src/wasm/pkg-jit-*`).
  - Uses its **own private linear memory** (does not import the emulator's shared memory) to avoid
    undefined behaviour from multiple Rust runtimes aliasing one `WebAssembly.Memory`.
- **JS glue (web runtime):**
  - CPU worker: `web/src/workers/cpu.worker.ts` drives the `WasmVm` export (and may use `WasmTieredVm` for tiered/JIT
    execution) and wires up the JS shims used by the runtime (`globalThis.__aero_io_port_*`, `globalThis.__aero_mmio_*`,
    and Tier-1 `globalThis.__aero_jit_call`).
  - JIT worker: `web/src/workers/jit.worker.ts` compiles provided WASM bytes into a `WebAssembly.Module` (cached by
    content hash). If `WebAssembly.Module` is not structured-cloneable, it reports an `unsupported` error instead.
  - Loader: `web/src/runtime/jit_wasm_loader.ts` loads `aero-jit-wasm` and currently prefers the
    single-threaded package to avoid wasm-bindgen allocating huge `SharedArrayBuffer`s during
    instantiation when `--max-memory` is large.

> Note: `crates/aero-wasm` exposes multiple WASM-facing VM wrappers. The canonical **full-system** export is
> `aero_wasm::Machine` (backed by `aero_machine::Machine`). The current CPU-worker runtime uses the legacy CPU-only
> `WasmVm` / `WasmTieredVm` exports.
>
> See [`docs/vm-crate-map.md`](./vm-crate-map.md) and [ADR 0014](./adr/0014-canonical-machine-stack.md) for the
> up-to-date mapping.

This pipeline is currently used by the repo-root Playwright smoke tests to validate Tier-1 compilation,
installation, invalidation (self-modifying code), and execution in real browsers.

---

## Tier-0 interpreter (`interp::tier0`)

Tier-0 is the canonical interpreter and executes directly on:

- `&mut state::CpuState` (architectural state / JIT ABI), and
- `&mut impl mem::CpuBus` (abstract memory + IO).

Key entry points:

- `interp::tier0::exec::step` (single instruction)
- `interp::tier0::exec::run_batch` (execute up to N instructions)
- `interp::tier0::exec::run_batch_with_assists` (resolves non-interrupt assist exits)
- `interp::tier0::exec::run_batch_cpu_core_with_assists` (resolves all assists, including interrupt-related ones)

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

Runtime state for assists is split across:

- `assist::AssistContext` (non-ABI):
  - `features`: CPUID feature policy (also used to mask MSR writes coherently)
  - `invlpg_log`: optional log of invalidated linear addresses (useful for tests)
- `time::TimeSource` (non-ABI): owns virtual TSC progression and is typically stored on `CpuCore` as `cpu.time`.

Callers that use the assist layer directly should pass both the architectural state and the time source:

```rust
assist::handle_assist(&mut ctx, &mut cpu.time, &mut cpu.state, &mut bus, reason)?;
```

Important integration detail: assists may modify paging-related state (`CR0/CR3/CR4/EFER`, CPL via segment loads, etc.). The CPU bus contract supports this via `CpuBus::sync(state)`:

- Tier-0 calls `bus.sync(state)` once per instruction boundary.
- `handle_assist` also calls `bus.sync(state)` before and after executing the assist to keep translation state coherent even when used outside the Tier-0 loop.

### Virtual time / TSC semantics (`time::TimeSource`)

The CPU’s timestamp counter is modeled by [`time::TimeSource`](../crates/aero-cpu-core/src/time.rs) (not by `CpuState` or `AssistContext`):

- **Deterministic mode (default):** TSC advances on *guest instruction retirement*.
  - Tier-0 increments time via `TimeSource::advance_cycles(1)` once per retired instruction (including assists).
  - JIT/tiered execution must advance by the exact same number of retired guest instructions:
    - committed JIT block: advance by `CompiledBlockMeta.instruction_count`
    - rollback JIT exit: advance by `0`
- **Wall-clock mode (optional):** TSC is derived from a host `Instant` stored inside `TimeSource` (intended for native/non-WASM integrations; it is inherently non-deterministic).
- **Coherency:** `CpuState.msr.tsc` mirrors `TimeSource`:
  - execution glue updates `state.msr.tsc = time.read_tsc()` after each retirement, and
  - `RDTSC/RDTSCP` and `RDMSR/WRMSR IA32_TSC` read/write through `TimeSource` and update `state.msr.tsc` as part of their architectural semantics.

### Perf counters integration

Instruction retirement is also the unit used by `aero-perf` counters (`PerfWorker::retire_instructions`).
When driving the CPU through `ExecDispatcher`, embedders should use the dispatcher-provided retirement count,
which already accounts for interpreter vs JIT and committed vs rollback exits:

```rust
use aero_cpu_core::exec::StepOutcome;

loop {
    let outcome = dispatcher.step(&mut vcpu);
    match outcome {
        StepOutcome::InterruptDelivered => {}
        StepOutcome::Block {
            tier,
            instructions_retired,
            ..
        } => {
            perf.retire_instructions(instructions_retired);
            // `tier` can be used for profiling, but instruction counting does not need to special-case it.
            let _ = tier;
        }
    }
}
```

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
