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
| `crates/aero-d3d9-shader/` | D3D9 shader parsing |
| `crates/aero-d3d11/` | DirectX 10/11 translation |
| `crates/aero-dxbc/` | DXBC bytecode parser (shared) |
| `crates/aero-webgpu/` | WebGPU abstraction layer |
| `drivers/aerogpu/` | Windows 7 AeroGPU driver (KMD + UMD) |
| `web/src/gpu/` | TypeScript GPU worker code |

---

## Essential Documentation

**Must read:**

- [`docs/04-graphics-subsystem.md`](../docs/04-graphics-subsystem.md) — Graphics architecture overview
- [`docs/16-d3d9ex-dwm-compatibility.md`](../docs/16-d3d9ex-dwm-compatibility.md) — D3D9Ex for DWM/Aero
- [`docs/16-d3d10-11-translation.md`](../docs/16-d3d10-11-translation.md) — D3D10/11 details
- [`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md) — VGA/VBE boot compatibility

**Reference:**

- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md) — System architecture
- [`docs/11-browser-apis.md`](../docs/11-browser-apis.md) — WebGPU/WebGL2 browser integration

---

## Interface Contracts

### Display Output

```rust
pub trait DisplayOutput {
    fn get_framebuffer(&self) -> &[u32];
    fn get_resolution(&self) -> (u32, u32);
    fn present(&mut self);
}

pub trait GpuCommandProcessor {
    fn submit_commands(&mut self, commands: &[GpuCommand]);
    fn flush(&mut self);
}
```

### AeroGPU Device ↔ Driver Protocol

The AeroGPU Windows driver communicates with the emulator via a shared protocol. See:
- `drivers/protocol/` — Protocol definitions
- `emulator/protocol/` — Emulator-side protocol implementation

---

## Tasks

### VGA Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| VG-001 | VGA register emulation | P0 | None | High |
| VG-002 | Text mode rendering | P0 | VG-001 | Medium |
| VG-003 | Mode 13h (320x200x256) | P0 | VG-001 | Medium |
| VG-004 | Planar graphics modes | P1 | VG-001 | Medium |
| VG-005 | SVGA/VESA modes | P0 | VG-001 | High |
| VG-006 | VGA palette handling | P0 | VG-001 | Low |
| VG-007 | VGA DAC | P0 | VG-006 | Low |
| VG-008 | VGA BIOS interrupt handlers | P0 | VG-002 | Medium |

### AeroGPU Tasks (Boot VGA + WDDM)

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| AeroGPU-EMU-DEV-001 | Base AeroGPU PCI device model (BARs, interrupts, MMIO) | P0 | DM-007, DM-008 | High |
| AeroGPU-EMU-DEV-002 | VGA legacy decode + VBE LFB modes + scanout handoff | P0 | AeroGPU-EMU-DEV-001, VG-005 | High |
| AeroGPU-EMU-DEV-003 | WDDM scanout registers + present path (canvas) | P0 | AeroGPU-EMU-DEV-001 | High |

### DirectX-9 Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| D9-001 | DXBC bytecode parser | P0 | None | High |
| D9-002 | Shader model 2.0 translation | P0 | D9-001 | High |
| D9-003 | Shader model 3.0 translation | P0 | D9-002 | High |
| D9-004 | Vertex shader support | P0 | D9-002 | High |
| D9-005 | Pixel shader support | P0 | D9-002 | High |
| D9-006 | Render state translation | P0 | None | High |
| D9-007 | Texture format translation | P0 | None | Medium |
| D9-008 | Texture sampling | P0 | D9-007 | Medium |
| D9-009 | Render target management | P0 | None | Medium |
| D9-010 | Depth/stencil buffer | P0 | None | Medium |
| D9-011 | Blend state | P0 | None | Medium |
| D9-012 | D3D9 test suite | P0 | D9-001..D9-011 | High |
| D9-013 | D3D9Ex API surface (DWM path) | P0 | D9-009, D9-012 | High |
| D9-014 | Ex present stats + fences + shared surfaces | P0 | D9-013 | High |
| D9-015 | D3D9Ex test app + integration test | P0 | D9-014 | Medium |

### DirectX-10/11 Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| D1-001 | Extend DXBC parser for SM4/SM5 | P1 | D9-001 | High |
| D1-002 | Shader model 4.0 VS/PS translation | P1 | D1-001 | High |
| D1-003 | Shader model 5.0 VS/PS translation | P1 | D1-002 | High |
| D1-004 | Constant buffers binding | P1 | WG-004, D1-001 | Medium |
| D1-005 | Resource views: SRV/RTV/DSV | P1 | WG-005 | High |
| D1-006 | Input layouts + semantic mapping | P1 | D1-001, WG-002 | High |
| D1-007 | Blend/depth/rasterizer state objects | P1 | WG-002 | High |
| D1-008 | DrawIndexed + instancing + indirect | P1 | WG-002, WG-004 | Medium |
| D1-009 | Synchronization (queries/fences) | P1 | WG-008 | Medium |
| D1-010 | Geometry shader support | P1 | D1-003 | High |
| D1-011 | Structured buffers + UAV | P2 | D1-003, WG-004, WG-005 | High |
| D1-012 | Compute shaders + dispatch | P2 | D1-003, WG-003 | High |
| D1-013 | Tessellation shaders (HS/DS) | P2 | D1-012 | Very High |
| D1-014 | D3D10/11 conformance suite | P1 | D1-002..D1-009 | High |

### WebGPU Backend Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| WG-001 | WebGPU device initialization | P0 | None | Low |
| WG-002 | Render pipeline creation | P0 | WG-001 | Medium |
| WG-003 | Compute pipeline creation | P1 | WG-001 | Medium |
| WG-004 | Buffer management | P0 | WG-001 | Medium |
| WG-005 | Texture management | P0 | WG-001 | Medium |
| WG-006 | WGSL shader library | P0 | None | High |
| WG-007 | Draw call batching | P1 | WG-002 | Medium |
| WG-008 | Framebuffer presentation | P0 | WG-002 | Medium |
| WG-009 | WebGL2 fallback | P2 | None | Very High |
| WG-010 | Persistent GPU cache (IndexedDB/OPFS) | P1 | WG-001 | Medium |

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
bash ./scripts/safe-run.sh cargo test -p aero-gpu --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d9 --locked
bash ./scripts/safe-run.sh cargo test -p aero-d3d11 --locked
bash ./scripts/safe-run.sh cargo test -p aero-dxbc --locked

# Run D3D9 integration test
bash ./scripts/safe-run.sh cargo test -p aero --test d3d9_blend_depth_stencil --locked
bash ./scripts/safe-run.sh cargo test -p aero --test d3d9_vertex_input --locked

# Run D3D11 smoke test
bash ./scripts/safe-run.sh cargo test -p aero --test d3d11_smoke --locked
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
