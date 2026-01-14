# Graphics status (Windows 7 UX)

This is the **single authoritative status doc** for the graphics stack.
It tracks what is **implemented in-tree today** vs what is still **missing** to reach a “Windows 7 feels usable” experience (boot → desktop → DWM/Aero + apps).

Legend:

- `[x]` = implemented (exists in-tree and has tests)
- `[~]` = partial / stubbed / exists in an alternate stack (see notes)
- `[ ]` = missing / not wired / not validated end-to-end

Coordination note:

- For mapping from “legacy agent scratchpad task IDs” (SM3/DXBC/shared-surface) to the current
  in-tree implementations/tests, see
  [`docs/graphics/task-489-sm3-dxbc-sharedsurface-audit.md`](./task-489-sm3-dxbc-sharedsurface-audit.md).

## Read first (architecture + contracts)

- [`docs/04-graphics-subsystem.md`](../04-graphics-subsystem.md) — architecture overview
- [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md) — canonical AeroGPU PCI identity contract
- [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md) — required VGA/VBE compatibility + boot→WDDM scanout handoff
- [`docs/graphics/win7-vblank-present-requirements.md`](./win7-vblank-present-requirements.md) — Win7 vblank/present timing contract (DWM stability)

> Scope note: the repo currently contains both:
>
> - a **canonical machine integration** (`crates/aero-machine`, surfaced to the browser via `crates/aero-wasm`), and
> - a sandbox/legacy “monolithic emulator” crate (`crates/emulator`).
>
> This doc calls out both where it matters, but treats `aero-machine` as the canonical integration surface unless explicitly marked “legacy/sandbox”.
>
> See also: [`docs/21-emulator-crate-migration.md`](../21-emulator-crate-migration.md) (explicit `crates/emulator` → canonical stack plan + deletion targets).

---

## At-a-glance matrix

| Area | Status | Where to look |
|---|---|---|
| Boot display (VGA text + VBE LFB) | `[x]` | [`crates/aero-gpu-vga/`](../../crates/aero-gpu-vga/) wired into [`crates/aero-machine/`](../../crates/aero-machine/) |
| AeroGPU ABI (C headers + Rust/TS mirrors + ABI tests) | `[x]` | [`drivers/aerogpu/protocol/`](../../drivers/aerogpu/protocol/) + [`emulator/protocol/aerogpu/`](../../emulator/protocol/aerogpu/) |
| AeroGPU PCI identity + BAR0/BAR1 transport + ring decode (submission bridge) | `[~]` | [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) + [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) |
| AeroGPU sandbox device model + executor (legacy integration surface) | `[~]` | [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs) + [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs) |
| Scanout shared-memory contracts | `[x]` | [`crates/aero-shared/src/`](../../crates/aero-shared/src/) + [`web/src/ipc/`](../../web/src/ipc/) |
| D3D9 translation/execution (subset) | `[~]` | [`crates/aero-d3d9/`](../../crates/aero-d3d9/) + [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs) + [`docs/graphics/d3d9-sm2-sm3-shader-translation.md`](./d3d9-sm2-sm3-shader-translation.md) |
| D3D10/11 translation/execution (subset; VS/PS/CS + GS compute-prepass (minimal subset for point-list and triangle-list draws; other cases use synthetic expansion)) | `[~]` | [`crates/aero-d3d11/`](../../crates/aero-d3d11/) |
| Web presenters/backends (WebGPU + WebGL2) | `[x]` | [`web/src/gpu/`](../../web/src/gpu/) |
| End-to-end Win7 WDDM + accelerated rendering in the **canonical browser machine** | `[ ]` | See [7) Critical path integration gaps](#7-current-critical-path-integration-gaps-factual) |

---

## 1) Boot display (VGA text, VBE LFB)

Win7 UX goal: the **same virtual GPU** should provide both boot VGA/VBE output and the later WDDM scanout path (no device swap).

### Implemented today: standalone VGA/VBE device (`crates/aero-gpu-vga`)

Status checklist:

- [x] VGA register file emulation (sequencer/graphics/attribute/CRTC)
- [x] Text mode rendering (80×25) with built-in bitmap font + cursor
- [x] Mode 13h rendering (320×200×256, chain-4)
- [x] Bochs/QEMU-style VBE (`VBE_DISPI`) register interface + linear framebuffer backing

Code pointers:

- [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs) (`VgaDevice`, VBE LFB at configurable base; legacy default `SVGA_LFB_BASE`)

Test pointers:

- [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs) (module `tests`)
  - `text_mode_golden_hash`
  - `mode13h_golden_hash`
  - `vbe_linear_framebuffer_write_shows_up_in_output`

CI/regression command:

```bash
# Runs the boot-display stack end-to-end (VGA/VBE device model + BIOS INT10 + machine wiring).
bash ./scripts/ci/run-vga-vbe-tests.sh
```

### Wired into the canonical machine (`crates/aero-machine`)

When `MachineConfig::enable_vga=true`, `aero_machine::Machine` wires the VGA/VBE device model for boot display.

Note: when the PC platform is enabled (`enable_pc_platform=true`), the VBE LFB is mapped directly at the configured LFB base inside the ACPI-reported PCI MMIO window (no dedicated PCI VGA stub).

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  - `MachineConfig::enable_vga` docs (port + address ranges)
  - `Machine::reset` (device wiring)
  - `Machine::display_present` / `display_framebuffer` / `display_resolution` (host-facing RGBA8888 snapshot)

Test pointers:

- [`crates/aero-machine/tests/boot_int10_vbe_sets_mode.rs`](../../crates/aero-machine/tests/boot_int10_vbe_sets_mode.rs) (INT 10h VBE mode set)
- [`crates/aero-machine/tests/boot_int10_active_page_renders_text.rs`](../../crates/aero-machine/tests/boot_int10_active_page_renders_text.rs) (text mode active-page behavior)
- [`crates/aero-machine/tests/vga_vbe_lfb_pci.rs`](../../crates/aero-machine/tests/vga_vbe_lfb_pci.rs) (VBE LFB reachable via the PC platform MMIO mapping)

### Implemented today: AeroGPU boot-display foundation (`enable_aerogpu=true`)

`MachineConfig::enable_aerogpu=true` disables the standalone VGA device and instead provides:

- [x] BAR1-backed VRAM
- [x] legacy VGA window decode (`0xA0000..0xBFFFF`) backed by BAR1 VRAM (mode-dependent aliasing):
  - VBE inactive: `0xA0000..0xBFFFF` ↔ `VRAM[0x00000..0x1FFFF]`
  - VBE active:
    - `0xA0000..0xAFFFF` becomes the VBE banked window into `VRAM[VBE_LFB_OFFSET + bank*64KiB + off]`
    - `0xB0000..0xBFFFF` remains `VRAM[0x10000..0x1FFFF]`
- [x] BIOS VBE LFB base set into BAR1: `PhysBasePtr = BAR1_BASE + VBE_LFB_OFFSET` (`VBE_LFB_OFFSET = 0x40000`, protocol: `AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`)
- [x] host-side presentation fallback when VGA is disabled:
  - If WDDM scanout0 has been claimed:
    - `SCANOUT0_ENABLE=1`: present the WDDM scanout framebuffer
    - `SCANOUT0_ENABLE=0`: present a blank frame (WDDM ownership is sticky; no fallback to legacy until reset)
  - Otherwise, present in priority order:
    - VBE LFB (from BIOS state)
    - VGA mode 13h (320×200×256) (from BIOS state)
    - text mode (scan `0xB8000`)

Implementation note: `SCANOUT0_ENABLE` is treated as a **visibility toggle**, not an ownership release.
Clearing it (`SCANOUT0_ENABLE=0`) blanks output (and stops vblank pacing / flushes vsync-paced fences), but keeps the sticky
`wddm_scanout_active` latch held so legacy VGA/VBE cannot reclaim scanout until reset.

- Ownership latch + disable handling: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs)
  (`AEROGPU_MMIO_REG_SCANOUT0_ENABLE`, `wddm_scanout_active`) + unit test `scanout_disable_keeps_wddm_ownership_latched`.
- Host-side presentation behavior when disabled: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  (`display_present_aerogpu_scanout`).

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  - `MachineConfig::enable_aerogpu` docs
  - `Machine::display_present` + `display_present_aerogpu_*` helpers

Test pointers:

- [`crates/aero-machine/tests/boot_int10_aerogpu_vbe_115_sets_mode.rs`](../../crates/aero-machine/tests/boot_int10_aerogpu_vbe_115_sets_mode.rs)
- [`crates/aero-machine/tests/aerogpu_text_mode_scanout.rs`](../../crates/aero-machine/tests/aerogpu_text_mode_scanout.rs)
- [`crates/aero-machine/tests/aerogpu_vbe_lfb_base_bar1.rs`](../../crates/aero-machine/tests/aerogpu_vbe_lfb_base_bar1.rs)
- [`crates/aero-machine/tests/aerogpu_vbe_clear_fastpath.rs`](../../crates/aero-machine/tests/aerogpu_vbe_clear_fastpath.rs) (VBE clear/no-clear semantics + preserve pre-LFB VRAM)

### Missing / still required (boot → WDDM)

- [~] Boot framebuffer → WDDM scanout handoff: host-facing `Machine::display_present` prefers WDDM scanout once scanout0 is claimed by a valid configuration (and enabled), but this path still needs end-to-end validation in the browser runtime and shared-scanout publication (see Section 7).
  - Code: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`display_present`, `display_present_aerogpu_scanout`)
  - Contract/design: [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md)

---

## 2) AeroGPU protocol + device model (ABI + host-side processors)

### Canonical ABI “source of truth” (C headers)

The canonical AeroGPU ABI is defined in C headers under `drivers/aerogpu/protocol/`.

Code pointers:

- [`drivers/aerogpu/protocol/`](../../drivers/aerogpu/protocol/)
  - [`drivers/aerogpu/protocol/aerogpu_pci.h`](../../drivers/aerogpu/protocol/aerogpu_pci.h) (PCI IDs, MMIO register map, feature bits)
  - [`drivers/aerogpu/protocol/aerogpu_ring.h`](../../drivers/aerogpu/protocol/aerogpu_ring.h) (submission ring + fence page)
  - [`drivers/aerogpu/protocol/aerogpu_cmd.h`](../../drivers/aerogpu/protocol/aerogpu_cmd.h) (ACMD packet stream)
  - [`drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`](../../drivers/aerogpu/protocol/aerogpu_wddm_alloc.h), [`drivers/aerogpu/protocol/aerogpu_escape.h`](../../drivers/aerogpu/protocol/aerogpu_escape.h) (WDDM-facing structs)

### Rust + TypeScript mirrors (`aero-protocol` crate)

The Rust/TS mirrors live in the **`aero-protocol`** crate, located at `emulator/protocol/`:

- [`emulator/protocol/Cargo.toml`](../../emulator/protocol/Cargo.toml) (package name: `aero-protocol`)
- [`emulator/protocol/aerogpu/`](../../emulator/protocol/aerogpu/) (Rust `*.rs` and TS `*.ts` mirrors)

Test pointers (ABI conformance / drift detection):

- [`emulator/protocol/tests/aerogpu_abi.rs`](../../emulator/protocol/tests/aerogpu_abi.rs) (Rust sizes/offsets/consts)
- [`emulator/protocol/tests/aerogpu_abi.test.ts`](../../emulator/protocol/tests/aerogpu_abi.test.ts) (TS sizes/offsets/consts)
- [`emulator/protocol/tests/aerogpu_pci_id_conformance.rs`](../../emulator/protocol/tests/aerogpu_pci_id_conformance.rs)

### Device models

#### Canonical machine (`crates/aero-machine`): BAR0/BAR1 + backend boundary (submission bridge / in-process backends)

`MachineConfig::enable_aerogpu=true` exposes the canonical identity:

- [x] `VID:DID = A3A0:0001`
- [x] BDF `00:07.0`
- [x] BAR1 VRAM + legacy VGA window aliasing
- [~] BAR0 MMIO register block + ring/fence transport + scanout/vblank + cursor + error-info registers
  - Ring processing decodes `aerogpu_ring` submissions and can capture `AEROGPU_CMD` payloads into a bounded queue
    for host-driven execution (`Machine::aerogpu_drain_submissions`).
  - Fence forward-progress policy is selectable:
    - default (no backend, submission bridge disabled): fences complete automatically (bring-up / no-op execution),
      with optional vblank pacing when vblank is active and the submission contains a vsync present.
    - submission bridge enabled (`Machine::aerogpu_enable_submission_bridge`): fences are deferred until the host reports completion (`Machine::aerogpu_complete_fence`).
      - The device model does not apply its own vblank pacing in this mode; the external executor (e.g. browser GPU worker)
        is responsible for any vsync completion policy before reporting fence completion.
    - in-process backend installed: fences complete when the backend reports completions (see below).
  - Error-info latches are implemented (ABI 1.3+) behind `AEROGPU_FEATURE_ERROR_INFO` (`AEROGPU_MMIO_REG_ERROR_*` + `AEROGPU_IRQ_ERROR`).

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`MachineConfig::enable_aerogpu`, BAR1 aliasing, display helpers)
- [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) (BAR0 register model, ring decode, submission bridge + backend boundary, vblank/scanout/cursor)

Test pointers:

- [`crates/aero-machine/tests/pci_display_bdf_contract.rs`](../../crates/aero-machine/tests/pci_display_bdf_contract.rs) (BDF contract)
- [`crates/aero-machine/tests/machine_aerogpu_pci_identity.rs`](../../crates/aero-machine/tests/machine_aerogpu_pci_identity.rs)
- [`crates/aero-machine/tests/aerogpu_ring_noop_fence.rs`](../../crates/aero-machine/tests/aerogpu_ring_noop_fence.rs) (default “bring-up” completion policy + drains submissions)
- [`crates/aero-machine/tests/aerogpu_submission_bridge.rs`](../../crates/aero-machine/tests/aerogpu_submission_bridge.rs) (submission bridge requires host fence completion)
- [`crates/aero-machine/tests/aerogpu_immediate_backend_completes_fence.rs`](../../crates/aero-machine/tests/aerogpu_immediate_backend_completes_fence.rs) (in-process backend APIs)
- [`crates/aero-machine/tests/aerogpu_bar0_mmio_vblank.rs`](../../crates/aero-machine/tests/aerogpu_bar0_mmio_vblank.rs)

In-process backend APIs (native/tests):

- `Machine::aerogpu_set_backend_immediate()` / `Machine::aerogpu_set_backend_null()` (always available)
  - Test: `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_immediate_backend_completes_fence --locked`
- `Machine::aerogpu_set_backend_wgpu()` (feature-gated, native-only: `aero-machine/aerogpu-wgpu-backend`)
  - Build sanity: `bash ./scripts/safe-run.sh cargo test -p aero-machine --features aerogpu-wgpu-backend --locked`
  - End-to-end backend tests live in `aero-devices-gpu`:
    `bash ./scripts/safe-run.sh cargo test -p aero-devices-gpu --features wgpu-backend --test aerogpu_end_to_end --locked`

#### Canonical browser runtime (`crates/aero-wasm` + `web/`): submission bridge + GPU worker execution

The canonical browser integration runs:

- the guest-visible AeroGPU PCI/MMIO device model **in the CPU worker** (inside the `aero-wasm` `Machine`), and
- `AEROGPU_CMD` execution **in the GPU worker** (`web/src/workers/gpu-worker.ts`).

Message flow (high level):

1. Guest rings the AeroGPU doorbell (BAR0 MMIO), causing the in-process device model to decode ring entries.
2. CPU worker calls `Machine.aerogpu_drain_submissions()` (WASM export) and posts each submission to the coordinator:
   `kind: "aerogpu.submit"` (payload includes `cmdStream`, optional `allocTable`, and `signalFence`).
3. Coordinator buffers submissions until the GPU worker is READY, then forwards them to the GPU worker using the GPU
   protocol message `type: "submit_aerogpu"`.
4. GPU worker executes the command stream (TypeScript CPU executor and/or `aero-gpu-wasm` D3D9 path) and replies with
   `type: "submit_complete"` containing `completedFence`.
5. Coordinator forwards `kind: "aerogpu.complete_fence"` to the CPU worker, which calls the WASM export
   `Machine.aerogpu_complete_fence(fence: BigInt)` to update the guest-visible fence page + IRQ status.

Code pointers:

- WASM bridge exports (enables external-executor semantics on first drain):
  - [`crates/aero-wasm/src/lib.rs`](../../crates/aero-wasm/src/lib.rs) (`Machine::aerogpu_drain_submissions`, `Machine::aerogpu_complete_fence`)
- CPU worker drain + completion queue:
  - [`web/src/workers/machine_cpu.worker.ts`](../../web/src/workers/machine_cpu.worker.ts) (`drainAerogpuSubmissions`, `processPendingAerogpuFenceCompletions`)
- Coordinator routing/buffering + fence forwarding:
  - [`web/src/runtime/coordinator.ts`](../../web/src/runtime/coordinator.ts) (`forwardAerogpuSubmit`, `forwardAerogpuFenceComplete`)
  - [`web/src/runtime/coordinator.test.ts`](../../web/src/runtime/coordinator.test.ts) (“buffers aerogpu.submit…”)
- GPU worker executor + vsync completion policy:
  - [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts) (`handleSubmitAerogpu`, `enqueueAerogpuSubmitComplete`)

Test commands:

```bash
# Rust: submission bridge semantics (host must complete fences)
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_submission_bridge --locked

# Web unit: coordinator message flow (aerogpu.submit -> submit_aerogpu -> submit_complete -> aerogpu.complete_fence)
AERO_TIMEOUT=600 AERO_MEM_LIMIT=32G bash ./scripts/safe-run.sh npm -w web run test:unit -- src/runtime/coordinator.test.ts

# Playwright: GPU worker ACMD execution + submit_complete semantics
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/web/gpu_submit_aerogpu.spec.ts
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/web/gpu_submit_aerogpu_vsync_completion.spec.ts
```

#### Legacy/sandbox (`crates/emulator`): separate device model + executor

A more complete AeroGPU PCI device model (including a real command execution path via the `gpu_worker` backends) exists in `crates/emulator`.

Code pointers:

- [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs)
- [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs)

Representative test pointers:

- [`crates/emulator/tests/aerogpu_d3d9_triangle_end_to_end.rs`](../../crates/emulator/tests/aerogpu_d3d9_triangle_end_to_end.rs)

#### Shared device-side library (`crates/aero-devices-gpu`): regs/ring/executor + portable PCI wrapper

The `crates/aero-devices-gpu` crate is the shared “device-side” home for:

- MMIO register constants + backing `AeroGpuRegs`,
- ring + fence page structs/helpers,
- the ring executor (doorbell processing, submission decode, fence tracking, vsync/vblank pacing), and
- a lightweight PCI device wrapper (`AeroGpuPciDevice`) that can be reused by multiple hosts.

Code pointers:

- [`crates/aero-devices-gpu/src/executor.rs`](../../crates/aero-devices-gpu/src/executor.rs)
- [`crates/aero-devices-gpu/src/pci.rs`](../../crates/aero-devices-gpu/src/pci.rs)
- [`crates/aero-devices-gpu/src/ring.rs`](../../crates/aero-devices-gpu/src/ring.rs)
- [`crates/aero-devices-gpu/src/regs.rs`](../../crates/aero-devices-gpu/src/regs.rs)

Test pointers:

- [`crates/aero-devices-gpu/tests/aerogpu_executor_decode.rs`](../../crates/aero-devices-gpu/tests/aerogpu_executor_decode.rs)
- [`crates/aero-devices-gpu/tests/aerogpu_pci_device.rs`](../../crates/aero-devices-gpu/tests/aerogpu_pci_device.rs)
- [`crates/aero-devices-gpu/tests/vram_bar1.rs`](../../crates/aero-devices-gpu/tests/vram_bar1.rs)
- Feature-gated wgpu end-to-end: [`crates/aero-devices-gpu/tests/aerogpu_end_to_end.rs`](../../crates/aero-devices-gpu/tests/aerogpu_end_to_end.rs) (run with `cargo test -p aero-devices-gpu --features wgpu-backend --test aerogpu_end_to_end`)

### Host-side processors/executors (wgpu/WebGPU)

The canonical “host-side” consumption of the AeroGPU command stream lives in `crates/aero-gpu/` and friends.

Code pointers:

- Protocol parsing:
  - [`crates/aero-gpu/src/protocol.rs`](../../crates/aero-gpu/src/protocol.rs) (`parse_cmd_stream`, `AeroGpuCmd`)
- Command processors:
  - [`crates/aero-gpu/src/command_processor.rs`](../../crates/aero-gpu/src/command_processor.rs)
  - [`crates/aero-gpu/src/command_processor_d3d9.rs`](../../crates/aero-gpu/src/command_processor_d3d9.rs)
  - [`crates/aero-gpu/src/protocol_d3d11.rs`](../../crates/aero-gpu/src/protocol_d3d11.rs)
- Executors:
  - [`crates/aero-gpu/src/aerogpu_executor.rs`](../../crates/aero-gpu/src/aerogpu_executor.rs) (minimal executor)
  - [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs) (D3D9-focused)
  - [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (D3D10/11-focused)

Test pointers:

- [`crates/aero-gpu/tests/`](../../crates/aero-gpu/tests/) (protocol + executor behavior)
  - Example: [`crates/aero-gpu/tests/aerogpu_ex_protocol.rs`](../../crates/aero-gpu/tests/aerogpu_ex_protocol.rs)

---

## 3) Scanout contracts (shared memory)

There are two distinct shared-memory contracts used between Rust/WASM and JS:

1. `ScanoutState`: a compact, lock-free descriptor of *where the “current scanout” lives* (guest paddr + geometry).
2. `SharedFramebuffer`: a double-buffered RGBA8 framebuffer used for CPU-produced frames (with optional dirty-tile bitsets).

### 3.1) `ScanoutState`

Status checklist:

- [x] Seqlock-style publish protocol (busy-bit in `generation`)
- [x] Explicit `source` enum (`LEGACY_TEXT`, `LEGACY_VBE_LFB`, `WDDM`)
- [x] TS mirror uses `Atomics.*` on an `Int32Array`

Code pointers:

- Rust: [`crates/aero-shared/src/scanout_state.rs`](../../crates/aero-shared/src/scanout_state.rs)
- TS mirror: [`web/src/ipc/scanout_state.ts`](../../web/src/ipc/scanout_state.ts)

Test pointers:

- Rust: [`crates/aero-shared/src/scanout_state.rs`](../../crates/aero-shared/src/scanout_state.rs) (unit + loom tests)
- TS: [`web/src/ipc/scanout_state.test.ts`](../../web/src/ipc/scanout_state.test.ts)

### 3.2) `SharedFramebuffer`

Status checklist:

- [x] Stable, aligned shared layout (`SharedFramebufferLayout`)
- [x] Atomic header protocol (`active_index`, `frame_seq`, `frame_dirty`, per-buffer seq)
- [x] Optional per-tile dirty bitset + rect extraction
- [x] TS mirror layout + dirty-rect logic

Code pointers:

- Rust: [`crates/aero-shared/src/shared_framebuffer.rs`](../../crates/aero-shared/src/shared_framebuffer.rs)
- TS mirror: [`web/src/ipc/shared-layout.ts`](../../web/src/ipc/shared-layout.ts)

Test pointers:

- Rust: [`crates/aero-shared/src/shared_framebuffer.rs`](../../crates/aero-shared/src/shared_framebuffer.rs) (unit + loom tests)
- TS: [`web/src/ipc/shared-layout.test.ts`](../../web/src/ipc/shared-layout.test.ts)

---

## 4) D3D9 stack (`crates/aero-d3d9*`)

### What exists today

The D3D9 implementation is split into:

- D3D9 shader parsing/translation primitives in `crates/aero-d3d9` (+ legacy parser in `crates/legacy/aero-d3d9-shader`)
- a D3D9-focused AeroGPU command executor in `crates/aero-gpu` consuming `aerogpu_cmd.h` packets
- [x] D3D9 half-pixel center convention (✅ Task 124 closed)
  - Translation (SM3-first path): [`crates/aero-d3d9/src/shader_translate.rs`](../../crates/aero-d3d9/src/shader_translate.rs) injects the `@group(3) @binding(0)` `HalfPixel` uniform + clip-space XY adjustment into translated vertex WGSL when `WgslOptions::half_pixel_center` is enabled (`inject_half_pixel_center_sm3_vertex_wgsl`).
  - Translation (legacy fallback path): [`crates/aero-d3d9/src/shader.rs`](../../crates/aero-d3d9/src/shader.rs) emits the same `HalfPixel` uniform + adjustment for the legacy token-stream translator when `WgslOptions::half_pixel_center` is enabled.
  - Execution: [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs) creates/binds the group(3) bind group and updates the uniform on `AeroGpuCmd::SetViewport`.
  - Test: [`crates/aero-gpu/tests/aerogpu_d3d9_half_pixel_center.rs`](../../crates/aero-gpu/tests/aerogpu_d3d9_half_pixel_center.rs) (`bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_d3d9_half_pixel_center --locked`)
- [x] SM3 derivatives (`dsx`/`dsy`) and gradient sampling (`texldd`) (✅ Task 216/217 closed)
  - Translation: [`crates/aero-d3d9/src/sm3/`](../../crates/aero-d3d9/src/sm3/) lowers derivatives to WGSL `dpdx`/`dpdy`.
  - Legacy-fallback translation: [`crates/aero-d3d9/src/shader.rs`](../../crates/aero-d3d9/src/shader.rs) also supports `dsx`/`dsy` for best-effort compatibility.
  - Tests: `crates/aero-d3d9/tests/sm3_wgsl.rs` (derivatives + `texldd`), `crates/aero-d3d9/src/tests.rs` (fallback path).
- [x] SM3 texture sampling + `texkill` semantics (✅ Tasks 401/402 closed)
  - `texld`/`texldp`/`texldb`/`texldd`/`texldl` lower to WGSL `textureSample*` variants, with texture/sampler binding emission and bind-layout mapping.
  - `texkill` lowers to `discard` when any component of the operand is `< 0`, preserving predication nesting.
  - Details + tests: [`docs/graphics/d3d9-sm2-sm3-shader-translation.md`](./d3d9-sm2-sm3-shader-translation.md)
- [x] Shader constant updates include int/bool registers (`SetShaderConstantsI` / `SetShaderConstantsB`)
  - Protocol: new D3D9 command stream opcodes in `drivers/aerogpu/protocol/aerogpu_cmd.h` (mirrored by `aero-protocol`).
  - Translation: shaders use stable `@group(0)` bindings for float/int/bool constant registers (bool regs are represented as `vec4<u32>` with `0/1` replicated across all lanes).
  - Execution: the D3D9 executor uploads float/int/bool constant data alongside other state.
  - Tests: `crates/aero-gpu/tests/aerogpu_d3d9_int_bool_constants.rs`, `aerogpu_d3d9_bool_constants.rs`, `aerogpu_d3d9_int_constants_dynamic.rs`, `aerogpu_d3d9_bool_constants_stage_isolation.rs`.
- [x] SM3 pixel shader `MISCTYPE` builtins: `misc0` (vPos) + `misc1` (vFace) (✅ Task 439 closed)
  - `misc0` (vPos) maps to WGSL `@builtin(position)` in [`FsIn.frag_pos`](../../crates/aero-d3d9/src/sm3/wgsl.rs), exposed to the shader body as `misc0: vec4<f32>`.
  - `misc1` (vFace) maps to WGSL `@builtin(front_facing)`, exposed as a D3D-style `misc1: vec4<f32>` where `face` is `+1` or `-1` replicated across all lanes.
  - Translation: [`crates/aero-d3d9/src/sm3/wgsl.rs`](../../crates/aero-d3d9/src/sm3/wgsl.rs)
  - Tests: [`crates/aero-d3d9/tests/sm3_wgsl.rs`](../../crates/aero-d3d9/tests/sm3_wgsl.rs)
    - `wgsl_ps3_vpos_misctype_builtin_compiles`
    - `wgsl_ps3_vface_misctype_builtin_compiles`
- [x] SM3 pixel shader depth output (`oDepth`) (✅ Task 468 closed)
  - D3D9 `oDepth` / `RegFile::DepthOut` lowers to WGSL `@builtin(frag_depth)` and is assigned from `oDepth.x`.
  - Translation: [`crates/aero-d3d9/src/sm3/wgsl.rs`](../../crates/aero-d3d9/src/sm3/wgsl.rs)
  - Test: [`crates/aero-d3d9/tests/sm3_wgsl_depth_out.rs`](../../crates/aero-d3d9/tests/sm3_wgsl_depth_out.rs)
    - `wgsl_ps30_writes_odepth_emits_frag_depth`
- [x] D3D9 shader translation cache (in-memory + WASM-only persistent cache)
  - In-memory cache: [`crates/aero-d3d9/src/shader_translate.rs`](../../crates/aero-d3d9/src/shader_translate.rs) (`ShaderCache`)
  - Persistent cache (WASM): [`crates/aero-d3d9/src/runtime/shader_cache.rs`](../../crates/aero-d3d9/src/runtime/shader_cache.rs) + browser backing store [`web/gpu-cache/persistent_cache.ts`](../../web/gpu-cache/persistent_cache.ts)
  - Executor wiring: [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs)
  - Test (WASM): [`crates/aero-gpu/tests/wasm/aerogpu_d3d9_shader_cache_wasm.rs`](../../crates/aero-gpu/tests/wasm/aerogpu_d3d9_shader_cache_wasm.rs)

Code pointers:

- Translator + runtime primitives:
  - [`crates/aero-d3d9/`](../../crates/aero-d3d9/)
  - Legacy standalone shader parser (not used by the runtime): [`crates/legacy/aero-d3d9-shader/`](../../crates/legacy/aero-d3d9-shader/)
- AeroGPU D3D9 executor:
  - [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs)

Representative test pointers:

- Translator tests: [`crates/aero-d3d9/src/tests.rs`](../../crates/aero-d3d9/src/tests.rs)
- Executor tests: [`crates/aero-gpu/tests/`](../../crates/aero-gpu/tests/)
  - [`crates/aero-gpu/tests/aerogpu_d3d9_triangle.rs`](../../crates/aero-gpu/tests/aerogpu_d3d9_triangle.rs)
  - [`crates/aero-gpu/tests/aerogpu_d3d9_fixedfunc_triangle.rs`](../../crates/aero-gpu/tests/aerogpu_d3d9_fixedfunc_triangle.rs)
- Guest-side Win7 tests live under [`drivers/aerogpu/tests/win7/`](../../drivers/aerogpu/tests/win7/) (see [`drivers/aerogpu/tests/win7/README.md`](../../drivers/aerogpu/tests/win7/README.md)), including fixed-function regression coverage (`d3d9_fixedfunc_wvp_triangle`, `d3d9_fixedfunc_textured_wvp`, `d3d9_fixedfunc_lighting_directional`).

Known gaps / limitations (enforced by code):

- Shader translation rejects unsupported tokens/opcodes:
  - [`crates/aero-d3d9/src/shader.rs`](../../crates/aero-d3d9/src/shader.rs) (`ShaderError::Unsupported*`)
- SM3 IR builder rejects some control-flow / addressing forms:
  - [`crates/aero-d3d9/src/sm3/ir_builder.rs`](../../crates/aero-d3d9/src/sm3/ir_builder.rs)

For Win7 D3D9Ex/DWM context:

- [`docs/16-d3d9ex-dwm-compatibility.md`](../16-d3d9ex-dwm-compatibility.md)
- [`docs/graphics/win7-d3d9ex-umd-minimal.md`](./win7-d3d9ex-umd-minimal.md)

---

## 5) D3D10/11 stack (`crates/aero-d3d11`)

### What exists today

`crates/aero-d3d11` contains:

1. DXBC SM4/SM5 decode + WGSL translation (VS/PS/CS today; plus GS/HS/DS `stage_ex` plumbing; a minimal SM4 GS DXBC→WGSL compute translator exists and is executed for point-list and triangle-list draws (`Draw` and `DrawIndexed`); HS/DS translation/execution is not implemented).
2. A wgpu-backed executor for the AeroGPU command stream (`aerogpu_cmd.h`).

Code pointers:

- Translation:
  - [`crates/aero-d3d11/src/shader_translate.rs`](../../crates/aero-d3d11/src/shader_translate.rs)
  - [`crates/aero-d3d11/src/sm4/`](../../crates/aero-d3d11/src/sm4/)
- Command execution:
  - [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs)

Representative test pointers:

- [`crates/aero-d3d11/tests/aerogpu_cmd_smoke.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_smoke.rs)
- [`crates/aero-d3d11/tests/aerogpu_cmd_textured_triangle.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_textured_triangle.rs)
- Compute translation/execution: [`crates/aero-d3d11/tests/d3d11_runtime_compute_dispatch.rs`](../../crates/aero-d3d11/tests/d3d11_runtime_compute_dispatch.rs), [`crates/aero-d3d11/tests/shader_translate_compute.rs`](../../crates/aero-d3d11/tests/shader_translate_compute.rs)
- GS compute-prepass plumbing (synthetic expansion bring-up): [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs), [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_vertex_pulling.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_vertex_pulling.rs), [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_primitive_id.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_primitive_id.rs)
- GS translator unit tests: [`crates/aero-d3d11/tests/gs_translate.rs`](../../crates/aero-d3d11/tests/gs_translate.rs)
- GS prepass execution tests (point-list and triangle-list, translated SM4 subset):
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_restart_strip.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_pointlist_draw_indexed.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_output_topology_pointlist.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_output_topology_pointlist.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_trianglelist_emits_triangle.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_cbuffer_b0_translated_prepass.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_cbuffer_b0_translated_prepass.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_line_strip_output.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_line_strip_output.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_gs_emulation_passthrough.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_gs_emulation_passthrough.rs)
  - [`crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs)
- Guest-side Win7 tests live under [`drivers/aerogpu/tests/win7/`](../../drivers/aerogpu/tests/win7/) (see e.g. `d3d10_*`, `d3d11_*`)

Known gaps / limitations (enforced by code/tests):

- Geometry shaders require compute-based emulation on WebGPU (no GS stage):
  - The executor routes draws through a compute prepass (expanded buffers + indirect args) followed by a normal render pass:
    - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`gs_hs_ds_emulation_required`, `exec_draw_with_compute_prepass`)
  - The compute prepass includes a built-in WGSL path that emits deterministic synthetic triangle geometry for bring-up/fallback (see `GEOMETRY_PREPASS_CS_WGSL`).
  - For a small supported subset of geometry shaders with point-list and triangle-list input, the executor translates GS DXBC→WGSL compute at create time and can execute it as the prepass for point-list and triangle-list draws (`Draw` and `DrawIndexed`) (see `exec_geometry_shader_prepass_pointlist` and `exec_geometry_shader_prepass_trianglelist`):
    - Translator: [`crates/aero-d3d11/src/runtime/gs_translate.rs`](../../crates/aero-d3d11/src/runtime/gs_translate.rs)
    - Translator tests: [`crates/aero-d3d11/tests/gs_translate.rs`](../../crates/aero-d3d11/tests/gs_translate.rs)
  - Strip output expansion helpers for `CutVertex` / `RestartStrip` semantics:
    - Reference implementation: [`crates/aero-d3d11/src/runtime/strip_to_list.rs`](../../crates/aero-d3d11/src/runtime/strip_to_list.rs)
    - Unit tests: `crates/aero-d3d11/src/runtime/strip_to_list.rs` (module `tests`)
- GS/HS/DS shader objects can be created/bound (the command stream binds these stages via
  `BIND_SHADERS`; newer streams may append `{gs,hs,ds}` handles after the stable 24-byte prefix—when
  present the appended handles are authoritative). HS/DS currently compile to minimal compute shaders
  for state tracking and are not executed. GS shaders attempt translation to a compute prepass at
  create time:
  - If translation succeeds, only point-list and triangle-list draws (`Draw` and `DrawIndexed`) currently execute translated GS DXBC; other cases use synthetic expansion (guest GS DXBC does not execute).
  - If translation fails, draws with that GS bound currently return a clear “geometry shader not supported” error.
    - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`exec_create_shader_dxbc`, `from_aerogpu_u32_with_stage_ex`)
    - Tests: [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs)
- Current GS translator limitations / initial target subset (non-exhaustive):
  - No adjacency end-to-end (`*_ADJ`)
  - No multi-stream output (`emit_stream` / `cut_stream`); only stream 0 is supported
  - Output topology (GS→WGSL compute translator): `pointlist`, `linestrip`, `triangle_strip`
    - `linestrip` is expanded into an indexed **line list**
    - `triangle_strip` is expanded into an indexed **triangle list**
    - Note: executor wiring is still partial; the end-to-end translated-GS prepass path is currently
      only exercised by point-list and triangle-list draws. For that path, the expanded draw topology is derived from
      the GS output topology kind (`PointList`/`LineList`/`TriangleList`, with strips expanded to
      lists).
  - GS instancing (`dcl_gsinstancecount` / `[instance(n)]`, `SV_GSInstanceID`) is supported:
    - Test: [`crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_gs_instance_count.rs)
  - No stream-out (SO / transform feedback)
- Tessellation (Hull/Domain) emulation is bring-up only:
  - Patchlist topology routes draws through the compute-prepass expansion path
    (see `gs_hs_ds_emulation_required`, `exec_draw_with_compute_prepass`).
  - Patchlist topology **without GS/HS/DS bound** uses the synthetic expansion prepass (emits a
    deterministic triangle) so apps that select patchlists speculatively can still render *something*
    during bring-up.
  - Patchlist topology **with HS+DS bound** routes through an initial tessellation compute prepass
    pipeline (currently PatchList3 only) that expands the patch list into an indexed triangle list
    for rendering. Guest HS/DS DXBC is not executed yet; the pipeline uses placeholder/passthrough
    stages (HS passthrough currently writes a fixed tess factor: `4.0`).
  - Tessellation building blocks live under `crates/aero-d3d11/src/runtime/tessellation/` (VS-as-compute
    stub, HS passthrough, layout pass, DS passthrough, index gen, sizing/guardrails).
  - Guest HS/DS handles/resources are tracked for state/binding, but not executed yet.
  - Design doc: [`docs/graphics/tessellation-emulation.md`](./tessellation-emulation.md)
  - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`CmdPrimitiveTopology::PatchList`, `gs_hs_ds_emulation_required`, `exec_draw_with_compute_prepass`)
  - Tests:
    - [`crates/aero-d3d11/tests/aerogpu_cmd_tessellation_smoke.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_tessellation_smoke.rs)
    - [`crates/aero-d3d11/tests/aerogpu_cmd_tessellation_hs_ds_compute_prepass_error.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_tessellation_hs_ds_compute_prepass_error.rs)
- SM5 compute/UAV bring-up is partially supported, but still has important limitations:
  - `sync` barriers are translated for compute shaders.
    - Fence-only variants (no thread-group sync) do not have a perfect WGSL/WebGPU mapping; the current translation uses `storageBarrier()` as an approximation and therefore rejects fence-only `sync` in potentially divergent control flow (see `crates/aero-d3d11/src/shader_translate.rs`).
    - `*WithGroupSync` barriers are translated, but are rejected when they appear after potentially conditional returns (to avoid deadlocks when not all invocations reach the barrier).
  - Typed UAV stores and UAV buffer atomics are supported for a small subset of formats/operations, but broader `RWTexture*` and `Interlocked*` coverage is still missing.

Roadmap/plan docs:

- [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md)
- [`docs/graphics/win7-d3d10-11-umd-minimal.md`](./win7-d3d10-11-umd-minimal.md)

---

## 6) Web presenters/backends (`web/src/gpu/*`)

The browser “present” layer takes RGBA8 frames and draws them to an `OffscreenCanvas`.

Status checklist:

- [x] WebGPU presenter (native WebGPU API)
- [x] WebGL2 fallback presenter (raw WebGL2)
- [x] WebGL2 presenter via `wgpu` (WASM, forcing the wgpu GL backend)

Code pointers:

- API surface: [`web/src/gpu/presenter.ts`](../../web/src/gpu/presenter.ts)
- WebGPU presenter: [`web/src/gpu/webgpu-presenter.ts`](../../web/src/gpu/webgpu-presenter.ts)
- Raw WebGL2 presenter: [`web/src/gpu/raw-webgl2-presenter.ts`](../../web/src/gpu/raw-webgl2-presenter.ts)
- wgpu-over-WebGL2 presenter: [`web/src/gpu/wgpu-webgl2-presenter.ts`](../../web/src/gpu/wgpu-webgl2-presenter.ts)

Test pointers:

- [`web/src/gpu/webgpu-presenter-backend.test.ts`](../../web/src/gpu/webgpu-presenter-backend.test.ts)
- [`web/src/gpu/frame_pacing.test.ts`](../../web/src/gpu/frame_pacing.test.ts)

---

## 7) Current critical path integration gaps (factual)

This section lists integration blockers that prevent a full “Win7 WDDM + accelerated rendering” experience on the canonical machine today.

### AeroGPU command execution: external executor exists; Win7 validation is still pending

What exists today:

- `aero-machine` implements **BAR0/BAR1 transport** (ring + fences + vblank/scanout/cursor regs) and decodes submissions.
  It can route `AEROGPU_CMD` payloads via:
  - the **submission bridge** (external executor), or
  - an optional **in-process backend** (immediate/null, plus feature-gated native wgpu backend).
- Default bring-up behavior (no backend, submission bridge disabled) completes fences without executing commands (to avoid wedging early guests).
- The canonical browser runtime uses the **submission bridge** and executes command streams in the GPU worker
  (`web/src/workers/gpu-worker.ts`), completing fences back into the device model.

Evidence/pointers:

- Device-side capture + bridge/backends: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs)
  (`enable_submission_bridge`, `drain_pending_submissions`, `complete_fence_from_backend`, `set_backend`)
- Browser control-plane routing: [`web/src/runtime/coordinator.ts`](../../web/src/runtime/coordinator.ts)
  (`forwardAerogpuSubmit`, `forwardAerogpuFenceComplete`)
- GPU worker executor: [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts) (`handleSubmitAerogpu`)

Fast regression commands:

```bash
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_submission_bridge --locked
AERO_TIMEOUT=600 AERO_MEM_LIMIT=32G bash ./scripts/safe-run.sh npm -w web run test:unit -- src/runtime/coordinator.test.ts
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/web/gpu_submit_aerogpu.spec.ts
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/web/gpu_submit_aerogpu_vsync_completion.spec.ts
```

What is still missing (P0):

- **End-to-end Win7 bring-up + accelerated rendering validation** on the canonical browser machine:
  driver install → ring submissions → ACMD execution → scanout present → DWM/Aero stability.
  (See: [`docs/graphics/win7-aerogpu-validation.md`](./win7-aerogpu-validation.md))
- Validate vblank + vsynced present behavior against the documented contract (DWM stability):
  [`docs/graphics/win7-vblank-present-requirements.md`](./win7-vblank-present-requirements.md).

### WDDM scanout publication into `ScanoutState` exists (MVP) but needs end-to-end validation

- `aero-machine` publishes **legacy** scanout transitions (text ↔ VBE LFB) to `ScanoutState`, and can also publish WDDM scanout state from BAR0 scanout0 registers when `Machine::process_aerogpu()` runs (atomic builds).
  - Code: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`process_aerogpu`, INT 10h scanout publishing)
  - Code: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) (`take_scanout0_state_update`)
  - Tests: [`crates/aero-machine/tests/aerogpu_wddm_scanout_state_format_mapping.rs`](../../crates/aero-machine/tests/aerogpu_wddm_scanout_state_format_mapping.rs)
  - Disable semantics: after WDDM scanout is claimed, clearing `SCANOUT0_ENABLE=0` publishes a **disabled WDDM** descriptor
    (source=WDDM, base/width/height/pitch=0) so legacy scanout cannot steal ownership back.
    - Test: [`crates/aero-machine/tests/aerogpu_scanout_disable_publishes_wddm_disabled.rs`](../../crates/aero-machine/tests/aerogpu_scanout_disable_publishes_wddm_disabled.rs)
- The GPU worker can present WDDM scanout from either guest RAM **or** the shared VRAM aperture (BAR1 backing) when `ScanoutState` is published with `source=WDDM` and a non-zero `base_paddr`:
  - Code: [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts) (`tryReadScanoutFrame` / `tryReadScanoutRgba8`)
  - E2E test (guest RAM base_paddr): [`tests/e2e/wddm_scanout_smoke.spec.ts`](../../tests/e2e/wddm_scanout_smoke.spec.ts) (harness: [`web/wddm-scanout-smoke.ts`](../../web/wddm-scanout-smoke.ts))
  - E2E test (VRAM aperture base_paddr): [`tests/e2e/wddm_scanout_vram_smoke.spec.ts`](../../tests/e2e/wddm_scanout_vram_smoke.spec.ts) (harness: [`web/wddm-scanout-vram-smoke.ts`](../../web/wddm-scanout-vram-smoke.ts))
  - VRAM/base-paddr contract notes: [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md#vram-bar1-backing-as-a-sharedarraybuffer)
- Manual harness: [`web/wddm-scanout-debug.html`](../../web/wddm-scanout-debug.html) (interactive toggles for scanoutState source/base_paddr/pitch and BGRX X-byte alpha forcing)
- Current limitation: scanout presentation is currently limited to a small set of packed formats:
  - 32bpp layouts (`B8G8R8X8` / `B8G8R8A8` / `R8G8B8X8` / `R8G8B8A8` + sRGB variants; X8 treated as fully opaque)
  - 16bpp layouts (`B5G6R5` (opaque) / `B5G5R5A1` (1-bit alpha))
  Unsupported formats publish a deterministic disabled descriptor.

Repro commands:

```bash
# Rust: scanout handoff + disable semantics
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_scanout_handoff --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_scanout_disable_publishes_wddm_disabled --locked

# Browser e2e: scanout presentation from guest RAM / VRAM aperture
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/wddm_scanout_vram_smoke.spec.ts
```

Impact:

- End-to-end validation is still required that the Win7 driver + browser runtime converge on supported scanout formats + update cadence (see docs below).

Owning docs:

- [`docs/graphics/win7-wddm11-aerogpu-driver.md`](./win7-wddm11-aerogpu-driver.md)
- [`docs/graphics/win7-vblank-present-requirements.md`](./win7-vblank-present-requirements.md)

### Canonical machine vs sandbox: duplicate device models

- A more complete AeroGPU device-side library exists in `crates/aero-devices-gpu` (portable PCI wrapper + ring executor + optional native wgpu backend) and is wired into the legacy sandbox integration surface in `crates/emulator`.
  The canonical in-browser machine (`crates/aero-machine` + `crates/aero-wasm` + web workers) currently has its own BAR0/BAR1 integration layer behind the same PCI identity (`A3A0:0001`) to satisfy the Windows 7 driver binding/boot-display contract, and relies on the submission bridge (browser) or optional in-process backends (native) for command execution.
  Consolidating these integration surfaces onto a single device model remains outstanding.
  - Shared device-side library: [`crates/aero-devices-gpu/src/pci.rs`](../../crates/aero-devices-gpu/src/pci.rs), [`crates/aero-devices-gpu/src/executor.rs`](../../crates/aero-devices-gpu/src/executor.rs)
  - Legacy emulator integration: [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs), [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs)
  - Canonical machine integration: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) + WASM bridge in [`crates/aero-wasm/src/lib.rs`](../../crates/aero-wasm/src/lib.rs) + web runtime wiring in [`web/src/workers/machine_cpu.worker.ts`](../../web/src/workers/machine_cpu.worker.ts) / [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts)

### End-to-end Win7 graphics validation: needs verification

The repo contains extensive unit/integration tests for ABI correctness and host-side execution, but new contributors should treat these items as **unknown until verified end-to-end in the browser runtime**:

- Win7 install boots to desktop under `aero-wasm` + web runtime.
- Win7 AeroGPU driver can be installed and submit work end-to-end (including scanout handoff and vblank waits).

Where to start verifying:

- [`tests/windows7_boot.rs`](../../tests/windows7_boot.rs) (baseline Win7 boot)
- [`docs/graphics/win7-aerogpu-validation.md`](./win7-aerogpu-validation.md) (driver + validation checklist)

---

## Appendix: Known duplicates / tech debt (pointers)

 - VGA device model wiring has two *integration* surfaces, but one shared implementation:
   - canonical VGA/VBE device model: [`crates/aero-gpu-vga/`](../../crates/aero-gpu-vga/)
   - legacy emulator module path: [`crates/emulator/src/devices/vga.rs`](../../crates/emulator/src/devices/vga.rs) (re-export of `aero-gpu-vga` for compatibility)
- Multiple AeroGPU device models exist for the canonical versioned ABI (`A3A0:0001`):
  - canonical machine MVP: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) + display/VRAM glue in [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  - shared device-side library: [`crates/aero-devices-gpu/src/pci.rs`](../../crates/aero-devices-gpu/src/pci.rs) (legacy sandbox integration: [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs))
  - legacy bring-up ABI (`1AED:0001`): [`crates/emulator/src/devices/pci/aerogpu_legacy.rs`](../../crates/emulator/src/devices/pci/aerogpu_legacy.rs)
  - contract doc: [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- Two command execution paths exist in the web runtime:
  - TypeScript CPU executor: [`web/src/workers/aerogpu-acmd-executor.ts`](../../web/src/workers/aerogpu-acmd-executor.ts)
  - Rust/WASM executor: [`crates/aero-gpu/src/acmd_executor.rs`](../../crates/aero-gpu/src/acmd_executor.rs) (surfaced via `crates/aero-gpu-wasm/`)
- Shared-surface bookkeeping is centralized in [`crates/aero-gpu/src/shared_surface.rs`](../../crates/aero-gpu/src/shared_surface.rs) (`SharedSurfaceTable`) and used by both the D3D9 and D3D11 executors.
  - Lightweight mirrors (for protocol tests + tooling): [`web/src/workers/aerogpu-acmd-executor.ts`](../../web/src/workers/aerogpu-acmd-executor.ts), [`web/tools/gpu_trace_replay.ts`](../../web/tools/gpu_trace_replay.ts)
  - Unit tests: `crates/aero-gpu/src/tests/shared_surface.rs` and `crates/aero-gpu/src/shared_surface.rs` (module `tests`)
  - Executor-level tests: `crates/aero-gpu/tests/*shared_surface*` and `crates/aero-d3d11/tests/aerogpu_cmd_shared_surface.rs`

---

## Appendix: “Known good” local test commands

These are the fast, repeatable commands used to validate the current graphics stack.

```bash
# Boot display (VGA/VBE) + machine wiring
bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --locked

# wasm32 guardrail (compile-only; does not require a JS runtime)
# (Increase timeout if needed; first-time wasm32 builds can be slow without a warm Cargo cache.)
AERO_TIMEOUT=1800 bash ./scripts/safe-run.sh cargo xtask wasm-check

# AeroGPU bridge/backends (canonical in-browser integration boundary)
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_submission_bridge --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_immediate_backend_completes_fence --locked

# AeroGPU protocol + host-side command processing
bash ./scripts/safe-run.sh cargo test -p aero-protocol --locked
bash ./scripts/safe-run.sh npm run test:protocol
bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked

# D3D translation layers
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked

# Legacy/sandbox emulator path (device model + e2e tests)
bash ./scripts/safe-run.sh cargo test -p aero-devices-gpu --locked
bash ./scripts/safe-run.sh cargo test -p emulator --locked

# Browser e2e smoke tests
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts
bash ./scripts/safe-run.sh npm run test:e2e -- tests/e2e/web/gpu_submit_aerogpu.spec.ts
```
