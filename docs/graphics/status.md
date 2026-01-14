# Graphics status (Windows 7 UX)

This is the **single authoritative status doc** for the graphics stack.
It tracks what is **implemented in-tree today** vs what is still **missing** to reach a “Windows 7 feels usable” experience (boot → desktop → DWM/Aero + apps).

Legend:

- `[x]` = implemented (exists in-tree and has tests)
- `[~]` = partial / stubbed / exists in an alternate stack (see notes)
- `[ ]` = missing / not wired / not validated end-to-end

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
| AeroGPU PCI identity + minimal device model in `aero-machine` | `[~]` | [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) + [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) |
| AeroGPU sandbox device model + executor (legacy integration surface) | `[~]` | [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs) + [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs) |
| Scanout shared-memory contracts | `[x]` | [`crates/aero-shared/src/`](../../crates/aero-shared/src/) + [`web/src/ipc/`](../../web/src/ipc/) |
| D3D9 translation/execution (subset) | `[~]` | [`crates/aero-d3d9/`](../../crates/aero-d3d9/) + [`crates/aero-gpu/src/aerogpu_d3d9_executor.rs`](../../crates/aero-gpu/src/aerogpu_d3d9_executor.rs) |
| D3D10/11 translation/execution (subset; VS/PS/CS + GS/HS/DS plumbing) | `[~]` | [`crates/aero-d3d11/`](../../crates/aero-d3d11/) |
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

- [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs) (`VgaDevice`, VBE LFB at `SVGA_LFB_BASE`)

Test pointers:

- [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs) (module `tests`)
  - `text_mode_golden_hash`
  - `mode13h_golden_hash`
  - `vbe_linear_framebuffer_write_shows_up_in_output`

### Wired into the canonical machine (`crates/aero-machine`)

When `MachineConfig::enable_vga=true`, `aero_machine::Machine` wires the VGA/VBE device model for boot display.

Note: when the PC platform is enabled (`enable_pc_platform=true`), the VBE LFB is routed through a transitional Bochs/QEMU “Standard VGA”-like PCI function (BDF `00:0c.0`, `VGA_PCI_BDF`) so the LFB sits inside the PCI MMIO window.

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  - `MachineConfig::enable_vga` docs (port + address ranges)
  - `Machine::reset` (device wiring)
  - `Machine::display_present` / `display_framebuffer` / `display_resolution` (host-facing RGBA8888 snapshot)

Test pointers:

- [`crates/aero-machine/tests/boot_int10_vbe_sets_mode.rs`](../../crates/aero-machine/tests/boot_int10_vbe_sets_mode.rs) (INT 10h VBE mode set)
- [`crates/aero-machine/tests/int10_active_page_renders_text.rs`](../../crates/aero-machine/tests/int10_active_page_renders_text.rs) (text mode active-page behavior)
- [`crates/aero-machine/tests/vga_vbe_lfb_pci.rs`](../../crates/aero-machine/tests/vga_vbe_lfb_pci.rs) (VBE LFB routed via PCI stub)

### Implemented today: AeroGPU boot-display foundation (`enable_aerogpu=true`)

`MachineConfig::enable_aerogpu=true` disables the standalone VGA device and instead provides:

- [x] BAR1-backed VRAM
- [x] legacy VGA window decode (`0xA0000..0xBFFFF`) backed by BAR1 VRAM (mode-dependent aliasing):
  - VBE inactive: `0xA0000..0xBFFFF` ↔ `VRAM[0x00000..0x1FFFF]`
  - VBE active:
    - `0xA0000..0xAFFFF` becomes the VBE banked window into `VRAM[VBE_LFB_OFFSET + bank*64KiB + off]`
    - `0xB0000..0xBFFFF` remains `VRAM[0x10000..0x1FFFF]`
- [x] BIOS VBE LFB base set into BAR1: `PhysBasePtr = BAR1_BASE + VBE_LFB_OFFSET` (`VBE_LFB_OFFSET = 0x40000`)
- [x] host-side presentation fallback when VGA is disabled:
  - WDDM scanout0 if claimed (`SCANOUT0_ENABLE`), otherwise
  - VBE LFB (from BIOS state), otherwise
  - text mode (scan `0xB8000`)

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs)
  - `MachineConfig::enable_aerogpu` docs
  - `Machine::display_present` + `display_present_aerogpu_*` helpers

Test pointers:

- [`crates/aero-machine/tests/boot_int10_aerogpu_vbe_115_sets_mode.rs`](../../crates/aero-machine/tests/boot_int10_aerogpu_vbe_115_sets_mode.rs)
- [`crates/aero-machine/tests/aerogpu_text_mode_scanout.rs`](../../crates/aero-machine/tests/aerogpu_text_mode_scanout.rs)
- [`crates/aero-machine/tests/aerogpu_vbe_lfb_base_bar1.rs`](../../crates/aero-machine/tests/aerogpu_vbe_lfb_base_bar1.rs)

### Missing / still required (boot → WDDM)

- [~] Boot framebuffer → WDDM scanout handoff: host-facing `Machine::display_present` prefers WDDM scanout once `SCANOUT0_ENABLE` is written, but this path still needs end-to-end validation in the browser runtime and shared-scanout publication (see Section 7).
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

#### Canonical machine (`crates/aero-machine`): minimal BAR0 + BAR1 VRAM

`MachineConfig::enable_aerogpu=true` exposes the canonical identity:

- [x] `VID:DID = A3A0:0001`
- [x] BDF `00:07.0`
- [x] BAR1 VRAM + legacy VGA window aliasing
- [~] BAR0 MMIO register block + ring/fence transport + scanout/vblank registers
  - Ring processing is currently **no-op** (fence completion only); full command execution is not implemented.

Code pointers:

- [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`MachineConfig::enable_aerogpu`, BAR1 aliasing, display helpers)
- [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) (BAR0 register model, ring no-op, vblank tick)

Test pointers:

- [`crates/aero-machine/tests/pci_display_bdf_contract.rs`](../../crates/aero-machine/tests/pci_display_bdf_contract.rs) (BDF contract)
- [`crates/aero-machine/tests/machine_aerogpu_pci_identity.rs`](../../crates/aero-machine/tests/machine_aerogpu_pci_identity.rs)
- [`crates/aero-machine/tests/aerogpu_ring_noop_fence.rs`](../../crates/aero-machine/tests/aerogpu_ring_noop_fence.rs)
- [`crates/aero-machine/tests/aerogpu_bar0_mmio_vblank.rs`](../../crates/aero-machine/tests/aerogpu_bar0_mmio_vblank.rs)

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
- Guest-side Win7 tests live under [`drivers/aerogpu/tests/win7/`](../../drivers/aerogpu/tests/win7/) (see [`drivers/aerogpu/tests/win7/README.md`](../../drivers/aerogpu/tests/win7/README.md))

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

1. DXBC SM4/SM5 decode + WGSL translation (VS/PS/CS today; plus stage-ex binding plumbing for GS/HS/DS compute emulation).
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
- Geometry-stage plumbing (compute prepass path): [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs), [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_point_to_triangle.rs)
- Guest-side Win7 tests live under [`drivers/aerogpu/tests/win7/`](../../drivers/aerogpu/tests/win7/) (see e.g. `d3d10_*`, `d3d11_*`)

Known gaps / limitations (enforced by code/tests):

- Geometry shaders are **emulated via compute** (WebGPU has no GS stage), but the current “compute prepass” is still a **placeholder**:
  - it emits fixed triangles (see `GEOMETRY_PREPASS_CS_WGSL`), and
  - it does **not** execute guest GS/HS/DS DXBC yet.
  - Design/notes: [`docs/graphics/geometry-shader-emulation.md`](./geometry-shader-emulation.md) (“Current limitation” section)
  - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`gs_hs_ds_emulation_required`, `exec_draw_with_compute_prepass`)
  - Tests: [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_compute_prepass_smoke.rs)
- GS/HS/DS shader objects are accepted via the `stage_ex` ABI extension, but are currently stored as **stub WGSL modules** (no guest GS/HS/DS DXBC execution yet). Legacy `AerogpuShaderStage::Geometry` (non-`stage_ex`) shader creation is accepted-but-ignored; however, resource bindings for the legacy GS stage (`shader_stage = GEOMETRY`) are still accepted and tracked so apps that set GS resources do not fail.
  - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`exec_create_shader_dxbc`, `from_aerogpu_u32_with_stage_ex`)
  - Tests: [`crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs`](../../crates/aero-d3d11/tests/aerogpu_cmd_geometry_shader_ignore.rs)
- Tessellation (Hull/Domain) execution is not implemented; patchlist topologies are accepted in `SET_PRIMITIVE_TOPOLOGY` but draws error (`patchlist topology requires tessellation emulation`) until HS/DS execution exists.
  - Code: [`crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs`](../../crates/aero-d3d11/src/runtime/aerogpu_cmd_executor.rs) (`patchlist topology requires tessellation emulation`)

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

### `aero-machine` AeroGPU command execution is stubbed

- `MachineConfig::enable_aerogpu` exposes BAR0/BAR1 and implements transport + vblank/scanout register storage, but **does not execute** the AeroGPU command stream.
  - Evidence: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) treats submissions as no-op and only advances `completed_fence`.

Impact:

- The in-tree Win7 driver can plausibly *detect/init* the device and use vblank pacing, but it cannot get accelerated D3D rendering via ACMD execution on the canonical machine yet.

### WDDM scanout publication into `ScanoutState` exists (MVP) but needs end-to-end validation

- `aero-machine` publishes **legacy** scanout transitions (text ↔ VBE LFB) to `ScanoutState`, and can also publish WDDM scanout state from BAR0 scanout0 registers when `Machine::process_aerogpu()` runs (atomic builds).
  - Code: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`process_aerogpu`, INT 10h scanout publishing)
  - Code: [`crates/aero-machine/src/aerogpu.rs`](../../crates/aero-machine/src/aerogpu.rs) (`take_scanout0_state_update`)
  - Tests: [`crates/aero-machine/tests/aerogpu_wddm_scanout_state_format_mapping.rs`](../../crates/aero-machine/tests/aerogpu_wddm_scanout_state_format_mapping.rs)
- The GPU worker can present WDDM scanout from either guest RAM **or** the shared VRAM aperture (BAR1 backing) when `ScanoutState` is published with `source=WDDM` and a non-zero `base_paddr`:
  - Code: [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts) (`tryReadWddmScanoutFrame` / `tryReadWddmScanoutRgba8`)
  - E2E test (guest RAM base_paddr): [`tests/e2e/wddm_scanout_smoke.spec.ts`](../../tests/e2e/wddm_scanout_smoke.spec.ts) (harness: [`web/wddm-scanout-smoke.ts`](../../web/wddm-scanout-smoke.ts))
  - E2E test (VRAM aperture base_paddr): [`tests/e2e/wddm_scanout_vram_smoke.spec.ts`](../../tests/e2e/wddm_scanout_vram_smoke.spec.ts) (harness: [`web/wddm-scanout-vram-smoke.ts`](../../web/wddm-scanout-vram-smoke.ts))
  - VRAM/base-paddr contract notes: [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md#vram-bar1-backing-as-a-sharedarraybuffer)
- Manual harness: [`web/wddm-scanout-debug.html`](../../web/wddm-scanout-debug.html) (interactive toggles for scanoutState source/base_paddr/pitch and BGRX X-byte alpha forcing)
- Current limitation: scanout presentation is currently limited to `B8G8R8X8`-compatible 32bpp layouts (`B8G8R8X8` / `B8G8R8A8` + sRGB variants); unsupported formats publish a deterministic disabled descriptor.

Impact:

- End-to-end validation is still required that the Win7 driver + browser runtime converge on supported scanout formats + update cadence (see docs below).

Owning docs:

- [`docs/graphics/win7-wddm11-aerogpu-driver.md`](./win7-wddm11-aerogpu-driver.md)
- [`docs/graphics/win7-vblank-present-requirements.md`](./win7-vblank-present-requirements.md)

### Canonical machine vs sandbox: duplicate device models

- A more complete AeroGPU device model + executor exists in `crates/emulator`, but it is not the canonical in-browser machine wiring.
  - [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs)
  - [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs)

### End-to-end Win7 graphics validation: needs verification

The repo contains extensive unit/integration tests for ABI correctness and host-side execution, but new contributors should treat these items as **unknown until verified end-to-end in the browser runtime**:

- Win7 install boots to desktop under `aero-wasm` + web runtime.
- Win7 AeroGPU driver can be installed and submit work end-to-end (including scanout handoff and vblank waits).

Where to start verifying:

- [`tests/windows7_boot.rs`](../../tests/windows7_boot.rs) (baseline Win7 boot)
- [`docs/graphics/win7-aerogpu-validation.md`](./win7-aerogpu-validation.md) (driver + validation checklist)

---

## Appendix: Known duplicates / tech debt (pointers)

- Two VGA implementations exist:
  - canonical boot VGA/VBE: [`crates/aero-gpu-vga/`](../../crates/aero-gpu-vga/)
  - legacy emulator VGA: [`crates/emulator/src/devices/vga.rs`](../../crates/emulator/src/devices/vga.rs)
- Two AeroGPU PCI identities/device models exist:
  - canonical versioned ABI (`A3A0:0001`): [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs)
  - legacy bring-up ABI (`1AED:0001`): [`crates/emulator/src/devices/pci/aerogpu_legacy.rs`](../../crates/emulator/src/devices/pci/aerogpu_legacy.rs)
  - contract doc: [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- Two command execution paths exist in the web runtime:
  - TypeScript CPU executor: [`web/src/workers/aerogpu-acmd-executor.ts`](../../web/src/workers/aerogpu-acmd-executor.ts)
  - Rust/WASM executor: [`crates/aero-gpu/src/acmd_executor.rs`](../../crates/aero-gpu/src/acmd_executor.rs) (surfaced via `crates/aero-gpu-wasm/`)

---

## Appendix: “Known good” local test commands

These are the fast, repeatable commands used to validate the current graphics stack.

```bash
# Boot display (VGA/VBE) + machine wiring
bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --locked

# AeroGPU protocol + host-side command processing
bash ./scripts/safe-run.sh cargo test -p aero-protocol --locked
bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked

# D3D translation layers
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked

# Legacy/sandbox emulator path (device model + e2e tests)
bash ./scripts/safe-run.sh cargo test -p emulator --locked
```
