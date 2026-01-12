# 15 - Task Breakdown & Work Organization

## Overview

This document breaks down Aero development into parallelizable work items. Tasks are organized by functional area, priority, and dependencies. This breakdown can inform how work might be distributed and parallelized.

## Windows Driver Implementation Notes

- [Virtio PCI (Modern) Interrupts on Windows 7 (KMDF)](./windows/virtio-pci-modern-interrupts.md)

---

## Suggested Work Organization

```
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Functional Areas                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  CORE                                                           │
│  ├── CPU-Decoder                                                │
│  ├── CPU-Interpreter                                            │
│  ├── CPU-JIT                                                    │
│  └── Memory                                                     │
│                                                                  │
│  GRAPHICS                                                       │
│  ├── VGA                                                        │
│  ├── DirectX-9                                                  │
│  ├── DirectX-10/11                                              │
│  └── WebGPU-Backend                                             │
│                                                                  │
│  I/O                                                            │
│  ├── Storage                                                    │
│  ├── Network                                                    │
│  ├── Audio                                                      │
│  └── Input                                                      │
│                                                                  │
│  FIRMWARE                                                       │
│  ├── BIOS                                                       │
│  ├── ACPI                                                       │
│  └── Device-Models                                              │
│                                                                  │
│  PERFORMANCE                                                    │
│  ├── Profiling                                                  │
│  └── Optimization                                               │
│                                                                  │
│  INFRASTRUCTURE                                                 │
│  ├── Build                                                      │
│  ├── Testing                                                    │
│  └── Browser-Compat                                             │
│                                                                  │
│  SERVICE / DEPLOYMENT / SECURITY                                │
│  ├── Disk-Auth                                                  │
│  ├── Disk-Gateway                                               │
│  ├── Upload/Import                                              │
│  └── CDN (CloudFront)                                           │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Interface Contracts

Components should adhere to these interfaces for integration:

### CPU ↔ Memory Interface
 
```rust
// Canonical CPU bus trait used by `aero_cpu_core` Tier-0 + JIT.
//
// See: `aero_cpu_core::mem::CpuBus` (`crates/aero-cpu-core/src/mem.rs`)
//
// Note: this is intentionally abridged; the real trait also includes scalar
// reads/writes, bulk byte operations, `atomic_rmw` (write-intent semantics),
// and `preflight_write_bytes` (used to keep multi-byte writes fault-atomic).
pub trait CpuBus {
    /// Sync paging/MMU view with architectural state (CR0/CR3/CR4/EFER/CPL).
    fn sync(&mut self, state: &aero_cpu_core::state::CpuState) {}
    /// Invalidate a single translation (INVLPG).
    fn invlpg(&mut self, vaddr: u64) {}

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception>;
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception>;

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception>;
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception>;
    fn io_write(
        &mut self,
        port: u16,
        size: u32,
        val: u64,
    ) -> Result<(), aero_cpu_core::Exception>;
}
```
 
### CPU ↔ Device Interface
 
```rust
// Port IO is modeled directly on the CPU bus via `CpuBus::io_read/io_write`.
//
// Architectural interrupt/exception delivery is handled by `aero_cpu_core::CpuCore`
// (wrapper around `state::CpuState` + `interrupts::PendingEventState` + `time::TimeSource`).
// External interrupts are injected by queuing a vector in `PendingEventState` and delivered
// at instruction boundaries.
```

### Graphics Interface

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

---

## CORE Tasks

### CPU-Decoder Tasks


| ID     | Task                                        | Priority | Dependencies | Complexity |
| ------ | ------------------------------------------- | -------- | ------------ | ---------- |
| CD-001 | Implement prefix parsing (legacy, REX, VEX) | P0       | None         | Medium     |
| CD-002 | Implement 1-byte opcode table               | P0       | CD-001       | High       |
| CD-003 | Implement 2-byte opcode table (0F xx)       | P0       | CD-001       | High       |
| CD-004 | Implement 3-byte opcode tables              | P1       | CD-003       | Medium     |
| CD-005 | Implement ModR/M + SIB parsing              | P0       | None         | Medium     |
| CD-006 | Implement displacement/immediate parsing    | P0       | CD-005       | Low        |
| CD-007 | Implement VEX/EVEX prefix handling          | P1       | CD-001       | Medium     |
| CD-008 | SSE instruction decoding                    | P0       | CD-003       | High       |
| CD-009 | AVX instruction decoding                    | P2       | CD-007       | High       |
| CD-010 | Decoder test suite                          | P0       | CD-002       | Medium     |


### CPU-Interpreter Tasks


| ID     | Task                                         | Priority | Dependencies   | Complexity |
| ------ | -------------------------------------------- | -------- | -------------- | ---------- |
| CI-001 | Data movement instructions (MOV, PUSH, POP)  | P0       | CD-002         | Medium     |
| CI-002 | Arithmetic instructions (ADD, SUB, MUL, DIV) | P0       | CD-002         | High       |
| CI-003 | Logical instructions (AND, OR, XOR, NOT)     | P0       | CD-002         | Medium     |
| CI-004 | Shift/rotate instructions                    | P0       | CD-002         | Medium     |
| CI-005 | Control flow (JMP, CALL, RET, Jcc)           | P0       | CD-002         | Medium     |
| CI-006 | String instructions (MOVS, STOS, CMPS)       | P0       | CD-002         | Medium     |
| CI-007 | Bit manipulation (BT, BTS, BSF, BSR)         | P1       | CD-002         | Medium     |
| CI-008 | System instructions (INT, IRET, SYSCALL)     | P0       | CD-002         | High       |
| CI-009 | Privileged instructions (MOV CR/DR, LGDT)    | P0       | CD-002         | Medium     |
| CI-010 | x87 FPU instructions                         | P1       | CD-002         | Very High  |
| CI-011 | SSE instructions (scalar)                    | P0       | CD-008         | High       |
| CI-012 | SSE instructions (packed)                    | P0       | CD-008         | Very High  |
| CI-013 | SSE2 instructions                            | P0       | CD-008         | High       |
| CI-014 | SSE3/SSSE3 instructions                      | P1       | CI-012         | Medium     |
| CI-015 | SSE4.1/4.2 instructions                      | P1       | CI-014         | Medium     |
| CI-016 | Flag computation (lazy evaluation)           | P0       | CI-002         | Medium     |
| CI-017 | Interpreter test suite                       | P0       | CI-001..CI-009 | Very High  |


### CPU-JIT Tasks


| ID     | Task                                    | Priority | Dependencies   | Complexity |
| ------ | --------------------------------------- | -------- | -------------- | ---------- |
| CJ-001 | Basic block detection                   | P0       | CD-010         | Medium     |
| CJ-002 | IR (intermediate representation) design | P0       | None           | High       |
| CJ-003 | x86 → IR translation                    | P0       | CJ-001, CJ-002 | Very High  |
| CJ-004 | IR → WASM code generation               | P0       | CJ-002         | Very High  |
| CJ-005 | Code cache management                   | P0       | CJ-004         | Medium     |
| CJ-006 | Execution counter / hot path detection  | P0       | None           | Medium     |
| CJ-007 | Baseline JIT (Tier 1)                   | P0       | CJ-003, CJ-004 | High       |
| CJ-008 | Constant folding optimization           | P1       | CJ-002         | Medium     |
| CJ-009 | Dead code elimination                   | P1       | CJ-002         | Medium     |
| CJ-010 | Common subexpression elimination        | P1       | CJ-002         | Medium     |
| CJ-011 | Flag elimination optimization           | P1       | CJ-002         | Medium     |
| CJ-012 | Register allocation                     | P1       | CJ-002         | High       |
| CJ-013 | Optimizing JIT (Tier 2)                 | P1       | CJ-008..CJ-012 | Very High  |
| CJ-014 | SIMD code generation                    | P1       | CJ-004         | High       |
| CJ-015 | JIT test suite                          | P0       | CJ-007         | High       |
| CJ-016 | Inline RAM loads/stores via JIT TLB fast-path | P0  | CJ-004, MM-012 | High       |
| CJ-017 | MMIO/IO exits for JIT memory ops             | P0  | CJ-016, MM-003 | Medium     |


### Memory Tasks


| ID     | Task                                 | Priority | Dependencies | Complexity |
| ------ | ------------------------------------ | -------- | ------------ | ---------- |
| MM-001 | Physical memory allocation           | P0       | None         | Low        |
| MM-002 | Memory bus routing                   | P0       | MM-001       | Medium     |
| MM-003 | MMIO region management               | P0       | MM-002       | Medium     |
| MM-004 | 32-bit paging                        | P0       | MM-002       | Medium     |
| MM-005 | PAE paging                           | P0       | MM-004       | Medium     |
| MM-006 | 4-level paging (long mode)           | P0       | MM-005       | Medium     |
| MM-007 | TLB implementation                   | P0       | MM-006       | High       |
| MM-008 | TLB invalidation (INVLPG, CR3 write) | P0       | MM-007       | Medium     |
| MM-009 | Page fault handling                  | P0       | MM-006       | Medium     |
| MM-010 | Sparse memory allocation             | P1       | MM-001       | Medium     |
| MM-011 | Memory test suite                    | P0       | MM-006       | Medium     |
| MM-012 | JIT-visible TLB layout (stable offsets, packed entries) | P0 | MM-007 | Medium |
| MM-013 | `mmu_translate` helper for JIT (page walk + fill TLB) | P0 | MM-006..MM-012 | High |
| MM-014 | MMIO classification/epoch for JIT fast-path safety          | P0 | MM-003, MM-012 | Medium |


---

## GRAPHICS Tasks

### VGA Tasks


| ID     | Task                        | Priority | Dependencies | Complexity |
| ------ | --------------------------- | -------- | ------------ | ---------- |
| VG-001 | VGA register emulation      | P0       | None         | High       |
| VG-002 | Text mode rendering         | P0       | VG-001       | Medium     |
| VG-003 | Mode 13h (320x200x256)      | P0       | VG-001       | Medium     |
| VG-004 | Planar graphics modes       | P1       | VG-001       | Medium     |
| VG-005 | SVGA/VESA modes             | P0       | VG-001       | High       |
| VG-006 | VGA palette handling        | P0       | VG-001       | Low        |
| VG-007 | VGA DAC                     | P0       | VG-006       | Low        |
| VG-008 | VGA BIOS interrupt handlers | P0       | VG-002       | Medium     |

### AeroGPU Tasks (Boot VGA + WDDM)

These tasks wire the generic VGA/VBE work into the **AeroGPU virtual PCI device** so Windows 7 can boot and install without a second “legacy VGA” adapter.

See: [AeroGPU Legacy VGA/VBE Compatibility](./16-aerogpu-vga-vesa-compat.md)

| ID                   | Task                                                                 | Priority | Dependencies                 | Complexity |
| -------------------- | -------------------------------------------------------------------- | -------- | ---------------------------- | ---------- |
| AeroGPU-EMU-DEV-001  | Base AeroGPU PCI device model (BARs, interrupts, MMIO register space) | P0       | DM-007, DM-008               | High       |
| AeroGPU-EMU-DEV-002  | VGA legacy decode + VBE LFB modes + scanout handoff to WDDM           | P0       | AeroGPU-EMU-DEV-001, VG-005  | High       |
| AeroGPU-EMU-DEV-003  | WDDM scanout registers + present path (canvas)                        | P0       | AeroGPU-EMU-DEV-001          | High       |


### DirectX-9 Tasks


| ID     | Task                         | Priority | Dependencies   | Complexity |
| ------ | ---------------------------- | -------- | -------------- | ---------- |
| D9-001 | DXBC bytecode parser         | P0       | None           | High       |
| D9-002 | Shader model 2.0 translation | P0       | D9-001         | High       |
| D9-003 | Shader model 3.0 translation | P0       | D9-002         | High       |
| D9-004 | Vertex shader support        | P0       | D9-002         | High       |
| D9-005 | Pixel shader support         | P0       | D9-002         | High       |
| D9-006 | Render state translation     | P0       | None           | High       |
| D9-007 | Texture format translation   | P0       | None           | Medium     |
| D9-008 | Texture sampling             | P0       | D9-007         | Medium     |
| D9-009 | Render target management     | P0       | None           | Medium     |
| D9-010 | Depth/stencil buffer         | P0       | None           | Medium     |
| D9-011 | Blend state                  | P0       | None           | Medium     |
| D9-012 | D3D9 test suite              | P0       | D9-001..D9-011 | High       |
| D9-013 | D3D9Ex API surface (DWM path) | P0       | D9-009, D9-012 | High       |
| D9-014 | Ex present stats + fences + shared surfaces | P0       | D9-013         | High       |
| D9-015 | D3D9Ex test app + integration test | P0       | D9-014         | Medium     |


### DirectX-10/11 Tasks
 
 
| ID     | Task                                                                  | Priority | Dependencies           | Complexity |
| ------ | --------------------------------------------------------------------- | -------- | ---------------------- | ---------- |
| D1-001 | Extend DXBC parser for SM4/SM5 (SHEX/SHDR + ISGN/OSGN/RDEF reflection) | P1       | D9-001                 | High       |
| D1-002 | Shader model 4.0 VS/PS translation (core ALU/flow/sampling)            | P1       | D1-001                 | High       |
| D1-003 | Shader model 5.0 VS/PS translation (typed resources, integer ops)      | P1       | D1-002                 | High       |
| D1-004 | Constant buffers (cbuffers) binding + dynamic update/renaming          | P1       | WG-004, D1-001         | Medium     |
| D1-005 | Resource views: SRV/RTV/DSV (textures + texture arrays)                | P1       | WG-005                 | High       |
| D1-006 | Input layouts + semantic→location mapping + instancing step rate        | P1       | D1-001, WG-002         | High       |
| D1-007 | Blend/depth/rasterizer state objects                                   | P1       | WG-002                 | High       |
| D1-008 | DrawIndexed/baseVertex + instancing + indirect draws                   | P1       | WG-002, WG-004         | Medium     |
| D1-009 | Synchronization primitives (queries/event queries/fences)              | P1       | WG-008                 | Medium     |
| D1-010 | Geometry shader support (lowering/emulation path)                      | P1       | D1-003                 | High       |
| D1-011 | Structured buffers + UAV support (storage buffers/textures)            | P2       | D1-003, WG-004, WG-005 | High       |
| D1-012 | Compute shaders + dispatch                                              | P2       | D1-003, WG-003         | High       |
| D1-013 | Tessellation shaders (HS/DS) emulation                                  | P2       | D1-012                 | Very High  |
| D1-014 | D3D10/11 conformance suite (pixel compare scenes + perf sanity)        | P1       | D1-002..D1-009         | High       |


### WebGPU Backend Tasks


| ID     | Task                         | Priority | Dependencies | Complexity |
| ------ | ---------------------------- | -------- | ------------ | ---------- |
| WG-001 | WebGPU device initialization | P0       | None         | Low        |
| WG-002 | Render pipeline creation     | P0       | WG-001       | Medium     |
| WG-003 | Compute pipeline creation    | P1       | WG-001       | Medium     |
| WG-004 | Buffer management            | P0       | WG-001       | Medium     |
| WG-005 | Texture management           | P0       | WG-001       | Medium     |
| WG-006 | WGSL shader library          | P0       | None         | High       |
| WG-007 | Draw call batching           | P1       | WG-002       | Medium     |
| WG-008 | Framebuffer presentation     | P0       | WG-002       | Medium     |
| WG-009 | WebGL2 fallback              | P2       | None         | Very High  |
| WG-010 | Persistent GPU cache (shader translations + reflection, IndexedDB/OPFS, versioned keys, LRU, telemetry, clear API) | P1 | WG-001 | Medium |

#### Implementation notes (WG-001..WG-009)

These are clarifying notes only (no ID/priority changes). See
[16 - Browser GPU Backends (WebGPU-first + WebGL2 Fallback)](./16-browser-gpu-backends.md)
for the full browser backend design.

- **WG-001 (device initialization):**
  - Implement backend selection as `auto|webgpu|webgl2` with a user override.
  - Run GPU initialization inside the GPU worker using `OffscreenCanvas`.
  - Treat most WebGPU features as optional; negotiate from `adapter.features()`/limits.
- **WG-002 (render pipeline creation):**
  - Prefer pipeline caching keyed by translated state + shader IDs.
  - Keep a small “blit/present” pipeline always available (used by both backends).
- **WG-003 (compute pipeline creation):**
  - Only available in WebGPU mode; define CPU fallbacks for WebGL2 mode.
  - Gate all compute usage behind capability flags.
- **WG-004 (buffer management):**
  - Design for streaming updates (ring buffers/staging) rather than frequent map/unmap.
  - Avoid patterns that rely on storage buffers when targeting WebGL2 fallback.
- **WG-005 (texture management):**
  - Standardize on a small “portable” set of formats (RGBA8 + depth where possible).
  - Treat BCn/DXT as an optional fast-path; fall back to CPU decompression.
- **WG-006 (WGSL shader library):**
  - Keep a WebGL2-compatible shader subset for shared shaders (present/blit/debug).
  - Avoid WGSL features that cannot be lowered to GLSL ES 3.0.
- **WG-007 (draw call batching):**
  - Batch by pipeline/material state to reduce pipeline switches and bind updates.
  - Prefer command-buffer-friendly batching that maps cleanly to WebGPU.
- **WG-008 (framebuffer presentation):**
  - Implement “swapchain + blit”: render into an internal texture, then blit to canvas.
  - Handle resize by reconfiguring the surface and regenerating dependent resources.
- **WG-009 (WebGL2 fallback):**
  - Scope the fallback to framebuffer presentation + minimal render paths.
  - Document and enforce feature gaps (no compute, limited formats, restricted bindings).


---

## I/O Tasks

### Storage Tasks


| ID     | Task                       | Priority | Dependencies | Complexity |
| ------ | -------------------------- | -------- | ------------ | ---------- |
| ST-001 | IDE controller emulation   | P0       | None         | High       |
| ST-002 | AHCI controller emulation  | P0       | None         | Very High  |
| ST-003 | Disk image abstraction     | P0       | None         | Medium     |
| ST-004 | OPFS backend               | P0       | ST-003       | Medium     |
| ST-005 | IndexedDB fallback backend | P1       | ST-003       | Medium     |
| ST-006 | Sector caching             | P1       | ST-003       | Medium     |
| ST-007 | Sparse disk format         | P1       | ST-003       | Medium     |
| ST-008 | CD-ROM/ATAPI emulation     | P0       | ST-001       | High       |
| ST-009 | Virtio-blk driver (Win7) (see VIO-011) | P1 | VIO-001..VIO-003 | High    |
| ST-010 | Storage test suite         | P0       | ST-002       | Medium     |

Note: IndexedDB-based storage is async and is not currently exposed as a synchronous
`aero_storage::StorageBackend` / `aero_storage::VirtualDisk`. See
[`docs/19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md).


### Network Tasks


| ID     | Task                     | Priority | Dependencies | Complexity |
| ------ | ------------------------ | -------- | ------------ | ---------- |
| NT-001 | E1000 NIC emulation      | P0       | None         | Very High  |
| NT-002 | Packet receive/transmit  | P0       | NT-001       | Medium     |
| NT-003 | User-space network stack | P0       | None         | High       |
| NT-004 | DHCP client              | P0       | NT-003       | Medium     |
| NT-005 | DNS resolution (DoH)     | P1       | None         | Medium     |
| NT-006 | WebSocket TCP proxy      | P0       | None         | Medium     |
| NT-007 | WebRTC UDP proxy         | P1       | None         | High       |
| NT-008 | Virtio-net driver (Win7) (see VIO-012) | P1 | VIO-001..VIO-003 | High    |
| NT-009 | Network test suite       | P0       | NT-001       | Medium     |

Implementation references:

- Aero Gateway backend contract (TCP proxy + DoH): [`docs/backend/01-aero-gateway-api.md`](./backend/01-aero-gateway-api.md) (OpenAPI: [`docs/backend/openapi.yaml`](./backend/openapi.yaml))
- Gateway implementation: `backend/aero-gateway`
- UDP relay (WebRTC + WebSocket fallback, v1/v2 datagram framing): `proxy/webrtc-udp-relay` (protocol: [`proxy/webrtc-udp-relay/PROTOCOL.md`](../proxy/webrtc-udp-relay/PROTOCOL.md))
- Local development relay: `net-proxy/` (supports `/tcp`, `/tcp-mux`, and `/udp`)

### Audio Tasks


| ID     | Task                               | Priority | Dependencies | Complexity |
| ------ | ---------------------------------- | -------- | ------------ | ---------- |
| AU-001 | HD Audio controller emulation      | P0       | None         | Very High  |
| AU-002 | HDA codec emulation                | P0       | AU-001       | High       |
| AU-003 | Sample format conversion           | P0       | None         | Medium     |
| AU-004 | AudioWorklet integration           | P0       | None         | Medium     |
| AU-005 | Audio buffering/latency management | P0       | AU-004       | Medium     |
| AU-006 | AC'97 fallback (legacy; `emulator/legacy-audio`) | P2 | None     | High       |
| AU-007 | Audio input (microphone)           | P2       | AU-004       | Medium     |
| AU-008 | Audio test suite                   | P0       | AU-001       | Medium     |


### Input Tasks


| ID     | Task                     | Priority | Dependencies   | Complexity |
| ------ | ------------------------ | -------- | -------------- | ---------- |
| IN-001 | PS/2 controller (i8042)  | P0       | None           | Medium     |
| IN-002 | PS/2 keyboard            | P0       | IN-001         | Medium     |
| IN-003 | PS/2 mouse               | P0       | IN-001         | Medium     |
| IN-004 | Scancode translation     | P0       | None           | Medium     |
| IN-005 | Browser event capture    | P0       | None           | Medium     |
| IN-006 | Pointer Lock integration | P0       | IN-005         | Low        |
| IN-007 | USB HID (keyboard)       | P2       | None           | Medium     |
| IN-008 | USB HID (mouse)          | P2       | None           | Medium     |
| IN-009 | Gamepad support          | P2       | None           | Medium     |
| IN-010 | Input test suite         | P0       | IN-001..IN-003 | Medium     |
| IN-011 | Virtio-input (keyboard/mouse) device model (device config + event/status queues) | P1 | DM-008, VTP-002 | High     |
| IN-012 | Windows 7 virtio-input (keyboard/mouse) KMDF HID minidriver (see VIO-010) | P1 | VIO-001..VIO-003 | Very High |
| IN-013 | HID report descriptor + keyboard/mouse mapping (see VIO-013)      | P1 | IN-012         | High      |
| IN-014 | Driver packaging/signing + installation docs (see VIO-014)        | P1 | IN-012, IN-013 | Medium    |
| IN-015 | Browser events → virtio-input events (EV_KEY/EV_REL + SYN)        | P1 | IN-005..IN-006, IN-011 | Medium |
| IN-016 | Virtio-input (keyboard/mouse) functional test plan/tooling (see VIO-015) | P1 | IN-011..IN-015 | Medium    |

### Virtio Drivers (Windows 7 guest)

Virtio device-specific drivers (virtio-blk/net/input/etc.) should **not** each reinvent the transport layer. Any Windows 7 virtio driver work presumes shared foundations exist first:

- **Virtio 1.0 PCI modern transport** (capability discovery, BAR mapping, feature negotiation)
- **Virtqueue split-ring** implementation (descriptor/avail/used rings and DMA-safe memory)
- **Interrupt handling** (MSI-X and legacy INTx plumbing)

See `docs/16-virtio-drivers-win7.md` for an implementation-oriented overview of these building blocks.

| ID      | Task                                       | Priority | Dependencies        | Complexity |
| ------- | ------------------------------------------ | -------- | ------------------- | ---------- |
| VIO-001 | Virtio-pci modern transport library (shared) | P0     | None                | High       |
| VIO-002 | Virtqueue split-ring implementation (shared) | P0     | VIO-001             | Very High  |
| VIO-003 | MSI-X + legacy interrupt plumbing (shared)   | P0     | VIO-001             | High       |
| VIO-010 | Virtio-input KMDF HID minidriver (Win7) (keyboard/mouse) (Input lane: IN-012) | P1 | VIO-001..VIO-003 | Very High |
| VIO-011 | Virtio-blk driver (Win7) (Storage lane: ST-009) | P1   | VIO-001..VIO-003    | High       |
| VIO-012 | Virtio-net driver (Win7) (Network lane: NT-008) | P1   | VIO-001..VIO-003    | High       |
| VIO-013 | Virtio-input HID report descriptor + key mapping (Input lane: IN-013) | P1 | VIO-010         | High       |
| VIO-014 | Virtio-input packaging/signing + installation docs (Input lane: IN-014) | P1 | VIO-010, VIO-013 | Medium    |
| VIO-015 | Virtio-input functional test plan/tooling (Input lane: IN-016) | P1 | VIO-010..VIO-014 | Medium    |

### Virtio PCI transport (device model / emulator side)

These tasks implement Aero’s virtio devices in the emulator. In addition to the modern virtio-pci transport used by Aero’s own Win7 drivers, we should also support **legacy/transitional virtio-pci** for maximum compatibility with older virtio-win drivers.

See: [`16-virtio-pci-legacy-transitional.md`](./16-virtio-pci-legacy-transitional.md)

| ID      | Task                                                                    | Priority | Dependencies            | Complexity |
| ------- | ----------------------------------------------------------------------- | -------- | ----------------------- | ---------- |
| VTP-001 | Virtio core (virtqueue, feature negotiation, device status machine)     | P0       | DM-007                  | High       |
| VTP-002 | Virtio PCI modern transport (virtio 1.0+ capabilities + MMIO layout)    | P0       | VTP-001, DM-007         | High       |
| VTP-003 | Virtio PCI legacy transport (virtio 0.9 I/O port BAR + PFN queues)      | P0       | VTP-001, DM-007         | High       |
| VTP-004 | Virtio PCI transitional device (expose both legacy + modern transports) | P0       | VTP-002, VTP-003        | Medium     |
| VTP-005 | Legacy INTx wiring + ISR read-to-clear semantics                         | P0       | VTP-003                 | Medium     |
| VTP-006 | MSI-X support for virtio PCI (recommended for virtio-net performance)   | P1       | VTP-002, DM-007         | High       |
| VTP-007 | Unit tests: legacy guest flow (feature negotiation, PFN setup, notify)  | P0       | VTP-003                 | Medium     |
| VTP-008 | Config option: disable modern caps (force legacy path for testing)      | P1       | VTP-004                 | Low        |


---

## Windows Guest Tools & Paravirtual Driver Tasks

These tasks cover the **Windows-side packaging** needed for paravirtual devices to work reliably (especially boot-critical storage), plus the cross-team contract that prevents PCI ID drift.

**Source of truth (must stay in sync with emulator + drivers):**

- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO`, definitive for virtio devices)
- [`windows-device-contract.md`](./windows-device-contract.md) / [`windows-device-contract.json`](./windows-device-contract.json) (binding summary + manifest; must match `AERO-W7-VIRTIO` for virtio)

| ID     | Task                                                                 | Priority | Dependencies | Complexity |
| ------ | -------------------------------------------------------------------- | -------- | ------------ | ---------- |
| GT-001 | Define/maintain the Windows PCI device contract (IDs, BAR usage, INF) | P0       | None         | Medium     |
| GT-002 | Guest Tools installer consumes `windows-device-contract.json`         | P0       | GT-001       | Medium     |
| GT-003 | Seed `CriticalDeviceDatabase` for boot-critical storage (virtio-blk)  | P0       | GT-002       | High       |
| GT-004 | Ensure each driver INF models exactly match contract hardware IDs     | P0       | GT-001       | Medium     |
| GT-005 | Add emulator CI check: PCI IDs emitted match `windows-device-contract.json` | P1  | GT-001       | Medium     |
| GT-006 | Versioning policy: bump contract version on breaking PCI/ABI changes  | P1       | GT-001       | Low        |


---

## FIRMWARE Tasks

### BIOS Tasks


| ID     | Task                         | Priority | Dependencies   | Complexity |
| ------ | ---------------------------- | -------- | -------------- | ---------- |
| BI-001 | POST sequence                | P0       | None           | Medium     |
| BI-002 | Memory detection (E820)      | P0       | BI-001         | Medium     |
| BI-003 | Interrupt vector table setup | P0       | BI-001         | Low        |
| BI-004 | BIOS data area setup         | P0       | BI-001         | Low        |
| BI-005 | INT 10h (video)              | P0       | None           | Medium     |
| BI-006 | INT 13h (disk)               | P0       | None           | Medium     |
| BI-007 | INT 15h (system)             | P0       | None           | Medium     |
| BI-008 | INT 16h (keyboard)           | P0       | None           | Low        |
| BI-009 | Boot device selection        | P0       | BI-006         | Low        |
| BI-010 | MBR/boot sector loading      | P0       | BI-009         | Low        |
| BI-011 | BIOS test suite              | P0       | BI-001..BI-010 | Medium     |


### ACPI Tasks


| ID     | Task                                   | Priority | Dependencies   | Complexity |
| ------ | -------------------------------------- | -------- | -------------- | ---------- |
| AC-001 | RSDP/RSDT/XSDT generation              | P0       | None           | Medium     |
| AC-002 | FADT (Fixed ACPI Description Table)    | P0       | AC-001         | Medium     |
| AC-003 | MADT (Multiple APIC Description Table) | P0       | AC-001         | Medium     |
| AC-004 | HPET table                             | P0       | AC-001         | Low        |
| AC-005 | DSDT (AML bytecode)                    | P1       | AC-001         | High       |
| AC-006 | Power management stubs                 | P1       | AC-002         | Medium     |
| AC-007 | ACPI test suite                        | P0       | AC-001..AC-004 | Medium     |


### Device Models Tasks


| ID     | Task                     | Priority | Dependencies   | Complexity |
| ------ | ------------------------ | -------- | -------------- | ---------- |
| DM-001 | PIC (8259A)              | P0       | None           | Medium     |
| DM-002 | PIT (8254)               | P0       | None           | Medium     |
| DM-003 | CMOS/RTC                 | P0       | None           | Medium     |
| DM-004 | Local APIC               | P0       | None           | High       |
| DM-005 | I/O APIC                 | P0       | DM-004         | High       |
| DM-006 | HPET                     | P0       | None           | Medium     |
| DM-007 | PCI configuration space  | P0       | None           | High       |
| DM-008 | PCI device enumeration   | P0       | DM-007         | Medium     |
| DM-009 | DMA controller (8237)    | P1       | None           | Medium     |
| DM-010 | Serial port (16550)      | P2       | None           | Medium     |
| DM-011 | Device models test suite | P0       | DM-001..DM-006 | Medium     |


---

## PERFORMANCE Tasks


| ID     | Task                         | Priority | Dependencies   | Complexity |
| ------ | ---------------------------- | -------- | -------------- | ---------- |
| PF-001 | Profiling infrastructure     | P0       | None           | Medium     |
| PF-002 | Instruction counter          | P0       | None           | Low        |
| PF-003 | Frame time tracking          | P0       | None           | Low        |
| PF-004 | Memory usage tracking        | P0       | None           | Low        |
| PF-005 | Hot path identification      | P1       | PF-001         | Medium     |
| PF-006 | JIT optimization analysis    | P1       | CJ-015, PF-001 | Medium     |
| PF-007 | Graphics bottleneck analysis | P1       | PF-001, WG-001..WG-002 | Medium     |
| PF-008 | Benchmark suite              | P0       | None           | High       |
| PF-009 | Regression tracking          | P0       | PF-008         | Medium     |

PF-008 specifically includes a **guest CPU instruction throughput** microbenchmark suite (no OS images) with checksum validation, perf export integration, and a Playwright scenario. See: [Guest CPU Instruction Throughput Benchmarks (PF-008)](./16-guest-cpu-benchmark-suite.md).


---

## INFRASTRUCTURE Tasks


| ID     | Task                            | Priority | Dependencies | Complexity |
| ------ | ------------------------------- | -------- | ------------ | ---------- |
| IF-001 | Project structure setup         | P0       | None         | Low        |
| IF-002 | Rust/WASM build configuration   | P0       | IF-001       | Medium     |
| IF-003 | CI/CD pipeline (GitHub Actions) | P0       | IF-002       | Medium     |
| IF-004 | Unit test framework             | P0       | IF-001       | Low        |
| IF-005 | Integration test framework      | P0       | IF-004       | Medium     |
| IF-006 | Browser test automation         | P0       | IF-002       | High       |
| IF-007 | Code coverage reporting         | P1       | IF-004       | Low        |
| IF-008 | Documentation generation        | P1       | None         | Low        |
| IF-009 | Release automation              | P1       | IF-003       | Medium     |
| IF-010 | Performance regression CI       | P1       | PF-008       | Medium     |


---

## SERVICE / DEPLOYMENT / SECURITY Tasks

These tasks cover the hosted-service components needed to support **user-provided disk images** (upload, storage, and streamed access) while meeting legal/security constraints.

| ID     | Task                                                                  | Priority | Dependencies         | Complexity |
| ------ | --------------------------------------------------------------------- | -------- | -------------------- | ---------- |
| HS-001 | Disk image lifecycle + access control spec (`docs/17-*`)               | P0       | None                 | Medium     |
| HS-002 | Disk image streaming authentication spec (`docs/16-*`)                 | P0       | HS-001               | Medium     |
| HS-003 | `disk-gateway` server (Range GET, auth verification, CORS)             | P0       | HS-001, HS-002       | High       |
| HS-004 | Upload/import pipeline (ingest, validate, store, attach to lifecycle)  | P0       | HS-001               | High       |
| HS-005 | CDN profiles (CloudFront) for disk streaming (`docs/deployment/*`)     | P1       | HS-002, HS-003       | Medium     |
| HS-006 | Hosted-service conformance tests (contract tests for gateway + auth)   | P1       | HS-003, HS-004, HS-005 | Medium   |
| HS-007 | Browser E2E tests for disk streaming + auth failures                   | P1       | HS-003, IF-006       | High       |

---

## Dependency Graph (Simplified)

```
┌─────────────────────────────────────────────────────────────────┐
│                    Task Dependencies                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Phase 1: Foundation                                             │
│  ┌──────────┐                                                   │
│  │ IF-001/2 │──┬──▶ CD-001 ──▶ CD-002..009                     │
│  └──────────┘  │       │                                        │
│                │       ▼                                        │
│                ├──▶ CI-001..017                                 │
│                │       │                                        │
│                │       ▼                                        │
│                └──▶ MM-001..006                                 │
│                                                                  │
│  Phase 2: Core                                                   │
│  ┌──────────┐                                                   │
│  │CD-010 ───┼──▶ CJ-001..007 ──▶ CJ-008..015                   │
│  └──────────┘                                                   │
│                                                                  │
│  Phase 3: Graphics + I/O (can run in parallel)                  │
│  ┌──────────┐     ┌──────────┐     ┌──────────┐                │
│  │ VG-001   │     │ ST-001   │     │ NT-001   │                │
│  └────┬─────┘     └────┬─────┘     └────┬─────┘                │
│       │                │                │                       │
│       ▼                ▼                ▼                       │
│  ┌──────────┐     ┌──────────┐     ┌──────────┐                │
│  │ D9-001   │     │ AU-001   │     │ IN-001/11│                │
│  └────┬─────┘     └────┬─────┘     └────┬─────┘                │
│       │                │                │                       │
│       ▼                ▼                ▼                       │
│  ┌──────────┐                                                   │
│  │ D1-001   │                                                   │
│  └──────────┘                                                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Parallel Execution Lanes

### Maximum Parallelization

At any given time, these work streams can proceed independently:

**Phase 1 (8+ parallel lanes):**

1. Instruction decoder (CD-001..010)
2. Data movement instructions (CI-001)
3. Arithmetic instructions (CI-002)
4. Logical instructions (CI-003)
5. Memory bus (MM-001..003)
6. Paging (MM-004..006)
7. BIOS POST (BI-001..003)
8. Infrastructure (IF-001..005)

**Phase 2 (6+ parallel lanes):**

1. JIT framework (CJ-001..007)
2. SSE instructions (CI-011..015)
3. System instructions (CI-008..009)
4. TLB (MM-007..009)
5. Device models (DM-001..006)
6. ACPI tables (AC-001..004)

**Phase 3 (10+ parallel lanes):**

1. VGA (VG-001..008)
2. DirectX 9 parser (D9-001..003)
3. DirectX 9 state (D9-006..011)
4. WebGPU backend (WG-001..008)
5. AHCI (ST-001..002)
6. Storage cache (ST-006..007)
7. Network (NT-001..008)
8. Audio (AU-001..005)
9. Input (IN-001..006, IN-011..016)
10. Profiling (PF-001..005)

---

## Suggested Work Principles

### Interface-Driven Design

- Assign ownership by interface boundaries
- Clear ownership prevents conflicts
- Use defined interfaces to minimize cross-dependencies
- Mock dependencies early in development

### Development Approach Suggestions

- **Understand Interface Contract First** - read the trait/interface definition
- **Test-Driven Development** - write tests before implementation
- **Document Edge Cases** - note undocumented behavior
- **Iterative Reviews** - get feedback early rather than waiting until completion

### Priority Ordering

- P0 tasks before P1 - these are on the critical path
- Unblock other work streams first where possible

---

## Task Status Tracking

### Status Definitions


| Status         | Meaning                      |
| -------------- | ---------------------------- |
| 🔴 Not Started | Task not yet begun           |
| 🟡 In Progress | Currently being worked on    |
| 🟢 Complete    | Implementation finished      |
| ✅ Verified     | Tests passing, code reviewed |
| 🔵 Blocked     | Waiting on dependency        |


### Progress Template

```
## Week N Progress Report

### Core Team
- CD-001: ✅ Complete
- CD-002: 🟡 In progress (80%)
- CI-001: 🟢 Complete, awaiting review

### Graphics Team
- VG-001: 🟡 In progress (50%)
- D9-001: 🔴 Not started (blocked by CD-010)

### Blockers
- D9-001 blocked on instruction decoder completion
- NT-001 needs interface clarification

### Next Week Goals
- Complete CD-002, CD-003
- Begin CJ-001 (JIT framework)
```

---

## Coordination Points

### Key Sync Topics

- Task status and blockers
- Dependency resolution
- Priority adjustments

### Architecture Reviews

- Interface changes may require cross-functional review
- Performance-critical decisions
- Security considerations

---

*This document should be updated as tasks are completed and priorities shift.*
