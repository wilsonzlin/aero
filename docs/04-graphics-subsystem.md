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

- Boot graphics path (`enable_vga=true`, `enable_aerogpu=false`): `aero_gpu_vga::VgaDevice`. When the PC platform is enabled (`enable_pc_platform=true`), the machine maps the VBE linear framebuffer (LFB) MMIO aperture directly at the configured LFB base inside the PCI MMIO window (no dedicated PCI VGA stub).
- AeroGPU device (MVP; `enable_aerogpu=true`, `enable_vga=false`): requires `enable_pc_platform=true`.
  - Exposes the canonical AeroGPU PCI identity at **`00:07.0`** (`VID:DID = A3A0:0001`).
  - Wires BAR1-backed VRAM (legacy VGA window aliasing / VBE compatibility mapping).
  - Exposes a minimal BAR0 MMIO surface used for bring-up (ABI/features, ring+fence transport +
    submission decode/capture + IRQs, scanout0/cursor registers, vblank counters; default behavior
    can complete fences without executing ACMD, and browser runtimes can enable the AeroGPU
    submission bridge to drain submissions for out-of-process execution; native builds can install
    a feature-gated in-process wgpu backend; implementation: `crates/aero-machine/src/aerogpu.rs`).
- On the Rust side, the host can call `Machine::display_present()` to update a host-visible RGBA framebuffer cache (`Machine::display_framebuffer()` / `Machine::display_resolution()`).
  - In AeroGPU mode (no standalone VGA device model), `display_present()` prefers the WDDM scanout0 framebuffer once it has been claimed by a valid scanout config; otherwise it falls back to BIOS VBE LFB or BIOS text mode (see `Machine::display_present` in `crates/aero-machine/src/lib.rs`).
      - Once WDDM scanout is claimed, WDDM ownership remains sticky until VM reset. Writing `SCANOUT0_ENABLE=0` blanks presentation but does not release WDDM ownership back to legacy output.
      - When scanout is claimed but cannot be presented (e.g. PCI `COMMAND.BME=0`), `display_present()` clears the cached framebuffer instead of falling back to legacy output.
  - `aero-machine` does not execute the AeroGPU command stream in-process by default; browser
    runtimes can enable the submission bridge (`Machine::aerogpu_drain_submissions` /
    `Machine::aerogpu_complete_fence`) and execute drained submissions in the GPU worker. Native
    builds can also install an in-process headless wgpu backend (feature-gated;
    `Machine::aerogpu_set_backend_wgpu`).
  - Shared device-side AeroGPU building blocks (regs/ring/executor + reusable PCI wrapper) live in
    `crates/aero-devices-gpu/`. A legacy sandbox integration surface remains in
    `crates/emulator/src/devices/pci/aerogpu.rs` (see
    [`docs/21-emulator-crate-migration.md`](./21-emulator-crate-migration.md)).

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
  - The GPU worker typically uses `trySnapshotScanoutState()` (bounded retries; returns `null` on failure) rather than `snapshotScanoutState()` (which throws on timeout) so present loops can recover.
- Used by:
  - `web/src/main/frameScheduler.ts` (decides when to tick the GPU worker)
  - `web/src/workers/gpu-worker.ts` (chooses between legacy framebuffer and other output sources)

Developer-facing scanout validation harnesses (served under `/web/` when running the repo-root harness via
`npm run dev` / `npm run dev:harness`):

- `web/wddm-scanout-debug.html` — interactive scanoutState validation (guest RAM vs BAR1/VRAM backing, pitch, alpha policy)
- `web/wddm-scanout-smoke.html` — non-interactive smoke harness (Playwright: `tests/e2e/wddm_scanout_smoke.spec.ts`)
- `web/wddm-scanout-vram-smoke.html` — BAR1/VRAM-backed scanout smoke harness (Playwright: `tests/e2e/wddm_scanout_vram_smoke.spec.ts`)

Related shared-memory descriptor: **hardware cursor state** uses the same seqlock pattern:

- CursorState layout + publish protocol (Rust): `crates/aero-shared/src/cursor_state.rs`
- CursorState layout + publish protocol (TS mirror): `web/src/ipc/cursor_state.ts`
  - The GPU worker typically uses `trySnapshotCursorState()` (bounded retries; returns `null` on failure) rather than `snapshotCursorState()` (which throws on timeout).
- Consumed by the GPU worker: `web/src/workers/gpu-worker.ts` (cursor snapshot + presenter cursor APIs / compositing)
  - `CursorState.format` also uses AeroGPU `AerogpuFormat` discriminants and follows the same X8 alpha + sRGB interpretation rules as scanout formats.

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

Note: this “frame status” SAB is **separate** from the legacy shared framebuffer header’s `frame_dirty` flag (`SharedFramebufferHeaderIndex.FRAME_DIRTY`). The names are similar but they serve different purposes:

- `FRAME_STATUS_INDEX` / `FRAME_DIRTY` / `FRAME_PRESENTED` (in `web/src/ipc/gpu-protocol.ts`): main-thread↔GPU-worker **tick/pacing coordination**.
- `SharedFramebufferHeaderIndex.FRAME_DIRTY` (in `web/src/ipc/shared-layout.ts` / `crates/aero-shared/src/shared_framebuffer.rs`): producer→consumer **“new frame” / liveness** flag for the shared framebuffer itself.

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
- Producer also sets a `frame_dirty` flag (`SharedFramebufferHeaderIndex.FRAME_DIRTY`) on publish. This is a producer→consumer “new frame” / liveness flag. Implementations that want to block for new frames typically `Atomics.wait` on `frame_seq` (the canonical “new frame” address), and may also treat `frame_dirty` as a best-effort **consumer acknowledgement** (ACK): consumers clear it after they finish copying/presenting the active buffer, and producers may choose to throttle publishing until it is cleared to avoid overwriting a buffer that is still being read.
  (Not to be confused with the frame pacing state value `FRAME_DIRTY` in `web/src/ipc/gpu-protocol.ts`.)
  - Published by: `SharedFramebufferWriter::write_frame()` in `crates/aero-shared/src/shared_framebuffer.rs`
- Cleared by consumers after consuming a frame (examples):
  - Rust: `FrameSource::ack_frame(frame.seq)` in `crates/aero-gpu/src/frame_source.rs`
  - Browser GPU worker: `presentOnce()` in `web/src/workers/gpu-worker.ts` (clears after consuming a legacy frame, and also when scanout owns output to avoid stale legacy-dirty state).
  - Main-thread fallback presenter: `SharedLayoutPresenter` in `web/src/display/shared_layout_presenter.ts` (clears after `putImageData`).
- Optional **dirty-tile tracking**:
  - each slot may have a dirty bitset (`dirty_words_per_buffer`)
  - dirty tiles are converted to pixel rects by:
    - Rust: `dirty_tiles_to_rects()` in `crates/aero-shared/src/shared_framebuffer.rs`
    - TS: `dirtyTilesToRects()` in `web/src/ipc/shared-layout.ts`

The canonical publish ordering (important for Atomics-based consumers) is documented and implemented in:

- Rust: `SharedFramebufferWriter::write_frame()` in `crates/aero-shared/src/shared_framebuffer.rs`

Rust-side consumer (host/presenter utilities):

- `crates/aero-gpu/src/frame_source.rs` (`FrameSource`) polls `frame_seq`, selects the active slot, and converts dirty tiles into rects for the presenter. Consumers can call `FrameSource::ack_frame` after they are finished reading/copying a frame to clear the shared `frame_dirty` flag (ACK).

### Consumption in the GPU worker

The GPU worker:

1. Validates the header (`magic`, `version`).
2. Reads `active_index` to select the active slot.
3. Optionally derives dirty rects from the per-slot dirty bitset.
   - If dirty tracking is enabled but the producer sets **no** dirty bits, the consumer treats the frame as **full-frame dirty** (mirrors `FrameSource` behavior to avoid interpreting `[]` as “nothing changed”).
4. Uploads either:
   - a full frame (`present()`), or
   - rect updates (`presentDirtyRects()` when the selected backend supports it).
   - The worker may still choose to fall back to a full-frame upload even when dirty rects are available (e.g. too many rects or estimated upload bytes too high). Policy helper:
     - `chooseDirtyRectsForUpload()` in `web/src/gpu/dirty-rect-policy.ts`
5. Clears the producer→consumer `frame_dirty` flag (`SharedFramebufferHeaderIndex.FRAME_DIRTY`) after consuming a frame.

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
  - Format semantics (from the AeroGPU protocol):
    - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha; treat alpha as fully opaque (`0xFF`) when converting to RGBA.
    - `*_SRGB` variants are layout-identical to UNORM; only the interpretation differs (avoid double-applying gamma in presenters).

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
- TS: `publishScanoutState()` / `snapshotScanoutState()` / `trySnapshotScanoutState()` in `web/src/ipc/scanout_state.ts`

### How it is used today

- **Main thread scheduling:** `web/src/main/frameScheduler.ts` uses `ScanoutState` to decide whether to keep ticking the GPU worker even when the shared framebuffer is in the `PRESENTED` state.
  - When `ScanoutState.source` is `WDDM` or `LEGACY_VBE_LFB`, it keeps ticking so the worker can poll/present scanout output and drain vsync-paced completions even if the legacy shared framebuffer is idle.
  - When WDDM publishes the **disabled** descriptor (`base/width/height/pitch = 0`), the scheduler stops continuous ticking (vblank pacing is effectively off) but will still wake on scanout generation changes.
- **GPU worker output selection:** `web/src/workers/gpu-worker.ts` snapshots `ScanoutState` during `presentOnce()` and uses it to avoid “flashing back” to the legacy framebuffer after WDDM scanout is considered active.
- **GPU worker scanout readback (guest-memory scanout):** when `ScanoutState.source` is `WDDM` or `LEGACY_VBE_LFB` and `base_paddr` points at a real guest framebuffer, `web/src/workers/gpu-worker.ts` reads pixels from either the shared VRAM aperture (BAR1 backing) or guest RAM and normalizes to a tightly-packed RGBA8 buffer (`tryReadScanoutFrame()` / `tryReadScanoutRgba8()`).
  - Supported formats today (AeroGPU `AerogpuFormat` discriminants):
    - 32bpp packed: `B8G8R8X8` / `B8G8R8A8` / `R8G8B8X8` / `R8G8B8A8` (plus `_SRGB` variants).
    - 16bpp packed: `B5G6R5` (opaque) and `B5G5R5A1` (1-bit alpha).
    X8 formats force `A=255`; A8 formats preserve alpha. `_SRGB` variants are layout-identical; the GPU worker decodes sRGB→linear after swizzle so the intermediate RGBA8 buffer is in linear space for blending/presentation.
  - Shared helper used by readback paths (size checks + guest-RAM conversion): `web/src/runtime/scanout_readback.ts` (`tryComputeScanoutRgba8ByteLength`, `MAX_SCANOUT_RGBA8_BYTES`, `readScanoutRgba8FromGuestRam`).
  - Note: for `source=WDDM`, `base_paddr == 0` is used in two distinct ways:
    - **Placeholder descriptor** for the host-side AeroGPU path: `base_paddr=0` but **non-zero** `width/height/pitch`.
    - **Disabled descriptor** (WDDM retains ownership but blanks output): `base/width/height/pitch = 0`.
    Legacy VBE scanout expects a real framebuffer (`base_paddr != 0`).
  - The RAM-vs-VRAM resolution and the VRAM base-paddr contract are documented in [`docs/16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md#vram-bar1-backing-as-a-sharedarraybuffer).
  - Unit tests: `web/src/workers/gpu-worker_wddm_scanout_readback.test.ts`, `web/src/workers/gpu-worker_wddm_scanout_screenshot_refresh.test.ts`, `web/src/workers/gpu-worker_scanout_vram_missing.test.ts`, `web/src/workers/gpu-worker_wddm_tick_gate.test.ts`.
- **Canonical Rust machine (optional):** `crates/aero-machine/src/lib.rs` can publish scanout-source updates into an `aero_shared::scanout_state::ScanoutState` provided by the host:
  - `Machine::set_scanout_state()` installs the shared descriptor.
  - `Machine::reset()` publishes `LEGACY_TEXT` on reset.
  - `Machine::handle_bios_interrupt()` publishes legacy scanout transitions (`LEGACY_TEXT` ↔ `LEGACY_VBE_LFB`) on BIOS INT 10h mode changes, while refusing to let legacy INT 10h steal scanout while WDDM is active (until the VM resets).
  - `Machine::process_aerogpu()` publishes updates derived from AeroGPU scanout0 registers, including publishing a disabled WDDM scanout descriptor when the guest clears `SCANOUT0_ENABLE` (visibility toggle) so legacy scanout does not steal ownership back.

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
- When `enable_vga=true` (and PC platform is enabled), the VBE LFB lives inside the PCI MMIO window and is routed directly by the platform MMIO mapping (not via a dedicated PCI VGA function).

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
  - Cursor overlay blending (linear blend + sRGB encode): `tests/e2e/gpu_worker_presented_cursor_overlay.spec.ts`
  - CursorState upload + screenshot include/exclude: `tests/e2e/web/gpu_hardware_cursor_state.spec.ts`

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
  It decodes ring submissions and exposes them via a submission bridge for the browser runtime (GPU worker), and also supports optional in-process backends for native/tests. End-to-end Win7 validation is still pending (see `docs/graphics/status.md`).
- The MVP BAR0 MMIO surface + ring/fence/vblank/scanout implementation in the canonical machine lives in: `crates/aero-machine/src/aerogpu.rs`.
- A shared AeroGPU device-side library exists in `crates/aero-devices-gpu/` (regs/ring/executor + optional backend boundary) and a legacy sandbox integration surface exists in `crates/emulator/src/devices/pci/aerogpu.rs`. The canonical browser machine (`crates/aero-machine` + `crates/aero-wasm` + web workers) currently has its own BAR0/BAR1 integration layer; consolidating these surfaces remains outstanding.

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

# Targeted test for ScanoutState-driven scanout presentation (guest-memory scanout readback)
npm run test:e2e -- tests/e2e/web/runtime_workers_scanout_state.spec.ts

# Targeted scanout harness smoke tests (served by `/web/wddm-scanout-*.html`)
npm run test:e2e -- tests/e2e/wddm_scanout_smoke.spec.ts
npm run test:e2e -- tests/e2e/wddm_scanout_vram_smoke.spec.ts

# Targeted test for presenter backend fallback (WebGPU disabled → WebGL2)
npm run test:e2e -- tests/e2e/web/gpu-fallback.spec.ts
```

Rust tests relevant to shared-memory graphics/presentation:

```bash
cargo test -p aero-shared
cargo test -p aero-gpu
cargo test -p aero-machine
```
