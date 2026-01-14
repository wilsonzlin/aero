# 04 - Graphics Subsystem (current implementation)

This document is an **architecture overview of the graphics/presentation code that exists today**.
It intentionally avoids aspirational pseudocode; **the referenced code is the source of truth**.

For the current “what works today vs what’s missing for Win7 usability” checklist, see:

- [`docs/graphics/status.md`](./graphics/status.md)

If you are looking for deeper background on the AeroGPU/WDDM direction, start with:

- `docs/graphics/win7-wddm11-aerogpu-driver.md` (Windows driver model + guest strategy)
- `docs/16-gpu-command-abi.md` and `docs/abi/gpu-command-protocol.md` (command ABI/protocol)
- `docs/graphics/aerogpu-protocols.md` (map of similarly named in-tree “GPU protocol” docs)

## What exists today (summary)

### 1) Boot/early display: VGA + VBE

The canonical machine provides a legacy boot display using a **VGA + Bochs VBE (“VBE_DISPI”)** device model:

- Device model: `crates/aero-gpu-vga` (`aero_gpu_vga::VgaDevice`)
- Canonical machine wiring + host-facing `display_present()` API:
  - `crates/aero-machine/src/lib.rs` (see `MachineConfig::enable_vga`, VGA/VBE port+MMIO wiring, and `Machine::display_present`)

Canonical machine GPU device modes (today):

- Boot graphics path (`enable_vga=true`, `enable_aerogpu=false`): `aero_gpu_vga::VgaDevice`. When the PC platform is enabled (`enable_pc_platform=true`), the machine also exposes a transitional Bochs/QEMU “Standard VGA”-like PCI function at **`00:0c.0`** (`VID:DID = 1234:1111`) used for VBE linear framebuffer (LFB) routing.
- AeroGPU device (MVP; `enable_aerogpu=true`, `enable_vga=false`): requires `enable_pc_platform=true`.
  - Exposes the canonical AeroGPU PCI identity at **`00:07.0`** (`VID:DID = A3A0:0001`).
  - Wires BAR1-backed VRAM (legacy VGA window aliasing / VBE compatibility mapping).
  - Exposes a minimal BAR0 MMIO surface used for bring-up (ABI/features, ring+fence transport with a no-op executor + IRQs, scanout0/cursor registers, vblank counters; implementation: `crates/aero-machine/src/aerogpu.rs`).
  - On the Rust side, the host can call `Machine::display_present()` to update a host-visible RGBA framebuffer cache (`Machine::display_framebuffer()` / `Machine::display_resolution()`).
    - In AeroGPU mode (no standalone VGA device model), `display_present()` presents (priority order) the WDDM scanout0 framebuffer if claimed, otherwise the BIOS VBE LFB, otherwise BIOS text mode (see `Machine::display_present` in `crates/aero-machine/src/lib.rs`).
  - The full AeroGPU command execution model is not implemented in `aero-machine` yet.
  - The full emulator-side device model lives at `crates/emulator/src/devices/pci/aerogpu.rs` and is not the canonical browser machine today (tracked for extraction in [`docs/21-emulator-crate-migration.md`](./21-emulator-crate-migration.md)).

### 2) Browser presentation: shared-memory framebuffer → GPU worker → canvas

In the browser runtime, the “GPU worker” reads a shared framebuffer in a `SharedArrayBuffer` and uploads it to a presenter backend:

- Shared framebuffer layout (Rust): `crates/aero-shared/src/shared_framebuffer.rs`
- Shared framebuffer layout (TS mirror): `web/src/ipc/shared-layout.ts`
- GPU worker consumption and present loop: `web/src/workers/gpu-worker.ts`

Presenter backend selection (GPU worker):

- Backends (current implementations):
  - WebGPU: `web/src/gpu/webgpu-presenter-backend.ts`
  - WebGL2 (raw): `web/src/gpu/raw-webgl2-presenter-backend.ts`
  - WebGL2 (wgpu via WASM): `web/src/gpu/wgpu-webgl2-presenter.ts`
- Selection inputs:
  - `GpuRuntimeInitOptions` in `web/src/ipc/gpu-protocol.ts` (`forceBackend`, `disableWebGpu`, `preferWebGpu`)
  - selection logic: `initPresenterForRuntime()` in `web/src/workers/gpu-worker.ts`

Compatibility note: the GPU worker can also consume an older “shared framebuffer protocol” header (RGBA8888 + frame counter, no dirty tiles) used by some harnesses/demos:

- `web/src/display/framebuffer_protocol.ts` (layout)
- `web/src/workers/gpu-worker.ts` (`refreshFramebufferProtocolViews`)

### 3) Scanout coordination: `ScanoutState` (seqlock)

There is a second shared-memory structure that is **not a framebuffer**; it is a **lock-free descriptor** of the current scanout source.
It exists so the GPU worker can be ticked/woken even when the legacy shared framebuffer is idle (e.g. after WDDM “takes over”).

- ScanoutState layout + publish protocol (Rust): `crates/aero-shared/src/scanout_state.rs`
- ScanoutState layout + publish protocol (TS mirror): `web/src/ipc/scanout_state.ts`
- Used by:
  - `web/src/main/frameScheduler.ts` (decides when to tick the GPU worker)
  - `web/src/workers/gpu-worker.ts` (chooses between legacy framebuffer and other output sources)

Related shared-memory descriptor: **hardware cursor state** uses the same seqlock pattern:

- CursorState layout + publish protocol (Rust): `crates/aero-shared/src/cursor_state.rs`
- CursorState layout + publish protocol (TS mirror): `web/src/ipc/cursor_state.ts`
- Consumed by the GPU worker: `web/src/workers/gpu-worker.ts` (cursor snapshot + presenter cursor APIs / compositing)

### 4) Host-side AeroGPU execution / translation building blocks (implemented)

The repo also contains substantial implemented host-side GPU infrastructure (even though full guest WDDM integration is still evolving):

- AeroGPU command processing + present + recovery/telemetry primitives: `crates/aero-gpu`
- AeroGPU device-model helpers (PCI/MMIO/ring executor/vblank pacing building blocks): `crates/aero-devices-gpu`
- D3D-related crates:
  - `crates/aero-d3d9`
  - `crates/aero-d3d11`
  - `crates/aero-dxbc`
- Canonical protocol mirror (Rust + TypeScript): `emulator/protocol` (source headers: `drivers/aerogpu/protocol/`)

## Runtime topology (browser)

At a high level, the runtime is:

```
┌────────────────────────────┐
│ Main thread                │
│ - owns the visible canvas  │
│ - schedules GPU worker tick│
└───────────────┬────────────┘
                │ postMessage("tick") + SharedArrayBuffer handles
                ▼
┌────────────────────────────┐
│ GPU worker                 │
│ - reads shared memory      │
│ - uploads to WebGPU/WebGL2 │
│ - presents to canvas       │
└───────────────┬────────────┘
                │ SharedArrayBuffer / WebAssembly.Memory
                ▼
┌────────────────────────────┐
│ CPU/VM side (WASM)         │
│ - produces scanout content │
│ - writes shared framebuffer│
│ - may publish ScanoutState │
└────────────────────────────┘
```

The details of worker orchestration are outside the scope of this doc, but the “presentation boundary” (what memory is shared and how) is defined by the shared-memory structures below.

Note: there is also a main-thread fallback presenter for the legacy shared framebuffer path (no GPU worker / no OffscreenCanvas transfer). This uses a 2D canvas and polls `SharedFramebufferHeaderIndex.FRAME_SEQ`:

- implementation: `web/src/display/shared_layout_presenter.ts` (`SharedLayoutPresenter`)
- wired in the web UI/runtime: `web/src/main.ts` (`ensureVgaPresenter`)

Shared-memory wiring note: in the canonical multi-worker runtime, the SharedArrayBuffers / `WebAssembly.Memory` handles are distributed to workers via a coordinator init message:

- Message type: `WorkerInitMessage` (`kind: "init"`) in `web/src/runtime/protocol.ts` (includes `guestMemory`, optional `vram`, `scanoutState`, `cursorState`, `sharedFramebuffer`, and optional `frameStateSab`).
- Segment construction: `web/src/runtime/shared_layout.ts` (allocates the shared framebuffer, scanout/cursor descriptors, and optional VRAM aperture).

## Frame pacing / “new frame” state (SharedArrayBuffer)

In addition to the pixel/scanout structures, the browser runtime uses a small `SharedArrayBuffer` as a cross-thread “frame status” flag + metrics block.

- Definition (indices + values): `web/src/ipc/gpu-protocol.ts` (`FRAME_STATUS_INDEX`, `FRAME_DIRTY`, `FRAME_PRESENTING`, `FRAME_PRESENTED`, plus metrics fields)
- Main-thread scheduler that posts `tick` messages to the GPU worker based on this state (and optionally `ScanoutState`): `web/src/main/frameScheduler.ts`
- GPU worker updates this state as it receives/presents frames: `web/src/workers/gpu-worker.ts`

## Shared-memory display path #1: `SharedFramebuffer` (double-buffered + dirty tiles)

**Goal:** move pixels from the VM/CPU side to the GPU worker with minimal copying and an efficient “only upload what changed” option.

### Layout and publish protocol

Defined in:

- Rust: `crates/aero-shared/src/shared_framebuffer.rs`
- TypeScript mirror: `web/src/ipc/shared-layout.ts`

Key properties:

- **Header is an array of 32-bit atomics** so it can be accessed from both Rust and JS via `AtomicU32` / `Int32Array + Atomics`.
- **Double buffering** (`slot 0` and `slot 1`):
  - producer writes into the “back” slot, then publishes it by flipping `active_index` and incrementing `frame_seq`.
- Optional **dirty-tile tracking**:
  - each slot may have a dirty bitset (`dirty_words_per_buffer`)
  - dirty tiles are converted to pixel rects by:
    - Rust: `dirty_tiles_to_rects()` in `crates/aero-shared/src/shared_framebuffer.rs`
    - TS: `dirtyTilesToRects()` in `web/src/ipc/shared-layout.ts`

The canonical publish ordering (important for Atomics-based consumers) is documented and implemented in:

- Rust: `SharedFramebufferWriter::write_frame()` in `crates/aero-shared/src/shared_framebuffer.rs`

Rust-side consumer (host/presenter utilities):

- `crates/aero-gpu/src/frame_source.rs` (`FrameSource`) polls `frame_seq`, selects the active slot, and converts dirty tiles into rects for the presenter.

### Consumption in the GPU worker

The GPU worker:

1. Validates the header (`magic`, `version`).
2. Reads `active_index` to select the active slot.
3. Optionally derives dirty rects from the per-slot dirty bitset.
   - If dirty tracking is enabled but the producer sets **no** dirty bits, the consumer treats the frame as **full-frame dirty** (mirrors `FrameSource` behavior to avoid interpreting `[]` as “nothing changed”).
4. Uploads either:
   - a full frame (`present()`), or
   - rect updates (`presentDirtyRects()` when the selected backend supports it).

Code pointers:

- View creation: `refreshSharedFramebufferViews()` in `web/src/workers/gpu-worker.ts`
- Frame selection + dirty-rect derivation: `getCurrentFrameInfo()` in `web/src/workers/gpu-worker.ts`

There is an end-to-end Playwright test that exercises this path:

- `tests/e2e/web/aero-gpu-shared-framebuffer.spec.ts`

## Shared-memory display path #2: `ScanoutState` (seqlock scanout descriptor)

**Goal:** share a coherent “what should be displayed” descriptor across workers without locks, and without forcing the legacy shared framebuffer to be “busy” forever.

This is a small `u32[]` / `Int32Array` structure containing:

- generation (seqlock-style)
- source (`LEGACY_TEXT`, `LEGACY_VBE_LFB`, `WDDM`)
- base physical address (lo/hi)
- width/height/pitch/format
  - `format` uses the AeroGPU `AerogpuFormat` numeric (`u32`) discriminants (where `0` is reserved for `Invalid`).

Defined in:

- Rust: `crates/aero-shared/src/scanout_state.rs`
- TypeScript mirror: `web/src/ipc/scanout_state.ts`

### Seqlock publish protocol

The key implementation detail is the “busy bit” seqlock:

- Writer sets `SCANOUT_STATE_GENERATION_BUSY_BIT` before updating fields.
- Writer publishes a new generation (with the busy bit cleared) **as the final store**.
- Reader retries if:
  - the busy bit is set, or
  - generation changes during the read.

Code pointers:

- Rust: module-level docs + `ScanoutState::publish()` / `ScanoutState::snapshot()` in `crates/aero-shared/src/scanout_state.rs`
- TS: `publishScanoutState()` / `snapshotScanoutState()` in `web/src/ipc/scanout_state.ts`

### How it is used today

- **Main thread scheduling:** `web/src/main/frameScheduler.ts` uses `ScanoutState` to decide whether to keep ticking the GPU worker even when the shared framebuffer is in the `PRESENTED` state.
- **GPU worker output selection:** `web/src/workers/gpu-worker.ts` snapshots `ScanoutState` during `presentOnce()` and uses it to avoid “flashing back” to the legacy framebuffer after WDDM scanout is considered active.
- **GPU worker WDDM scanout readback (when `base_paddr != 0`):** `web/src/workers/gpu-worker.ts` treats `base_paddr` as a guest physical address and can present WDDM scanout by reading from either the shared VRAM aperture (BAR1 backing) or guest RAM, normalizing to a tightly-packed RGBA8 buffer (`tryReadWddmScanoutFrame()` / `tryReadWddmScanoutRgba8()`). The RAM-vs-VRAM resolution and the VRAM base-paddr contract are documented in [`docs/16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md#vram-bar1-backing-as-a-sharedarraybuffer).
  - Unit tests: `web/src/workers/gpu-worker_wddm_vram_scanout.test.ts`, `web/src/workers/gpu-worker_scanout_vram_missing.test.ts`.
- **Canonical Rust machine (optional):** `crates/aero-machine/src/lib.rs` can publish scanout-source updates into an `aero_shared::scanout_state::ScanoutState` provided by the host:
  - `Machine::set_scanout_state()` installs the shared descriptor.
  - `Machine::reset()` publishes `LEGACY_TEXT` on reset.
  - `Machine::handle_bios_interrupt()` publishes legacy scanout transitions (`LEGACY_TEXT` ↔ `LEGACY_VBE_LFB`) on BIOS INT 10h mode changes, while preserving sticky handoff semantics once WDDM has claimed scanout.
  - `Machine::process_aerogpu()` publishes updates derived from AeroGPU scanout0 registers (when enabled), and can also revert the shared scanout descriptor back to the legacy BIOS source if WDDM disables scanout.

Note: `ScanoutState` is also the intended mechanism for a device model to describe a guest-memory scanout buffer (base paddr + pitch etc). The full AeroGPU/WDDM scanout plumbing is owned elsewhere; see “AeroGPU status” below.

## Canonical machine boot display path (VGA/VBE)

The canonical Rust machine (`aero_machine::Machine`) wires in a legacy VGA/VBE device for BIOS + early boot output:

- VGA/VBE implementation: `crates/aero-gpu-vga`
- Integration into the canonical machine: `crates/aero-machine/src/lib.rs`
  - `MachineConfig::enable_vga`
  - VGA+VBE wiring (search for `VGA / SVGA integration`)
  - `Machine::display_present()`

Important ABI notes:

- `MachineConfig::enable_vga` and `MachineConfig::enable_aerogpu` are mutually exclusive.
- The canonical **AeroGPU PCI identity** is reserved at `00:07.0` (`VID:DID = A3A0:0001`) and documented in:
  - `docs/abi/aerogpu-pci-identity.md`
- When `enable_vga=true` (and PC platform is enabled), the machine exposes a transitional Bochs/QEMU-compatible “Standard VGA”-like PCI function for VBE LFB routing (see `VGA_PCI_BDF` in `crates/aero-machine/src/lib.rs`).

## Presenter backends and color/alpha policy

Presentation policy is also centralized on the Rust side (useful for native tests and wgpu-backed paths):

- `crates/aero-gpu/src/present.rs` (presentation policy enums + selection helpers, plus dirty-rect upload utilities)

### Source pixel format and conventions

Across the browser presentation code, the “CPU → presenter” source is treated as:

- **RGBA8** byte order `[R, G, B, A]`
- **top-left origin** (first row is the top scanline)

Code pointers:

- WebGPU worker presenter uploads: `web/src/gpu/webgpu-presenter-backend.ts` (`frameTexture` is `rgba8unorm`)
  - Top-left origin convention is enforced in the blit shader: `web/src/gpu/shaders/blit.wgsl`
- WebGL2 worker presenter uploads: `web/src/gpu/raw-webgl2-presenter-backend.ts` (`tex(Sub)Image2D` with `gl.UNPACK_FLIP_Y_WEBGL = 0`)
  - Top-left origin convention is enforced in the shaders: `web/src/gpu/shaders/blit.vert.glsl`, `web/src/gpu/shaders/blit.frag.glsl`

### Alpha policy: treat output as opaque

The browser canvas is configured to avoid blending with the page background.

Enforced in:

- WebGPU presenter backend: `web/src/gpu/webgpu-presenter-backend.ts` (`ctx.configure({ alphaMode: "opaque", ... })`)
- Raw WebGL2 presenter backend: `web/src/gpu/raw-webgl2-presenter-backend.ts` (context created with `{ alpha: false, premultipliedAlpha: false }`)

### Linear vs sRGB policy and validation

For deterministic comparisons between WebGPU and WebGL2, Aero also ships “validation presenters” that explicitly implement:

- output color space selection (`linear` vs `srgb`)
- alpha mode (`opaque` vs `premultiplied`)
- optional flip-Y

Code pointers:

- WebGPU validation presenter: `web/src/gpu/webgpu-presenter.ts`
  - uses `viewFormats` (e.g. `bgra8unorm-srgb`) when available; otherwise falls back to shader sRGB encoding
- WebGL2 validation presenter: `web/src/gpu/raw-webgl2-presenter.ts`
  - does sRGB encoding in shader for deterministic output (WebGL default framebuffer sRGB behavior varies)
- Playwright validation: `tests/e2e/web/gpu_color.spec.ts`

Worker presented-output validation (post sRGB/alpha policy; canvas pixels):

- Debug readback message: `screenshot_presented` in `web/src/ipc/gpu-protocol.ts`
- Test card generator: `web/src/gpu/test-card.ts`
- Playwright E2E:
  - Color policy (gamma/alpha/origin): `tests/e2e/web/gpu_worker_presented_color_policy.spec.ts`
  - Cursor blending (linear blend + sRGB encode): `tests/e2e/gpu_worker_presented_cursor_overlay.spec.ts`

## AeroGPU status (high level; protocol references)

This doc does **not** define the AeroGPU PCI/MMIO integration work (owned elsewhere). It only links the current contracts and the code that exists today.

Protocol references:

- Guest-facing protocol headers: `drivers/aerogpu/protocol/README.md`
  - includes `aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`
- In-tree Rust/TS mirror: `emulator/protocol`
- ABI docs:
  - `docs/abi/aerogpu-pci-identity.md` (PCI identity)
  - `docs/abi/gpu-command-protocol.md` and `docs/16-gpu-command-abi.md` (command stream format / ABI)

Canonical machine note:

- `MachineConfig::enable_aerogpu` wires BAR1 VRAM (plus legacy VGA window aliasing / VBE compatibility mapping) and an MVP BAR0 register block (ABI/features, ring/fence + IRQ transport, scanout0/cursor + vblank registers) that is sufficient for detection/bring-up and basic pacing.
  Full AeroGPU command execution is not implemented in `aero-machine` yet (see the `MachineConfig::enable_aerogpu` docs in `crates/aero-machine/src/lib.rs`).
- The MVP BAR0 MMIO surface + ring/fence/vblank/scanout implementation in the canonical machine lives in: `crates/aero-machine/src/aerogpu.rs`.
- A more complete AeroGPU PCI device model exists in the emulator crate (`crates/emulator/src/devices/pci/aerogpu.rs`) and is not the canonical browser machine today.

## How to validate (tests)

TypeScript/unit tests (fast, good for IPC/layout changes):

```bash
npm run test:unit:coverage
```

Playwright GPU-focused e2e tests:

```bash
# WebGPU project (includes gpu_color.spec.ts when WebGPU is available)
npm run test:webgpu

# Targeted test for the SharedFramebuffer + dirty-tiles presentation path
npm run test:e2e -- tests/e2e/web/aero-gpu-shared-framebuffer.spec.ts

# Targeted test for presenter backend fallback (WebGPU disabled → WebGL2)
npm run test:e2e -- tests/e2e/web/gpu-fallback.spec.ts
```

Rust tests relevant to shared-memory graphics/presentation:

```bash
cargo test -p aero-shared
cargo test -p aero-gpu
cargo test -p aero-machine
```
