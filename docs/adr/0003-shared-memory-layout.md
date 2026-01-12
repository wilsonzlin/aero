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

## Addendum: wasm32 linear memory contract (runtime vs guest RAM)

Even when guest RAM is "backed by `WebAssembly.Memory`", **that same linear memory is also used by the Rust/WASM runtime**:

- stack
- Rust heap allocations (e.g. `Vec`, `String`, wasm-bindgen shims)
- static data / TLS

Therefore **guest physical address 0 cannot map to linear memory offset 0**.

### Strategy (implementable today): fixed runtime-reserved region at low addresses

We reserve a fixed, page-aligned region at the bottom of wasm linear memory for the runtime:

- `runtime_reserved` = **128 MiB**
- `guest_base` = `align_up(runtime_reserved, 64KiB)` (currently also 128 MiB)
- `guest_size` is clamped so that:
  - it fits within wasm32's 4 GiB linear-memory maximum: `guest_size ≤ 4GiB - guest_base`
  - it does **not** overlap the fixed PCI MMIO aperture used by the web I/O worker:
    `guest_size ≤ PCI_MMIO_BASE` (currently `PCI_MMIO_BASE = 0xE0000000`, i.e. 3.5 GiB)

The guest RAM mapping is:

```
wasm linear memory (0..4GiB)

0                         guest_base                     guest_base+guest_size
│-------------------------│------------------------------│---------------------│
│  runtime reserved       │  guest RAM (paddr 0..)       │ (unused / none)     │
│  (stack/heap/statics)   │                              │                     │
│-------------------------│------------------------------│---------------------│
```

### Addendum: guest physical address map (RAM vs PCI MMIO)

Independently from the **wasm linear memory** layout, the emulator needs a consistent **32-bit guest physical address** map so PCI MMIO BAR space cannot overlap guest RAM:

```
guest physical address space (32-bit)

0                          guest_size                    PCI_MMIO_BASE          4GiB
│--------------------------│-----------------------------│----------------------│
│ guest RAM                │ (reserved / unmapped hole)  │ PCI MMIO aperture    │
│                          │                             │ (PCI BARs live here) │
│--------------------------│-----------------------------│----------------------│
```

The web runtime enforces this by clamping `guest_size` to `<= PCI_MMIO_BASE` in both:

- Rust/WASM layout contract: `crates/aero-wasm/src/lib.rs` (`guest_ram_layout`)
- TS layout mirror used by the coordinator: `web/src/runtime/shared_layout.ts` (`computeGuestRamLayout`)

Contract:

- Guest physical address `paddr` maps to linear address `guest_base + paddr`.
- JS/TS code must bounds-check guest accesses against `[0, guest_size)` and reject anything outside.
- The coordinator stores `{ guest_base, guest_size }` into the control/status `SharedArrayBuffer` so all workers (TS + WASM) agree on the mapping.
- The WASM build uses a **bounded global allocator** so Rust heap allocations cannot grow past `runtime_reserved` and silently corrupt guest RAM.
- The WASM build links with `wasm-ld --stack-first` so the stack stays at low addresses; the stack must fit within `runtime_reserved`.
- The WASM build reserves a tiny tail guard at the *end* of the runtime-reserved region so the web runtime can safely use a deterministic scratch word for JS↔WASM memory wiring probes (without overlapping real Rust heap allocations).
  - Current tail guard size: **64 bytes**.
  - The JS-side probe uses a small **16-word (64 byte)** context-based window (to reduce cross-worker races), which must fit inside the tail guard.
  - See `crates/aero-wasm/src/runtime_alloc.rs` (`HEAP_TAIL_GUARD_BYTES`) and `web/src/runtime/wasm_memory_probe.ts`.

Reference implementation:

- Shared-memory segment allocation + layout computation: [`web/src/runtime/shared_layout.ts`](../../web/src/runtime/shared_layout.ts)
- WASM-exported layout API: `crates/aero-wasm/src/lib.rs` (`guest_ram_layout`)
- WASM build flags (imported memory, max memory, stack placement): [`web/scripts/build_wasm.mjs`](../../web/scripts/build_wasm.mjs)
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
