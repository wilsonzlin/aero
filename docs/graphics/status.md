# Graphics status (Windows 7 UX)

This is the **single authoritative status doc** for the graphics workstream.
It tracks what is **implemented in-tree today** vs what is still **missing** to reach a “Windows 7 feels usable” experience (boot → desktop → DWM/Aero + apps).

Legend:

- `[x]` = implemented (exists in-tree and has tests)
- `[ ]` = missing / not wired / not validated end-to-end yet
- `[~]` = implemented in an alternate stack or partially implemented (see note)

Key “read first” architecture docs:

- [`docs/04-graphics-subsystem.md`](../04-graphics-subsystem.md)
- [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- [`docs/graphics/win7-vblank-present-requirements.md`](./win7-vblank-present-requirements.md)

---

## Boot display (VGA text, VBE LFB)

Goal for Win7 UX: **the same virtual GPU** should provide *both* boot VGA/VBE output and the later WDDM scanout path (no device swap).

### Implemented today

- [x] VGA text mode rendering (80×25) in `crates/aero-gpu-vga/`
  - Entry: [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs)
- [x] Bochs/QEMU-style VBE (`VBE_DISPI`) register interface and **linear framebuffer** (LFB)
  - LFB base: configurable (legacy default: `aero_gpu_vga::SVGA_LFB_BASE` / `0xE000_0000`)
  - Entry: [`crates/aero-gpu-vga/src/lib.rs`](../../crates/aero-gpu-vga/src/lib.rs)
- [x] Canonical machine boot display is **`aero_gpu_vga::VgaDevice`** wired into `aero_machine::Machine`
  - Entry: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (VGA integration + PCI stub)
  - Web demo wiring: [`web/src/workers/machine_vga.worker.ts`](../../web/src/workers/machine_vga.worker.ts)

### Missing / still required for Win7

- [~] AeroGPU boot-display foundation exists in `aero_machine` (`enable_aerogpu=true`):
  - BAR1-backed VRAM + legacy VGA window aliasing (`0xA0000..0xBFFFF`) + minimal VGA port decode
  - BIOS VBE mode sets (`INT 10h AX=4F02`) work with the VBE LFB mapped inside BAR1 (see
    `crates/aero-machine/tests/boot_int10_aerogpu_vbe_115_sets_mode.rs`)
  - Still missing: full BAR0 WDDM/MMIO/ring/vblank device model + scanout handoff once the Win7 driver loads
  - Design doc: [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md)
- [ ] Seamless handoff: boot framebuffer → WDDM scanout without losing display or forcing mode resets

---

## AeroGPU PCI identity (A3A0:0001) + canonical BDF

Goal for Win7 UX: the Win7 driver package binds to one stable identity and the emulator always exposes it at the same BDF.

### Implemented today

- [x] **Canonical AeroGPU PCI IDs**: `VID:DID = A3A0:0001`
  - Source of truth: [`drivers/aerogpu/protocol/aerogpu_pci.h`](../../drivers/aerogpu/protocol/aerogpu_pci.h)
  - Status + ABI notes: [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- [x] **Canonical BDF** for AeroGPU: `00:07.0` (exposed when `enable_aerogpu=true`)
  - Guard test: [`crates/aero-machine/tests/pci_display_bdf_contract.rs`](../../crates/aero-machine/tests/pci_display_bdf_contract.rs)
- [x] Transitional VGA/VBE PCI stub (Bochs/QEMU “Standard VGA”-like) is intentionally *not* at `00:07.0`
  - BDF: `00:0c.0`
  - Code: [`crates/aero-machine/src/lib.rs`](../../crates/aero-machine/src/lib.rs) (`VGA_PCI_BDF`)

### Missing / still required for Win7

- [ ] Wire the full AeroGPU BAR0 WDDM/MMIO/ring device model into the canonical `aero_machine::Machine` path
  - Today `aero_machine::Machine` exposes the PCI identity plus BAR1-backed VRAM/legacy VGA aliasing, but it does not yet wire the full BAR0 WDDM/MMIO/ring protocol or scanout path.

---

## WDDM scanout + vblank pacing

Goal for Win7 UX: DWM and apps must see a stable scanout + vsync model (no deadlocks; `WaitForVerticalBlankEvent` completes; `Present` pacing is sane).

### Implemented today

- [~] AeroGPU scanout registers (source 0) + cursor storage are implemented in the **`crates/emulator`** AeroGPU PCI device model
  - Device: [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs)
  - Registers: [`crates/emulator/src/devices/aerogpu_regs.rs`](../../crates/emulator/src/devices/aerogpu_regs.rs)
- [~] Free-running vblank model (default 60 Hz) with:
  - monotonically increasing `vblank_seq`
  - `vblank_time_ns` timestamp
  - `vblank_period_ns`
  - vblank IRQ status that is only latched while enabled
  - Code: [`crates/emulator/src/devices/pci/aerogpu.rs`](../../crates/emulator/src/devices/pci/aerogpu.rs)
- [~] Present/fence completion pacing hooks exist in both:
  - Rust executor path: [`crates/emulator/src/gpu_worker/aerogpu_executor.rs`](../../crates/emulator/src/gpu_worker/aerogpu_executor.rs)
  - Web runtime path: [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts) (vsync-delayed submit completion queue)

### Missing / still required for Win7

- [ ] End-to-end validation in the **canonical browser machine** that Win7 can:
  - boot with the full AeroGPU WDDM device model at `00:07.0`
  - enable scanout
  - block on `D3DKMTWaitForVerticalBlankEvent` without deadlocking
  - keep DWM composition enabled and paced
- [ ] Single-source-of-truth vblank model shared by “device model” and “presenter” layers (avoid double pacing / drift)

---

## D3D9 translation status + test coverage pointers

Goal for Win7 UX: enough D3D9Ex for **DWM** plus broad-enough D3D9 for apps.

### Implemented today (host-side translation/runtime)

- [x] D3D9 translation primitives (`aero-d3d9`)
  - Entry: [`crates/aero-d3d9/src/lib.rs`](../../crates/aero-d3d9/src/lib.rs)
  - Tests: `crates/aero-d3d9/tests/`
- [x] AeroGPU D3D9 command processing/runtime integration (`aero-gpu`)
  - Entry: [`crates/aero-gpu/src/command_processor_d3d9.rs`](../../crates/aero-gpu/src/command_processor_d3d9.rs)
  - Tests: `crates/aero-gpu/tests/aerogpu_d3d9_*`
- [x] End-to-end emulator-level D3D9/AeroGPU exercises exist under `crates/emulator/tests/`
  - Example: [`crates/emulator/tests/aerogpu_d3d9_triangle_end_to_end.rs`](../../crates/emulator/tests/aerogpu_d3d9_triangle_end_to_end.rs)

### Implemented today (Win7 guest test programs)

- [x] Guest-side Win7 D3D9/D3D9Ex validation suite lives under `drivers/aerogpu/tests/win7/`
  - D3D9Ex / DWM critical behavior doc: [`docs/16-d3d9ex-dwm-compatibility.md`](../16-d3d9ex-dwm-compatibility.md)
  - Example tests: `d3d9_validate_device_sanity/`, `d3d9ex_*`

### Missing / still required for Win7

- [ ] Canonical “Win7 boots to Aero desktop” e2e path (device model + driver + translation + presentation wired together)
- [ ] D3D9Ex/DWM behavior proven inside the browser runtime (guest tests are present, but must be runnable in the target environment)

---

## D3D10/11 translation status + test coverage pointers

Goal for Win7 UX: D3D10/11 apps run, and the driver stack can expose at least a stable FL10_0-ish surface.

### Implemented today (host-side translation/runtime)

- [x] SM4-era DXBC decode + translation scaffolding in `aero-d3d11`
  - Entry: [`crates/aero-d3d11/src/lib.rs`](../../crates/aero-d3d11/src/lib.rs)
  - Tests: `crates/aero-d3d11/tests/` (many `aerogpu_cmd_*` and shader/fixture tests)
- [x] AeroGPU D3D11 protocol + execution support in `aero-gpu`
  - Entry: [`crates/aero-gpu/src/protocol_d3d11.rs`](../../crates/aero-gpu/src/protocol_d3d11.rs)

### Implemented today (Win7 guest test programs)

- [x] Win7 guest D3D10/10.1/11 validation programs exist under `drivers/aerogpu/tests/win7/`
  - Examples: `d3d10_triangle/`, `d3d11_triangle/`, `d3d11_texture_sampling_sanity/`

### Missing / still required for Win7

- [ ] Full Win7 UMD/KMD D3D10/11 DDI coverage + DXGI swapchain semantics proven end-to-end
  - Roadmap docs: [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md), plus the focused Win7 bring-up notes in `docs/graphics/`

---

## WebGPU/WebGL2 presentation pipeline status

Goal for Win7 UX: deterministic presentation (format, gamma, alpha) with good-enough performance and a robust “device lost” recovery story.

### Implemented today

- [x] Canonical GPU worker consumes shared framebuffer and presents via WebGPU/WebGL2
  - Entrypoint: [`web/src/workers/gpu-worker.ts`](../../web/src/workers/gpu-worker.ts)
  - Presenter interface: `web/src/gpu/presenter.ts`
  - WebGL2 raw presenter backend: [`web/src/gpu/raw-webgl2-presenter-backend.ts`](../../web/src/gpu/raw-webgl2-presenter-backend.ts)
- [x] Presentation color policy and deterministic alpha guidance documented
  - See “Framebuffer Presentation” section in [`docs/04-graphics-subsystem.md`](../04-graphics-subsystem.md)

### Missing / still required for Win7

- [ ] Single end-to-end “Win7 scanout → GPU worker → canvas” validation harness (including pacing + screenshot regression)
- [ ] Clear contract for “who owns vsync”: guest vblank, host RAF cadence, and submit completion gating must not fight each other

---

## Known duplicates / tech debt (links to the duplicates)

- Two VGA implementations exist today:
  - `crates/aero-gpu-vga/` (canonical machine boot VGA/VBE)
  - `crates/emulator/src/devices/vga/` (legacy emulator VGA)
- Two “AeroGPU” device models / ABIs exist:
  - Canonical versioned ABI (`A3A0:0001`): `crates/emulator/src/devices/pci/aerogpu.rs`
  - Legacy bring-up ABI (`1AED:0001`): `crates/emulator/src/devices/pci/aerogpu_legacy.rs` (and legacy driver package under `drivers/aerogpu/packaging/win7/legacy/`)
  - Contract doc: [`docs/abi/aerogpu-pci-identity.md`](../abi/aerogpu-pci-identity.md)
- Two command execution paths exist in the web runtime:
  - TypeScript CPU executor: `web/src/workers/aerogpu-acmd-executor.ts`
  - Rust/WASM executor: `crates/aero-gpu/src/acmd_executor.rs` (surfaced via `crates/aero-gpu-wasm/`)

---

## “Known good” local test commands (Rust)

These are the fast, repeatable commands used to validate the current graphics stack in CI/dev.
(Some tests are skipped on headless machines without a usable WebGPU/WebGL2 context; see `AERO_REQUIRE_WEBGPU=1` in [`instructions/graphics.md`](../../instructions/graphics.md).)

```bash
# Boot display (VGA/VBE)
bash ./scripts/safe-run.sh cargo test -p aero-gpu-vga --locked
bash ./scripts/safe-run.sh cargo test -p aero-machine --locked

# AeroGPU protocol + command processing
bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked
bash ./scripts/safe-run.sh cargo test -p emulator --locked

# D3D translation layers
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked
```
