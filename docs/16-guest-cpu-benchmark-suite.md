# 16 - Guest CPU Instruction Throughput Benchmarks (PF-008)

## Overview

Host-side microbenchmarks (e.g. WASM loops) are useful for tracking runtime/compiler regressions, but they **do not measure the emulator’s actual core work** (fetch/decode/execute, memory emulation, tiered JIT overhead).

PF-008 adds a **deterministic guest instruction throughput suite** that runs small x86/x86-64 instruction streams directly inside the CPU core (no OS images required).

Goals:

1. Measure **guest IPS/MIPS** for representative instruction mixes.
2. Work as soon as the interpreter exists (CI-001..CI-005) and scales to baseline/optimizing JIT.
3. Provide correctness guardrails via **checksums** (fast-but-wrong regressions fail the run).
4. Export results via `window.aero.perf.export()` and exercise via a Playwright scenario.

Non-goals:

 - Measuring full-system “Windows boot” performance (covered by image-based benchmarks).
 - Measuring host JS/WASM performance in isolation.

---

## Public API (JS)

Expose a JS-callable bench entrypoint:

```ts
type GuestCpuMode = "interpreter" | "jit_baseline" | "jit_opt";

type GuestCpuBenchVariant =
  | "alu64"
  | "alu32"
  | "branch_pred64"
  | "branch_pred32"
  | "branch_unpred64"
  | "branch_unpred32"
  | "mem_seq64"
  | "mem_seq32"
  | "mem_stride64"
  | "mem_stride32"
  | "call_ret64"
  | "call_ret32";

type GuestCpuBenchOpts = {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;

  // Preferred: run until seconds budget is reached (multiple runs).
  seconds?: number;

  // Optional: run a single *measured* payload invocation for N iterations.
  // The harness will also perform an unmeasured "reference run" (same variant/iters)
  // to derive `expected_checksum` and assert determinism.
  // This is mainly for debugging; the main suite uses `seconds`.
  iters?: number;
};

type GuestCpuBenchRun = {
  variant: GuestCpuBenchVariant;
  mode: GuestCpuMode;

  // Configuration
  iters_per_run: number;
  warmup_runs: number;
  measured_runs: number;

  // Correctness
  expected_checksum: string; // hex string (e.g. "0xf935...")
  observed_checksum: string;

  // Counters
  total_instructions: number; // u64-ish; in JS as number if safe or bigint
  total_seconds: number;

  // Result
  ips: number;
  mips: number;

  // Variance indicators (per-run)
  run_mips: number[];
  mips_mean: number;
  mips_stddev: number;
  mips_min: number;
  mips_max: number;
};

// JS API surface:
window.aero.bench.runGuestCpuBench(opts: GuestCpuBenchOpts): Promise<GuestCpuBenchRun>;
```

Notes:

 - If `seconds` is specified, the harness repeats deterministic runs until the budget is reached.
 - `seconds` and `iters` are mutually exclusive; if provided they must be finite and > 0.
 - If checksum verification fails, `runGuestCpuBench` **throws** (and Playwright marks the scenario failed).
 - `mode` selects interpreter vs baseline JIT vs optimizing JIT. If a mode is not implemented yet, the API should fail loudly (or optionally downgrade while clearly reporting the actual mode used).

---

## Guest payload ABI (minimal, OS-less)

Each payload is a flat blob of machine code that:

 - consumes an iteration count (`ECX`/`RCX`)
 - optionally consumes a pointer to a scratch buffer (`EDI`/`RDI`)
 - returns a checksum (`EAX`/`RAX`)
 - ends with `ret`

The harness is responsible for:

 - allocating + initializing a scratch memory buffer for memory variants
 - setting up CPU state (stack pointer, code location, execution mode)
 - selecting execution tier (interp/JIT)
 - counting retired instructions (PF-002)

### Why `ret`?

It avoids needing privileged instructions (`hlt`) or exception-based exits (`ud2`). The harness can start the CPU at the payload entrypoint with a valid stack and stop when it returns.

---

## Payload set (required)

All payloads are:

 - deterministic
 - self-contained (no syscalls, no I/O)
 - checksum-returning

The suite includes:

1. **Tight integer ALU loop** (`alu32`, `alu64`)
2. **Branch-heavy loop (predictable)** (`branch_pred32`, `branch_pred64`)
3. **Branch-heavy loop (unpredictable)** (`branch_unpred32`, `branch_unpred64`)
4. **Memory load/store (sequential)** (`mem_seq32`, `mem_seq64`)
5. **Memory load/store (strided)** (`mem_stride32`, `mem_stride64`)
6. **Call/ret heavy (stack)** (`call_ret32`, `call_ret64`)

### Canonical checksum verification

To keep correctness checks fast and deterministic, the suite defines a canonical iteration count:

 - `iters_per_run = 10_000`

For each variant, we include:

 - the canonical machine code bytes
 - the expected checksum after `iters_per_run`

The harness should reset guest-visible state between runs (registers and, for memory benches, the scratch buffer) so that each run is identical and checksum verification remains valid.

---

## Payload definitions (assembly + bytes + expected checksums)

All byte arrays below are flat binaries assembled with NASM (e.g. `nasm -f bin file.asm -o file.bin`).

> **Important:** These payloads assume a *flat address space* from the harness. They are designed to be run inside the CPU core test harness, not in a full BIOS/OS boot flow.

### 1) `alu64`

Tight loop over integer ALU + shifts.

**Input:** `RCX = iters`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000):** `0xf935f9482b8f99b8`

Assembly:

```nasm
BITS 64
mov rax, 0x123456789ABCDEF0
mov rdx, 0x9E3779B97F4A7C15
.loop:
  add rax, rdx
  mov rbx, rax
  shr rbx, 13
  xor rax, rbx
  shl rax, 1
  dec rcx
  jnz .loop
ret
```

Bytes:

```text
0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xba, 0x15, 0x7c, 0x4a, 0x7f,
0xb9, 0x79, 0x37, 0x9e, 0x48, 0x01, 0xd0, 0x48, 0x89, 0xc3, 0x48, 0xc1, 0xeb, 0x0d, 0x48, 0x31,
0xd8, 0x48, 0xd1, 0xe0, 0x48, 0xff, 0xc9, 0x75, 0xeb, 0xc3
```

---

### 2) `alu32`

32-bit variant of the ALU loop.

**Input:** `ECX = iters`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000):** `0x30aae0b8`

Assembly:

```nasm
BITS 32
mov eax, 0x9ABCDEF0
mov edx, 0x7F4A7C15
.loop:
  add eax, edx
  mov ebx, eax
  shr ebx, 13
  xor eax, ebx
  shl eax, 1
  dec ecx
  jnz .loop
ret
```

Bytes:

```text
0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xba, 0x15, 0x7c, 0x4a, 0x7f, 0x01, 0xd0, 0x89, 0xc3, 0xc1, 0xeb,
0x0d, 0x31, 0xd8, 0xd1, 0xe0, 0x49, 0x75, 0xf2, 0xc3
```

---

### 3) `branch_pred64`

Branch-heavy loop with *predictable* conditional branches (always not taken), plus the loop back-edge.

**Input:** `RCX = iters`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000):** `0xd7ab5d5aaad6afab`

Assembly:

```nasm
BITS 64
mov rax, 0x123456789ABCDEF0
mov rbx, 0x9E3779B97F4A7C15
.loop:
  xor rdx, rdx
  jnz .skip_add
  add rax, rbx
.skip_add:
  xor rdx, rdx
  jnz .skip_xor
  xor rax, rbx
.skip_xor:
  shl rax, 1
  add rax, 1
  dec rcx
  jnz .loop
ret
```

Bytes:

```text
0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xbb, 0x15, 0x7c, 0x4a, 0x7f,
0xb9, 0x79, 0x37, 0x9e, 0x48, 0x31, 0xd2, 0x75, 0x03, 0x48, 0x01, 0xd8, 0x48, 0x31, 0xd2, 0x75,
0x03, 0x48, 0x31, 0xd8, 0x48, 0xd1, 0xe0, 0x48, 0x83, 0xc0, 0x01, 0x48, 0xff, 0xc9, 0x75, 0xe4,
0xc3
```

---

### 4) `branch_pred32`

32-bit predictable branch variant.

**Input:** `ECX = iters`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000):** `0xaad6afab`

Assembly:

```nasm
BITS 32
mov eax, 0x9ABCDEF0
mov ebx, 0x7F4A7C15
.loop:
  xor edx, edx
  jnz .skip_add
  add eax, ebx
.skip_add:
  xor edx, edx
  jnz .skip_xor
  xor eax, ebx
.skip_xor:
  shl eax, 1
  add eax, 1
  dec ecx
  jnz .loop
ret
```

Bytes:

```text
0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0x31, 0xd2, 0x75, 0x02, 0x01, 0xd8,
0x31, 0xd2, 0x75, 0x02, 0x31, 0xd8, 0xd1, 0xe0, 0x83, 0xc0, 0x01, 0x49, 0x75, 0xec, 0xc3
```

---

### 5) `branch_unpred64`

Branch-heavy loop with pseudo-random branch direction (xorshift64).

**Input:** `RCX = iters`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000):** `0xdf14f128b035d3f0`

Assembly:

```nasm
BITS 64
mov rax, 0x123456789ABCDEF0 ; RNG state
mov rbx, 0x0F0E0D0C0B0A0908 ; accumulator / checksum
.loop:
  mov rdx, rax
  shl rdx, 13
  xor rax, rdx
  mov rdx, rax
  shr rdx, 7
  xor rax, rdx
  mov rdx, rax
  shl rdx, 17
  xor rax, rdx

  mov rdx, rax
  and rdx, 1
  jz .even
  add rbx, rax
  jmp .after
.even:
  xor rbx, rax
  jmp .after
.after:
  dec rcx
  jnz .loop
  mov rax, rbx
ret
```

Bytes:

```text
0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xbb, 0x08, 0x09, 0x0a, 0x0b,
0x0c, 0x0d, 0x0e, 0x0f, 0x48, 0x89, 0xc2, 0x48, 0xc1, 0xe2, 0x0d, 0x48, 0x31, 0xd0, 0x48, 0x89,
0xc2, 0x48, 0xc1, 0xea, 0x07, 0x48, 0x31, 0xd0, 0x48, 0x89, 0xc2, 0x48, 0xc1, 0xe2, 0x11, 0x48,
0x31, 0xd0, 0x48, 0x89, 0xc2, 0x48, 0x83, 0xe2, 0x01, 0x74, 0x05, 0x48, 0x01, 0xc3, 0xeb, 0x05,
0x48, 0x31, 0xc3, 0xeb, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xca, 0x48, 0x89, 0xd8, 0xc3
```

---

### 6) `branch_unpred32`

32-bit xorshift branch variant.

**Input:** `ECX = iters`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000):** `0xb1fdf341`

Assembly:

```nasm
BITS 32
mov eax, 0x9ABCDEF0 ; RNG state
mov ebx, 0x0B0A0908 ; accumulator
.loop:
  mov edx, eax
  shl edx, 13
  xor eax, edx
  mov edx, eax
  shr edx, 7
  xor eax, edx
  mov edx, eax
  shl edx, 17
  xor eax, edx

  mov edx, eax
  and edx, 1
  jz .even
  add ebx, eax
  jmp .after
.even:
  xor ebx, eax
  jmp .after
.after:
  dec ecx
  jnz .loop
  mov eax, ebx
ret
```

Bytes:

```text
0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x08, 0x09, 0x0a, 0x0b, 0x89, 0xc2, 0xc1, 0xe2, 0x0d, 0x31,
0xd0, 0x89, 0xc2, 0xc1, 0xea, 0x07, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xe2, 0x11, 0x31, 0xd0, 0x89,
0xc2, 0x83, 0xe2, 0x01, 0x74, 0x04, 0x01, 0xc3, 0xeb, 0x04, 0x31, 0xc3, 0xeb, 0x00, 0x49, 0x75,
0xd9, 0x89, 0xd8, 0xc3
```

---

### 7) `mem_seq64`

Memory load/store loop over a 4096-byte ring buffer (sequential stride of 8 bytes).

**Input:** `RCX = iters`, `RDI = buffer_base (4096 bytes, 8-byte aligned)`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000, buffer initialized to zero):** `0xb744761306865560`

Assembly:

```nasm
BITS 64
mov rax, 0x0123456789ABCDEF
xor rsi, rsi
.loop:
  mov rdx, [rdi + rsi]
  add rax, rdx
  xor rdx, rax
  mov [rdi + rsi], rdx
  add rsi, 8
  and rsi, 0x0FFF
  dec rcx
  jnz .loop
ret
```

Bytes:

```text
0x48, 0xb8, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0x48, 0x31, 0xf6, 0x48, 0x8b, 0x14,
0x37, 0x48, 0x01, 0xd0, 0x48, 0x31, 0xc2, 0x48, 0x89, 0x14, 0x37, 0x48, 0x83, 0xc6, 0x08, 0x48,
0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xe2, 0xc3
```

---

### 8) `mem_seq32`

32-bit sequential memory loop over a 4096-byte ring buffer (stride of 4 bytes).

**Input:** `ECX = iters`, `EDI = buffer_base (4096 bytes, 4-byte aligned)`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000, buffer initialized to zero):** `0x0cc50aff`

Assembly:

```nasm
BITS 32
mov eax, 0x89ABCDEF
xor esi, esi
.loop:
  mov edx, [edi + esi]
  add eax, edx
  xor edx, eax
  mov [edi + esi], edx
  add esi, 4
  and esi, 0x0FFF
  dec ecx
  jnz .loop
ret
```

Bytes:

```text
0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2, 0x89, 0x14,
0x37, 0x83, 0xc6, 0x04, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75, 0xea, 0xc3
```

---

### 9) `mem_stride64`

Strided memory load/store loop (64-byte stride) over a 4096-byte ring buffer.

**Input:** `RCX = iters`, `RDI = buffer_base (4096 bytes, 8-byte aligned)`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000, buffer initialized to zero):** `0xd8d5ee9d0da7ebb4`

Assembly:

```nasm
BITS 64
mov rax, 0x0123456789ABCDEF
xor rsi, rsi
.loop:
  mov rdx, [rdi + rsi]
  add rax, rdx
  xor rdx, rax
  mov [rdi + rsi], rdx
  add rsi, 64
  and rsi, 0x0FFF
  dec rcx
  jnz .loop
ret
```

Bytes:

```text
0x48, 0xb8, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0x48, 0x31, 0xf6, 0x48, 0x8b, 0x14,
0x37, 0x48, 0x01, 0xd0, 0x48, 0x31, 0xc2, 0x48, 0x89, 0x14, 0x37, 0x48, 0x83, 0xc6, 0x40, 0x48,
0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xe2, 0xc3
```

---

### 10) `mem_stride32`

32-bit strided memory loop (64-byte stride) over a 4096-byte ring buffer.

**Input:** `ECX = iters`, `EDI = buffer_base (4096 bytes, 4-byte aligned)`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000, buffer initialized to zero):** `0x0da7ebb4`

Assembly:

```nasm
BITS 32
mov eax, 0x89ABCDEF
xor esi, esi
.loop:
  mov edx, [edi + esi]
  add eax, edx
  xor edx, eax
  mov [edi + esi], edx
  add esi, 64
  and esi, 0x0FFF
  dec ecx
  jnz .loop
ret
```

Bytes:

```text
0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2, 0x89, 0x14,
0x37, 0x83, 0xc6, 0x40, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75, 0xea, 0xc3
```

---

### 11) `call_ret64`

Call/ret + stack pressure (push/pop) in the callee.

**Input:** `RCX = iters`
**Output:** `RAX = checksum`
**Expected checksum (iters=10_000):** `0x0209be0771df5500`

Assembly:

```nasm
BITS 64
mov rax, 0xFEEDFACECAFEBEEF
mov rbx, 0x9E3779B97F4A7C15
.loop:
  call callee
  dec rcx
  jnz .loop
ret

callee:
  push rbx
  push rsi
  add rax, rbx
  xor rax, 0x1F123BB5
  shl rax, 3
  pop rsi
  pop rbx
ret
```

Bytes:

```text
0x48, 0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xce, 0xfa, 0xed, 0xfe, 0x48, 0xbb, 0x15, 0x7c, 0x4a, 0x7f,
0xb9, 0x79, 0x37, 0x9e, 0xe8, 0x06, 0x00, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xf6, 0xc3, 0x53,
0x56, 0x48, 0x01, 0xd8, 0x48, 0x35, 0xb5, 0x3b, 0x12, 0x1f, 0x48, 0xc1, 0xe0, 0x03, 0x5e, 0x5b,
0xc3
```

---

### 12) `call_ret32`

32-bit call/ret + stack pressure.

**Input:** `ECX = iters`
**Output:** `EAX = checksum`
**Expected checksum (iters=10_000):** `0x71df5500`

Assembly:

```nasm
BITS 32
mov eax, 0xCAFEBEEF
mov ebx, 0x7F4A7C15
.loop:
  call callee
  dec ecx
  jnz .loop
ret

callee:
  push ebx
  push esi
  add eax, ebx
  xor eax, 0x1F123BB5
  shl eax, 3
  pop esi
  pop ebx
ret
```

Bytes:

```text
0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x49,
0x75, 0xf8, 0xc3, 0x53, 0x56, 0x01, 0xd8, 0x35, 0xb5, 0x3b, 0x12, 0x1f, 0xc1, 0xe0, 0x03, 0x5e,
0x5b, 0xc3
```

---

## Execution harness requirements (emulator core)

Add a CPU-core-level helper that can:

1. Load a byte slice into a scratch executable region (or use an existing “execute bytes” harness).
2. Configure the CPU state for x86-32 vs x86-64 payload execution.
3. Run in a chosen tier: interpreter, baseline JIT, optimizing JIT.
4. Return:
   - checksum (`EAX`/`RAX`)
   - retired instruction count delta (PF-002)
   - wall-clock time delta

Measurement must compute throughput from **retired guest instructions**, not from static “instructions in the payload”.

Suggested internal structure:

```rust
pub enum GuestCpuMode { Interpreter, JitBaseline, JitOpt }

pub struct GuestCpuRunResult {
    pub checksum: u64,
    pub retired_instructions: u64,
    pub seconds: f64,
}

pub fn run_guest_payload(
    mode: GuestCpuMode,
    payload: &[u8],
    iters: u32,
    scratch_base: u64, // for mem_* benches
    scratch_len: u32,
) -> Result<GuestCpuRunResult>;
```

### Time budgeting (`seconds`)

Recommended approach for stable results:

1. Run `warmup_runs` (default 3) and discard.
2. For measured runs:
   - repeat deterministic invocations (`iters_per_run = 10_000`) until the elapsed time budget is reached.
   - collect per-run MIPS to compute variance.

This avoids needing to forcibly interrupt guest execution mid-loop.

---

## Perf export integration

Include results in `window.aero.perf.export()`:

```ts
type AeroPerfExport = {
  // ... other perf fields ...
  benchmarks: {
    guest_cpu: {
      iters_per_run: number;
      warmup_runs: number;
      measured_runs: number;
      results: Array<{
        variant: GuestCpuBenchVariant;
        mode: GuestCpuMode;
        mips_mean: number;
        mips_stddev: number;
        mips_min: number;
        mips_max: number;
        expected_checksum: string;
        observed_checksum: string;
      }>;
    };
  };
};
```

The export should be structured to allow baseline comparison in CI (PF-009).

---

## Playwright scenario: `guest_cpu`

Add a benchmark runner scenario `guest_cpu` (Playwright) that:

 - runs headless
 - calls `window.aero.bench.runGuestCpuBench(...)`
 - writes the exported perf blob to the benchmark artifact directory
 - compares against baseline and fails on large regressions

### PR (fast) subset

Run a small subset with short budgets (example):

 - `alu64` in `interpreter` for `0.25s`
 - `branch_pred64` in `interpreter` for `0.25s`

### Nightly (full) suite

Run all variants and all available modes with longer budgets (example):

 - `seconds = 1.0` per variant/mode
 - `warmup_runs = 3`, `measured_runs >= 10`

### Baseline comparison

Compare `mips_mean` per `(variant, mode)`:

 - PR threshold: e.g. fail if regression > 10% (configurable)
 - nightly threshold: tighter thresholds + trend tracking

---

## Correctness guardrails

The guest CPU bench **must fail on checksum mismatches**:

 - If `observed_checksum != expected_checksum`, throw an error and include:
   - variant + mode
   - expected vs observed
   - a short hint (likely an emulator correctness regression)

This prevents “fast but wrong” changes (especially in JIT) from silently improving benchmark numbers.
