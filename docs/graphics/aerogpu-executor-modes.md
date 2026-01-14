# AeroGPU executor modes (canonical `aero_machine`)

This document describes how the **canonical machine** (`crates/aero-machine`) can drive AeroGPU
submission fences under different host integration styles.

It is intended as an integration checklist for:

- the browser/WASM runtime (out-of-process GPU worker), and
- native/headless integration tests.

## Background

When `MachineConfig::enable_aerogpu = true`, `aero_machine::Machine` exposes the canonical AeroGPU
PCI function (`A3A0:0001` at `00:07.0`).

Notes:

- `enable_aerogpu` requires `MachineConfig::enable_pc_platform = true` (the PCI bus must be present).
- `enable_aerogpu` is mutually exclusive with `enable_vga` (standalone VGA/VBE device model).

The machine implements:

- BAR1-backed VRAM (including legacy VGA/VBE decode for boot display),
- BAR0 MMIO registers (ring/fence transport, scanout/cursor state, vblank timing + IRQ plumbing),
- submission capture (`Machine::aerogpu_drain_submissions`) for host-driven execution.

The guest submits work via the BAR0 ring. Each submission can optionally signal a fence; the Windows
driver uses fence forward progress (and vblank pacing) for correctness and DWM/Aero stability.

## Modes

### 1) Default mode: no-op executor (bring-up)

By default, `aero-machine` **does not execute** the AeroGPU `AEROGPU_CMD` stream. To avoid deadlocks
in early bring-up, submissions are treated as **no-op** and fences are completed so the guest can
continue.

If scanout/vblank timing is enabled and a submission contains a `PRESENT` with the `VSYNC` flag, the
device model may **pace fence completion until the next vblank tick** (even though it is otherwise
not executing the command stream). This is important for Win7/DWM stability expectations.

This mode is useful when you only need:

- correct PCI identity / BAR wiring,
- scanout/vblank register behavior,
- driver enumeration and initialization without a real renderer.

You can still call `Machine::aerogpu_drain_submissions()` in this mode to inspect/log decoded
submissions; it does **not** change fence completion behavior on native hosts. (In the browser,
`crates/aero-wasm` intentionally enables the submission bridge when draining so fence semantics are
explicit.)

### 2) Submission bridge: out-of-process executor mode

For browser/WASM integrations (or any host that executes command streams out-of-process), enable the
submission bridge:

- `Machine::aerogpu_enable_submission_bridge()`

Then drive execution using:

1. `Machine::aerogpu_drain_submissions()` to retrieve newly-decoded submissions (`cmd_stream`,
   `alloc_table`, `signal_fence`, etc).
2. Execute those submissions externally (e.g. in a GPU worker).
3. When complete, call `Machine::aerogpu_complete_fence(signal_fence)` so the guest observes forward
   progress (fence page + IRQ state updates).

Notes:

- The `crates/aero-wasm` exports (`aerogpu_drain_submissions` / `aerogpu_complete_fence`) enable the
  submission bridge automatically to keep the contract explicit for browser code.
- With the submission bridge enabled, the host controls **when work is executed**, but the device
  model still owns the **guest-facing fence pacing contract**:
  - once the host reports `signal_fence` completion via `Machine::aerogpu_complete_fence`, fences
    corresponding to vsync `PRESENT` packets complete on the **next vblank** (and block later
    immediate fences until then).
  - hosts should generally report completion as soon as execution finishes, rather than trying to
    “fake vsync” by delaying `submit_complete` based on host tick cadence.

### 3) In-process backend: native/headless executor mode

For native integration tests (or headless hosts) that want the device model to drive fence progress
without an external worker, install an in-process backend:

- `Machine::aerogpu_set_backend_immediate()` — completes fences synchronously, performs no rendering
  (headless-friendly).
- `Machine::aerogpu_set_backend_wgpu()` — feature-gated (`aerogpu-wgpu-backend`), wgpu-backed
  execution for end-to-end tests.

Installing an in-process backend is mutually exclusive with the submission bridge.

## Relevant tests

These tests exercise the executor-mode contracts:

```bash
# Submission bridge behavior (drain submissions + host fence completion)
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_submission_bridge --locked

# Fence completion gating / backend switching behavior
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_complete_fence_gating --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_deferred_fence_completion --locked

# Default-mode vblank pacing for VSYNC presents
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_vsync_fence_pacing --locked
```
