# ADR 0003: Shared memory layout (multiple SABs; WASM 4 GiB constraint)

## Context

Early designs often assume a single, monolithic `SharedArrayBuffer` large enough to hold:

- Guest physical RAM (up to ~4 GiB), *plus*
- Command/event rings, device state, framebuffers, audio rings, etc.

In practice, **WebAssembly MVP linear memory is 32-bit indexed and capped at 4 GiB**. For a threaded WASM build, that linear memory is itself backed by a `SharedArrayBuffer`, which means:

- You cannot rely on a single “5 GiB+” shared buffer to contain *everything*.
- Even a full 4 GiB guest RAM allocation leaves no address space for in-WASM control data.

## Decision

Use **multiple shared buffers** with a clear separation between guest RAM and host/control buffers:

1. **One `WebAssembly.Memory` (shared when threaded)** for:
   - Guest physical memory (primary consumer).
   - A small amount of in-WASM control/state as needed.
   - Total size must remain **≤ 4 GiB**.

2. **Separate `SharedArrayBuffer` instances** for:
   - Inter-worker command/event rings.
   - Status flags / atomics-based signaling.
   - Audio ring buffers (including AudioWorklet integration).
   - Optional CPU↔GPU staging buffers or debug/profiling buffers.

The guiding rule: **preserve WASM address space for guest RAM**, and keep large or host-only buffers outside the WASM linear memory unless there is a measured performance need to place them inside.

Reference implementation:

- Shared-memory segment allocation (control SAB + guest `WebAssembly.Memory`): [`web/src/runtime/shared_layout.ts`](../../web/src/runtime/shared_layout.ts)
- IPC protocol (binary rings + atomics contracts): [`docs/ipc-protocol.md`](../ipc-protocol.md)

## Alternatives considered

1. **Monolithic 5 GiB+ `SharedArrayBuffer`**
   - Pros: simple addressing model (one base pointer).
   - Cons: not compatible with the WASM 4 GiB limit; fragile across browsers due to buffer size limits.

2. **Banked guest RAM across multiple SABs**
   - Pros: could exceed 4 GiB guest RAM without `memory64`.
   - Cons: expensive address translation and bounds checks on every memory access; hard to integrate cleanly with WASM without multi-memory.

3. **Wait for `memory64` / multi-memory**
   - Pros: cleaner long-term story for >4 GiB guests.
   - Cons: not available/portable enough to be the baseline today.

## Consequences

- The host must manage multiple buffers and pass them to the relevant workers.
- Some subsystems may need explicit copy/staging steps between WASM memory and out-of-WASM SABs.
- Guest RAM capacity becomes a tuning knob: practical configurations may target **2–3 GiB guest RAM** to leave headroom for emulator state, depending on browser limits and device memory.
