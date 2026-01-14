# Workstream B: Graphics

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

## Current status / what’s missing

Most of the “hard” graphics pieces already exist in-tree (with unit/integration tests). The main
remaining gap is **end-to-end canonical AeroGPU**: `aero_machine` exposes the correct PCI identity and
has working boot-display fallbacks + scanout/vblank plumbing, but it still does **not** execute real
BAR0 command streams (AEROGPU_CMD) or drive a fully validated “Win7 WDDM → scanout → browser canvas”
path.

Key docs for that bring-up:

- [`docs/abi/aerogpu-pci-identity.md`](../docs/abi/aerogpu-pci-identity.md) — canonical AeroGPU PCI IDs + current `aero_machine::Machine` status
- [`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md) — required VGA/VBE compatibility + scanout handoff model
- [`docs/graphics/win7-vblank-present-requirements.md`](../docs/graphics/win7-vblank-present-requirements.md) — Win7 vblank/present timing contract (DWM/Aero stability)

Quick reality check (as of this repo revision):

- ✅ Boot display (default): `MachineConfig::enable_vga=true` uses `crates/aero-gpu-vga/` and is wired into
  `crates/aero-machine/` (plus BIOS INT 10h handlers in `crates/firmware/`). When the PC platform is enabled,
  `aero_machine` also exposes a **transitional** Bochs/QEMU “Standard VGA”-like PCI stub at `00:0c.0` used only
  to route the VBE LFB through PCI MMIO (stub BAR mirrors the configured LFB base; legacy default
  `0xE000_0000`).
- ✅ Canonical AeroGPU identity in `aero_machine`: `MachineConfig::enable_aerogpu=true` (requires `enable_pc_platform=true`) /
  `MachineConfig::win7_graphics(...)`
  exposes `A3A0:0001` at `00:07.0` with **BAR1-backed VRAM** and VRAM-backed legacy VGA/VBE decode (`0xA0000..0xC0000`),
  **minimal BAR0 MMIO + ring/fence transport** (no-op command execution), BAR0 scanout regs + vblank tick/IRQ, and
  WDDM scanout presentation/boot→WDDM handoff via `Machine::display_present` (see
  `crates/aero-machine/src/{lib.rs,aerogpu.rs}` and `crates/aero-machine/tests/aerogpu_bar0_mmio_vblank.rs`).
- ✅ AeroGPU ABI/protocol: `emulator/protocol/` (crate `aero-protocol`) contains Rust **and**
  TypeScript mirrors + ABI drift tests; it’s consumed by both Rust (`crates/aero-gpu/`, `crates/emulator/`)
  and the browser GPU worker (`web/src/workers/`).
- ✅ Full AeroGPU BAR0/MMIO/ring/vblank device model exists in the separate `crates/emulator/` sandbox (used by host-side tests),
  but it is not yet the canonical in-browser machine wiring.
- ✅ D3D9 + D3D11 translation: substantial implementations exist (`crates/aero-d3d9/`,
  `crates/aero-d3d11/`) with extensive host-side tests.
- ✅ WebGPU backend: `crates/aero-webgpu/` + `crates/aero-gpu/` provide WebGPU/wgpu-backed execution and present paths.
- 🚧 Missing: `aero_machine` still lacks **BAR0 command execution** for real AeroGPU submissions (AEROGPU_CMD streams).
  Today, ring submissions are treated as no-ops (fence-only). This blocks end-to-end “Win7 WDDM → accelerated
  present” bring-up in the canonical browser machine.

## Overview

This workstream owns **graphics emulation**: VGA/VBE for boot, DirectX 9/10/11 translation for Windows applications, and the WebGPU/WebGL2 backend that renders to the browser canvas.

Graphics is what makes Windows 7 "usable." The Aero glass interface, DWM compositor, and all Windows applications depend on this workstream.

---

## Key Crates & Directories

| Crate/Directory | Purpose |
|-----------------|---------|
| `crates/aero-gpu/` | Core GPU abstraction, WebGPU backend |
| `crates/aero-gpu-vga/` | VGA/VBE mode emulation |
| `crates/aero-gpu-wasm/` | WASM bindings for GPU |
| `crates/aero-d3d9/` | DirectX 9 state machine and translation |
| `crates/aero-d3d9-shader/` | D3D9 shader helper crate (currently a thin wrapper around shared DXBC parsing) |
| `crates/legacy/aero-d3d9-shader/` | Legacy SM2/SM3 token-stream parser + disassembler (not used by runtime) |
| `crates/aero-d3d11/` | DirectX 10/11 translation |
| `crates/aero-dxbc/` | DXBC bytecode parser (shared) |
| `crates/aero-webgpu/` | WebGPU abstraction layer |
| `emulator/protocol/` | **Canonical** AeroGPU ABI mirrors (Rust + TypeScript) |
| `crates/aero-machine/` | Canonical full-system machine (`aero_machine::Machine`) — currently boots via `aero-gpu-vga` |
| `crates/emulator/` | Device-model sandbox (contains a full AeroGPU BAR0/MMIO/ring implementation used by host-side tests) |
| `drivers/aerogpu/` | Windows 7 AeroGPU driver (KMD + UMD) |
| `web/src/gpu/` + `web/src/workers/` | TypeScript GPU runtime + GPU worker plumbing |

---

## Essential Documentation

**Must read:**

- [`docs/graphics/status.md`](../docs/graphics/status.md) — Canonical “what’s implemented vs missing” graphics status page
- [`docs/04-graphics-subsystem.md`](../docs/04-graphics-subsystem.md) — Graphics architecture overview
- [`docs/16-d3d9ex-dwm-compatibility.md`](../docs/16-d3d9ex-dwm-compatibility.md) — D3D9Ex for DWM/Aero
- [`docs/16-d3d10-11-translation.md`](../docs/16-d3d10-11-translation.md) — D3D10/11 details
- [`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md) — VGA/VBE boot compatibility
- [`docs/abi/aerogpu-pci-identity.md`](../docs/abi/aerogpu-pci-identity.md) — AeroGPU PCI identity contract (A3A0:0001)
- [`docs/graphics/win7-vblank-present-requirements.md`](../docs/graphics/win7-vblank-present-requirements.md) — Win7 vblank/present semantics (DWM)

**Reference:**

- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture
- [`docs/11-browser-apis.md`](../docs/11-browser-apis.md) — WebGPU/WebGL2 browser integration

---

## Interface Contracts

### Display Output

```rust
// `aero_gpu_vga::DisplayOutput` (implemented by `aero_gpu_vga::VgaDevice`).
pub trait DisplayOutput {
    fn get_framebuffer(&self) -> &[u32];
    fn get_resolution(&self) -> (u32, u32);
    fn present(&mut self);
}
```

In the canonical machine (`crates/aero-machine`), the host reads display output via:

- `Machine::display_present()`
- `Machine::display_framebuffer()` (RGBA8888)
- `Machine::display_resolution()`

### Host-side AeroGPU command processing (Rust)

- Command stream parsing + Ex-facing state machine (fence/present bookkeeping): `crates/aero-gpu/src/{protocol.rs,command_processor.rs}`
- WebGPU-backed command execution:
  - D3D9: `crates/aero-gpu/src/aerogpu_d3d9_executor.rs`
  - D3D10/11: `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`

### AeroGPU Device ↔ Driver Protocol

The AeroGPU Windows driver communicates with the emulator via a shared protocol. See:
- `drivers/aerogpu/protocol/` — AeroGPU protocol headers (`aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
- `emulator/protocol/aerogpu/` — Emulator-side mirrors (Rust + TypeScript)

Reference: `docs/abi/aerogpu-pci-identity.md` (canonical AeroGPU VID/DID contract; note that the canonical
`aero_machine::Machine` can expose the AeroGPU PCI identity and BAR1-backed VRAM via
`MachineConfig::enable_aerogpu` (mutually exclusive with `enable_vga`), and uses the standalone
`aero_gpu_vga` + `00:0c.0` PCI stub when `enable_vga=true`).

---

## Tasks

The tables below are meant to be an **onboarding map**: what already exists in-tree (with tests) and
what remains.

Legend:

- **Implemented** = exists in-tree and has at least unit/integration test coverage.
- **Partial** = exists, but is intentionally minimal/stubbed or has known gaps.
- **Remaining** = not implemented yet (or only exists as an out-of-tree doc/spec).

### Boot display: VGA/VBE (`crates/aero-gpu-vga`)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| VG-001 | Implemented | VGA register + legacy VRAM emulation (sequencer/CRTC/attribute/graphics + 0xA0000..0xBFFFF windows) | `crates/aero-gpu-vga/src/lib.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked` |
| VG-002 | Implemented | Text mode rasterization (80x25) | `crates/aero-gpu-vga/src/lib.rs`, `crates/aero-gpu-vga/src/text_font.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked` |
| VG-003 | Implemented | Mode 13h (320x200x256) chain-4 rendering | `crates/aero-gpu-vga/src/lib.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked` |
| VG-004 | Partial | Planar graphics write modes + basic rasterization (enough for BIOS/boot) | `crates/aero-gpu-vga/src/lib.rs` (planar paths + tests) | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked` |
| VG-005 | Implemented | Bochs VBE (`VBE_DISPI`) linear framebuffer modes (LFB base configurable; legacy default `SVGA_LFB_BASE`) | `crates/aero-gpu-vga/src/lib.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-machine --test boot_int10_vbe_sets_mode --locked` |
| VG-006 | Implemented | Palette + DAC behavior (VGA ports `0x3C6..0x3C9`) | `crates/aero-gpu-vga/src/palette.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked` |
| VG-007 | Implemented | Snapshot/restore (optional; behind `io-snapshot`) | `crates/aero-gpu-vga/src/snapshot.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-machine --test vga_snapshot_roundtrip --locked` |
| VG-008 | Implemented | BIOS INT 10h VGA + VBE entrypoints (real-mode boot) | `crates/firmware/src/bios/int10.rs`, `crates/firmware/src/bios/int10_vbe.rs` | `bash ./scripts/safe-run.sh cargo test -p firmware --test int10_vbe --locked` |

### AeroGPU ABI/protocol (`emulator/protocol`, crate `aero-protocol`)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| AGPU-PROTO-001 | Implemented | Rust mirrors of `drivers/aerogpu/protocol/*.h` (PCI IDs, MMIO regs, ring ABI, command ABI) | `emulator/protocol/aerogpu/*.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-protocol --locked` |
| AGPU-PROTO-002 | Implemented | TypeScript mirrors + iterators/writers (consumed by `web/src/workers/`) | `emulator/protocol/aerogpu/*.ts` | `npm run test:protocol` |
| AGPU-PROTO-003 | Implemented | ABI drift / conformance tests (Rust + TS) | `emulator/protocol/tests/*` | `bash ./scripts/safe-run.sh cargo test -p aero-protocol --locked` and `npm run test:protocol` |

### AeroGPU device model + scanout plumbing (the real remaining work)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| AGPU-MACHINE-001 | Partial (in `crates/aero-machine/`) | `A3A0:0001` @ `00:07.0`: BAR1 VRAM + VRAM-backed legacy VGA/VBE decode + BIOS VBE LFB/text fallback; minimal BAR0 MMIO + ring/fence/IRQ transport (no cmd exec), plus BAR0 scanout regs + vblank tick/IRQ + WDDM scanout presentation | `crates/aero-machine/src/{lib.rs,aerogpu.rs}` | `bash ./scripts/safe-run.sh cargo test -p aero-machine --test boot_int10_aerogpu_vbe_115_sets_mode --locked`; `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_ring_noop_fence --locked`; `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_bar0_mmio_vblank --locked` |
| AGPU-DEV-001 | Implemented (in `crates/emulator/`, not yet `aero_machine`) | Full AeroGPU PCI function (A3A0:0001): BAR0 MMIO, rings, IRQs, vblank tick, scanout regs | `crates/emulator/src/devices/pci/aerogpu.rs` | `bash ./scripts/safe-run.sh cargo test -p emulator --test aerogpu_device --locked` |
| AGPU-DEV-002 | Implemented | WebGPU-backed command execution + readback for tests | `crates/emulator/src/gpu_worker/aerogpu_wgpu_backend.rs` | `bash ./scripts/safe-run.sh cargo test -p emulator --test aerogpu_end_to_end --locked` |
| AGPU-WIRE-001 | **Remaining (P0)** | Implement real BAR0 **command execution** (AEROGPU_CMD) in `crates/aero-machine` (today submissions are fence-only no-ops): parse/execute command streams via WebGPU/wgpu and complete fences/presents correctly | Start: `crates/aero-machine/src/aerogpu.rs` (currently no-op) • Likely reuse: `crates/aero-gpu/src/command_processor.rs`, `crates/aero-webgpu/` • Reference impl: `crates/emulator/src/devices/pci/aerogpu.rs`, `crates/emulator/src/gpu_worker/*` • ABI: `emulator/protocol/aerogpu/` | Keep guards passing: `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_ring_noop_fence --locked`; reference e2e executor: `bash ./scripts/safe-run.sh cargo test -p emulator --test aerogpu_end_to_end --locked` |
| AGPU-WIRE-002 | Implemented (in `crates/aero-machine/`) | Boot display → WDDM scanout handoff: once the guest enables scanout0, `Machine::display_present` prefers the WDDM framebuffer over VBE/text (sticky semantics) | `crates/aero-machine/src/lib.rs` (`display_present`, `display_present_aerogpu_scanout`) | (No dedicated unit test yet; smoke via) `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_bar0_mmio_vblank --locked` |
| AGPU-WIRE-003 | Partial | Browser presentation path for WDDM scanout state exists (`SCANOUT_SOURCE_WDDM`). `aero_machine` can publish scanout0 descriptors when a shared `ScanoutState` is installed via `Machine::set_scanout_state`, but the canonical browser runtime still needs to plumb that shared state into the WASM machine for end-to-end present. | Scanout state contract: `crates/aero-shared/src/scanout_state.rs` • Machine scanout publication: `crates/aero-machine/src/lib.rs` (`set_scanout_state`, `process_aerogpu`) • GPU worker: `web/src/workers/gpu-worker.ts` (Vite entrypoint: `gpu.worker.ts`) • E2E smoke harness: `web/wddm-scanout-smoke.ts`, `tests/e2e/wddm_scanout_smoke.spec.ts` | `bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_wddm_scanout_state_format_mapping --locked`; `npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts` |
| AGPU-WIRE-004 | **Remaining (P0)** | Validate Win7 vblank + vsynced present behavior against the documented contract (DWM stability) | Spec: `docs/graphics/win7-vblank-present-requirements.md` • Guest tests: `drivers/aerogpu/tests/win7/*` | In Win7 guest: `cd drivers\\aerogpu\\tests\\win7 && build_all_vs2010.cmd && run_all.cmd` |

### DirectX 9 translation (`crates/aero-d3d9`)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| D9-001 | Implemented | DXBC container parsing helpers | `crates/aero-d3d9/src/dxbc/`, `crates/aero-dxbc/src/` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked` |
| D9-002 | Implemented | SM2/SM3 decode → IR → WGSL generation | `crates/aero-d3d9/src/sm3/`, `crates/aero-d3d9/src/shader.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked` |
| D9-003 | Implemented | Fixed-function pipeline translation (FVF/TSS → generated WGSL) | `crates/aero-d3d9/src/fixed_function/` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test d3d9_fixed_function --locked` |
| D9-004 | Implemented | Resource model + runtime/state tracking (textures, samplers, RT/DS, eviction) | `crates/aero-d3d9/src/resources/`, `crates/aero-d3d9/src/runtime/`, `crates/aero-d3d9/src/state/` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked` |
| D9-005 | Partial | D3D9Ex/DWM-facing semantics live in the **AeroGPU command processor** layer, not the translator | `crates/aero-gpu/src/command_processor.rs`, `docs/16-d3d9ex-dwm-compatibility.md` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu --test aerogpu_ex_protocol --locked` |

### DirectX 10/11 translation (`crates/aero-d3d11`)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| D11-001 | Implemented | SM4/SM5 decode + translation to WGSL for VS/PS/**CS** (FL10_0 bring-up + basic compute) | `crates/aero-d3d11/src/sm4/`, `crates/aero-d3d11/src/shader_translate.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test shader_translate --locked` |
| D11-002 | Implemented | WGPU-backed AeroGPU command executor (render/present **and compute pass/dispatch**) | `crates/aero-d3d11/src/runtime/` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test aerogpu_cmd_smoke --locked` |
| D11-003 | Partial | Geometry shaders: **stage_ex ABI + compute-prepass scaffolding** (wgpu has no native GS stage). GS DXBC is currently accepted-but-ignored (the prepass is still a placeholder; it does not execute guest GS bytecode yet). | `crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`, `crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_*` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_compute_prepass_smoke --locked`; `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test aerogpu_cmd_geometry_shader_ignore --locked` |
| D11-004 | Remaining | Hull/Domain (tessellation) execution + UAV/structured buffers + broader SM5 parity (stage_ex bindings are plumbed, but HS/DS shader execution is not implemented yet; patchlist topologies currently route through the **placeholder** compute-prepass) | Start at: `crates/aero-d3d11/src/shader_translate.rs`, `crates/aero-d3d11/src/runtime/{aerogpu_cmd_executor.rs,execute.rs}` • ABI: `emulator/protocol/aerogpu/aerogpu_cmd.rs` (stage_ex fields) | Add tests under `crates/aero-d3d11/tests/` and run `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked` |

### WebGPU/WebGL2 backend (`crates/aero-gpu`, `crates/aero-webgpu`, `crates/aero-gpu-wasm`)

| ID | Status | Task | Where | How to test |
|----|--------|------|-------|-------------|
| WG-001 | Implemented | WebGPU adapter/device init + feature/limit negotiation | `crates/aero-webgpu/src/webgpu.rs`, `crates/aero-webgpu/src/caps.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-webgpu --test webgpu_smoke --locked` |
| WG-002 | Implemented | wgpu-backed backend + shader/pipeline/resource helpers | `crates/aero-gpu/src/backend/wgpu_backend.rs`, `crates/aero-gpu/src/*` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked` |
| WG-003 | Partial | WebGL2 fallback is **present-only** today (no full D3D execution) | `crates/aero-gpu/src/backend/webgl2_present_backend.rs`, `web/src/gpu/raw-webgl2-presenter.ts` | `bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test negotiated_features_gl --locked` |
| WG-004 | Partial | Persistent caching exists for **D3D9 shader translation artifacts**; pipeline cache is still in-memory | Rust: `crates/aero-d3d9/src/runtime/shader_cache.rs` • JS: `web/gpu-cache/persistent_cache.ts` | (Browser) `wasm-pack test --headless --chrome crates/aero-d3d9` |
| WG-005 | Implemented | WASM bindings used by the browser runtime | `crates/aero-gpu-wasm/src/lib.rs` | `bash ./scripts/safe-run.sh cargo test -p aero-gpu-wasm --locked` |

---

## Shader Translation Pipeline

```
DXBC Bytecode (SM2/3/4/5)
    ↓
aero-dxbc parser
    ↓
Internal IR
    ↓
WGSL Generation
    ↓
WebGPU Shader Module
    ↓
Browser GPU
```

Key considerations:
- DXBC is a register-based bytecode; WGSL is more structured
- Texture sampling semantics differ between D3D and WebGPU
- Coordinate system differences (D3D is top-left origin, WebGPU is bottom-left)

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Desktop frame rate | ≥30 FPS with Aero enabled |
| Shader compilation | <100ms per shader (cached after first compile) |
| Draw call overhead | Batching should reduce by ≥50% |

---

## Coordination Points

### Dependencies on Other Workstreams

- **CPU (A)**: VGA register access comes through `CpuBus::io_read/io_write`
- **Windows Drivers (C)**: AeroGPU KMD/UMD must match emulator device model
- **Integration (H)**: VGA BIOS must work for boot

### What Other Workstreams Need From You

- Working VGA text mode for BIOS/boot
- Stable AeroGPU device model for driver development
- D3D9Ex surface for DWM compositor

---

## Testing

```bash
# Run graphics tests
bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked
bash ./scripts/safe-run.sh cargo test -p aero-protocol --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked
bash ./scripts/safe-run.sh cargo test -p aero-webgpu --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked

# WASM compatibility checks (browser runtime)
bash ./scripts/safe-run.sh cargo xtask wasm-check

# Run protocol TypeScript tests (Node test runner)
npm run test:protocol

# Browser e2e: WDDM scanout state presentation (no Win7 guest; validates BGRX->RGBA + alpha policy)
npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts

# Manual (interactive): WDDM scanout debug harness (toggle scanoutState source/base_paddr/pitch and XRGB alpha forcing)
# Open `/web/wddm-scanout-debug.html` under a COOP/COEP-enabled dev server (e.g. `npm run dev:harness`).

# Run emulator-side AeroGPU device model tests
bash ./scripts/safe-run.sh cargo test -p emulator --test aerogpu_end_to_end --locked

# Run aero_machine AeroGPU boot display + BAR0 ring/fence + vblank/scanout plumbing smoke tests
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_vram_alias --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test boot_int10_aerogpu_vbe_115_sets_mode --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_ring_noop_fence --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --test aerogpu_bar0_mmio_vblank --locked

# Run D3D9 translator-focused tests (no GPU required)
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test vertex_decl_translate --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test sm3_wgsl --locked

# Run D3D9 WebGPU integration tests (wgpu/WebGPU; may skip unless AERO_REQUIRE_WEBGPU=1)
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test d3d9_fixed_function --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test d3d9_vertex_input --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --test d3d9_blend_depth_stencil --locked

# Run D3D11 command-executor smoke test (wgpu/WebGPU; may skip unless AERO_REQUIRE_WEBGPU=1)
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --test aerogpu_cmd_smoke --locked
```

**Note:** GPU tests may be skipped on headless/GPU-less machines. Set `AERO_REQUIRE_WEBGPU=1` to force failure if no GPU is available.

If you need to validate CPU texture decompression fallbacks (or work around flaky driver/software-adapter compression paths), set `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1` to force wgpu/WebGPU feature negotiation to avoid BC/ETC2/ASTC texture compression.

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Run `bash ./scripts/agent-env-setup.sh` and `source ./scripts/agent-env.sh`
3. ☐ Read [`docs/04-graphics-subsystem.md`](../docs/04-graphics-subsystem.md)
4. ☐ Read [`docs/16-d3d9ex-dwm-compatibility.md`](../docs/16-d3d9ex-dwm-compatibility.md)
5. ☐ Explore `crates/aero-gpu/src/` and `crates/aero-d3d9/src/`
6. ☐ Run existing tests to establish baseline
7. ☐ Pick a task from the tables above and begin

---

*Graphics makes Windows 7 beautiful. This is what users will see.*
