# 16 - Browser GPU Backends (WebGPU-first + WebGL2 Fallback)

## Overview

This document specifies **how Aero uses GPU acceleration in the browser** with a **WebGPU-first** design and a **WebGL2 fallback**. It turns the high-level “use WebGPU” idea into an implementable plan covering:

- Runtime backend selection (auto + user override)
- OffscreenCanvas + worker lifecycle
- Capability negotiation (required vs optional features/limits)
- Presentation strategy (swapchain + blit)
- Error handling & recovery (device lost / context lost)
- Known limitations of the WebGL2 fallback
- Testing and benchmark hooks

This spec is written for an implementation that is:

- **Core in Rust → WASM**
- GPU abstraction via **`wgpu`** (so we have one renderer API)
- Shader compilation/translation via **`naga`** (wgpu’s shader IR)
- **WebGPU** used when available, otherwise **WebGL2** via wgpu’s WebGL backend.

If the project later decides to use the raw JS WebGPU API directly, this document’s concepts still apply, but the concrete API calls and shader translation pipeline will change.

---

## Why WebGPU-first + WebGL2 fallback exists

### WebGPU-first

WebGPU is the only practical web platform API that can support the long-term graphics roadmap:

- Explicit GPU resource management (buffers/textures/samplers)
- Modern rendering pipeline model that maps well to Direct3D/Vulkan concepts
- **Compute shaders** for acceleration (texture decompression, post-processing, compositing)
- Better performance predictability than WebGL (less driver magic)

### WebGL2 fallback

WebGL2 exists because:

- WebGPU is not universally available in all browsers/environments.
- Some users run in restricted environments (enterprise policies, older GPUs, sandboxed contexts).
- A fallback path is valuable for:
  - **Bring-up** (VGA/SVGA framebuffer presentation)
  - **Debugging** (simpler shader model, easier capture tools)
  - **Broader compatibility** with explicit known limitations

The fallback is not expected to support the full DirectX translation surface area. It is a “run something and present frames” path.

---

## Implementation approach: `wgpu` + `naga` in a worker

### Summary

- The **GPU backend runs in a dedicated “GPU worker”**.
- The main thread owns the DOM canvas and forwards it to the worker as an **`OffscreenCanvas`**.
- The worker creates a `wgpu::Instance` and chooses one of:
  - **WebGPU backend** (`Backends::BROWSER_WEBGPU`)
  - **WebGL2 backend** (`Backends::GL`)
- Rendering/presentation uses a shared design:
  - Render into an internal texture (`present_src`)
  - Blit to the canvas surface each frame (`present_dst`)

### Why a worker?

Keeping GPU work off the main thread prevents long GPU submission work (pipeline creation, shader compilation, large uploads) from blocking:

- input events
- UI updates
- async resource loading

It also matches Aero’s overall architecture (CPU worker, I/O worker, etc.).

---

## Runtime backend selection

### User override knobs

Backend selection must be deterministic and overridable for debugging, bug reports, and CI.

Proposed configuration surface (any subset is fine, but at least one override is required):

1. **URL query param**: `?gpu=auto|webgpu|webgl2`
2. **JS config**: `new Aero({ gpuBackend: "auto" | "webgpu" | "webgl2" })`
3. **Debug UI toggle** (persisted in `localStorage`)

Related knobs (optional but recommended):

- `?power=high|low` → maps to WebGPU `powerPreference` / wgpu power preference
- `?present=vsync|immediate` → maps to `PresentMode` where supported (WebGPU), ignored on WebGL2
- `?gpuValidation=1` → enable extra validation/logging (see “Testing & benchmarks”)

### Selection algorithm (normative)

Inputs:

- `requested_backend`: `"auto" | "webgpu" | "webgl2"`
- `allow_fallback`: boolean (default `true` in `"auto"`, `false` if explicitly forced)
- `required_caps`: derived from current configuration (see next section)

Algorithm:

1. If `requested_backend == "webgpu"`:
   - Try WebGPU path
   - If unavailable or does not meet **required capabilities**, fail with an actionable error
2. Else if `requested_backend == "webgl2"`:
   - Try WebGL2 path
   - If unavailable or does not meet **required capabilities**, fail with an actionable error
3. Else (`"auto"`):
   - Try WebGPU path; if it meets required capabilities, select it
   - Else try WebGL2 path; if it meets required capabilities, select it
   - Else fail with an actionable error

### Concrete `wgpu`-based pseudocode

```rust
enum RequestedBackend { Auto, WebGpu, WebGl2 }
enum SelectedBackend { WebGpu, WebGl2 }

async fn select_backend(
    requested: RequestedBackend,
    canvas: web_sys::OffscreenCanvas,
    required: RequiredCaps,
) -> Result<(SelectedBackend, GpuContext), GpuInitError> {
    match requested {
        RequestedBackend::WebGpu => try_init_webgpu(canvas, required).await,
        RequestedBackend::WebGl2 => try_init_webgl2(canvas, required).await,
        RequestedBackend::Auto => {
            if let Ok(ctx) = try_init_webgpu(canvas.clone(), required).await {
                return Ok((SelectedBackend::WebGpu, ctx));
            }
            try_init_webgl2(canvas, required).await
        }
    }
}

async fn try_init_webgpu(
    canvas: web_sys::OffscreenCanvas,
    required: RequiredCaps,
) -> Result<(SelectedBackend, GpuContext), GpuInitError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });
    init_with_instance(instance, canvas, required)
        .await
        .map(|ctx| (SelectedBackend::WebGpu, ctx))
}

async fn try_init_webgl2(
    canvas: web_sys::OffscreenCanvas,
    required: RequiredCaps,
) -> Result<(SelectedBackend, GpuContext), GpuInitError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        ..Default::default()
    });
    init_with_instance(instance, canvas, required)
        .await
        .map(|ctx| (SelectedBackend::WebGl2, ctx))
}
```

Note: this “two separate instances” approach ensures we control priority and error reporting. If we instead create one instance with both backends enabled, wgpu will pick a backend but the selection is less explicit.

---

## OffscreenCanvas + worker lifecycle

### Main thread responsibilities

The main thread:

- Creates the visible `HTMLCanvasElement` and manages CSS sizing/layout
- Transfers control to the GPU worker as an `OffscreenCanvas`
- Forwards:
  - resize events (including device pixel ratio changes)
  - user override settings (backend choice, present mode, validation)
  - shutdown/restart commands
- Displays user-facing errors and recovery prompts

Example (conceptual):

```ts
const canvas = document.querySelector("canvas")!;
const offscreen = canvas.transferControlToOffscreen();

gpuWorker.postMessage(
  {
    type: "gpu:init",
    canvas: offscreen,
    backend: "auto", // or user override
    size: { width: canvas.width, height: canvas.height },
    dpr: window.devicePixelRatio,
  },
  [offscreen],
);
```

### GPU worker responsibilities

The GPU worker is the sole owner of:

- WebGPU/WebGL2 context creation and lifetime
- Device/queue and surface configuration
- Pipeline/shader compilation and caching
- GPU resource management (buffers/textures)
- Present loop scheduling (or responding to “present” messages)
- Device/context loss handling and reporting

The worker must expose a **minimal protocol** back to the main thread:

- `gpu:ready` with selected backend + capabilities summary
- `gpu:error` with a stable error code + human-readable message
- `gpu:stats` for profiling/bench runs (optional)

### Worker state machine (recommended)

```
┌───────────┐
│  Created  │
└─────┬─────┘
      │ gpu:init(canvas, config)
      ▼
┌───────────┐     device/context lost     ┌─────────────┐
│  Running  │────────────────────────────▶│ Recovering  │
└─────┬─────┘                              └─────┬──────┘
      │ gpu:shutdown                             │ success
      ▼                                          ▼
┌───────────┐                              ┌───────────┐
│ Stopped   │◀─────────────────────────────│  Running  │
└───────────┘           failure            └───────────┘
```

For recovery, the worker may either:

1. Recreate device/surface in place (preferred), or
2. Ask the main thread to terminate and recreate the worker (simpler “reset everything” path).

---

## Capability negotiation

### Design goals

Capability negotiation must:

- Separate **hard requirements** (must-have to run) from **optional enhancements**
- Produce a stable `GpuCaps` struct used throughout the renderer
- Avoid over-requesting WebGPU features/limits (which causes initialization failures)
- Prefer “feature probes + branching” over “require everything”

### Required vs optional: definition

- **Required**: Without this, Aero cannot present frames correctly at all.
- **Optional**: Enables performance/quality improvements, but has a correct fallback.

### Required capabilities (baseline)

The baseline required capabilities are intentionally small; they enable:

- An RGBA framebuffer texture at the guest resolution
- A blit pipeline to present to the canvas

Hard requirements (both backends):

- Ability to allocate a 2D RGBA8 texture of at least the configured guest display size
- Ability to draw a full-screen triangle/quad
- Support for dynamic texture updates (upload guest framebuffer)

If a device cannot meet the guest’s requested resolution, the **display subsystem must negotiate down** (e.g., clamp guest resolution or downscale).

### Optional capabilities (examples)

These are discovered and enabled opportunistically:

- Texture compression (BCn / S3TC) → reduces VRAM + upload bandwidth
- Float render targets / filtering → HDR paths, higher precision compositing
- Timestamp queries → accurate GPU timing for profiling
- MSAA → quality improvement (if used by translated pipelines)

### Capability matrix (summary)

| Capability | WebGPU backend | WebGL2 backend | Required? | Notes |
|---|---:|---:|---:|---|
| Present to canvas (surface) | Yes | Yes | Yes | WebGPU uses `Surface`; WebGL2 uses default framebuffer |
| RGBA8 textures | Yes | Yes | Yes | Use `rgba8unorm`/`RGBA8` |
| Depth/stencil rendering | Yes | Limited | Optional | WebGL2: depends on `DEPTH24_STENCIL8` support |
| Compute shaders | Yes | No | Optional | Must have CPU fallback paths |
| Storage buffers / SSBO-like | Yes | No (wgpu-webgl) | Optional | WebGL2 has no general storage buffers model |
| Texture compression (BCn/S3TC) | Optional | Optional | Optional | WebGL2 requires extensions (`WEBGL_compressed_texture_s3tc`) |
| Float32 filterable textures | Optional | Rare | Optional | Avoid requiring this; fallback to unfilterable/normalized formats |
| Timestamp queries | Optional | No | Optional | Use CPU timers when unavailable |

### WebGPU feature/limit negotiation (concrete)

Rules:

1. **Do not** set `required_features` unless a correct fallback exists that does not need them.
2. Prefer: `enabled_features = supported ∩ desired_optional_features`.
3. Validate limits after adapter selection; clamp runtime configuration if needed.

Example desired optional feature set:

- `TEXTURE_COMPRESSION_BC` (if we have BCn assets or want to avoid CPU decompression)
- `TIMESTAMP_QUERY` (profiling builds)

Note: for native/headless runs (CI, tests), texture compression feature requests can be force-disabled
globally via `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1` to validate CPU decompression fallbacks or
work around flaky driver/software-adapter compression paths.

In wgpu terms:

```rust
let desired = wgpu::Features::TEXTURE_COMPRESSION_BC
    | wgpu::Features::TIMESTAMP_QUERY;

let supported = adapter.features();
let enabled = desired & supported;

let required_limits = wgpu::Limits {
    // Keep this minimal; clamp runtime behavior instead of demanding huge limits.
    max_texture_dimension_2d: required.max_texture_dimension_2d,
    ..wgpu::Limits::downlevel_webgl2_defaults()
};

let (device, queue) = adapter
    .request_device(
        &wgpu::DeviceDescriptor {
            label: Some("aero-gpu"),
            required_features: enabled,
            required_limits,
        },
        None,
    )
    .await?;
```

The key point: **treat most features as optional** and implement CPU or simplified shader fallbacks.

---

## Presentation strategy: swapchain + blit

### Rationale

Directly rendering every translated pipeline into the canvas surface is undesirable:

- The surface format is chosen by the browser and may not match the internal “guest” format.
- Resizing the canvas would force invalidation and reconfiguration complexity across the entire render graph.
- We want a stable “final frame” texture (`present_src`) that can be used for:
  - deterministic screenshots / readback tests (read back `present_src` bytes, not the canvas)
  - the input to presentation-time post-processing (scaling, color conversion, cursor compositing) when blitting to the surface

Therefore:

1. Render everything into an internal texture (`present_src`).
2. Each frame, acquire the surface texture (`present_dst`) and blit.

### WebGPU presentation (worker)

At initialization:

- Create `wgpu::Surface` from the `OffscreenCanvas`.
- Choose a surface format (prefer SRGB if available).
- Configure the surface.
- Create a fixed “blit pipeline”:
  - vertex: fullscreen triangle
  - fragment: sample `present_src`
  - optional: linear → sRGB conversion, scaling, letterboxing

Per frame:

1. Acquire current surface texture (`surface.get_current_texture()`).
2. Encode a render pass that draws fullscreen into the surface view.
3. Submit and present.

Failure cases:

- `SurfaceError::Lost` / `Outdated` / `Timeout` must trigger reconfigure and/or recovery (see below).

### WebGL2 presentation (worker)

At initialization:

- Create a WebGL2 context from `OffscreenCanvas`.
- Create a texture for `present_src` (RGBA8).
- Compile GLSL ES 3.0 shader program for the fullscreen blit.

Per frame:

1. Upload new `present_src` pixels (`texSubImage2D`, ideally with row-aligned data).
2. Draw fullscreen triangle to the default framebuffer.
3. `gl.flush()` as needed (avoid `gl.finish()` except in tests).

Note: WebGL2 has no explicit swapchain API; the browser owns presentation timing.

---

## Error handling & recovery

### WebGPU: device lost

WebGPU devices can be lost due to:

- GPU resets
- driver updates
- browser power management
- out-of-memory conditions

Recovery contract:

1. GPU worker detects loss via `device.lost()` (wgpu) / `device.lost` (raw WebGPU).
2. Worker sends `gpu:error` with:
   - `code: "WEBGPU_DEVICE_LOST"`
   - `canRecover: true` (if fallback is allowed)
3. Worker attempts:
   - re-request adapter/device
   - recreate surface and all GPU resources
4. If recovery fails and backend is `"auto"`:
   - attempt WebGL2 fallback initialization
5. If still failing:
   - report fatal error to main thread with an actionable message

### WebGPU: surface errors

On surface acquisition/present errors:

- `Outdated` / `Lost` → reconfigure surface (usually after resize)
- `Timeout` → skip frame and retry (avoid busy-loop)
- `OutOfMemory` → treat as fatal (or trigger a full GPU reset)

### WebGL2: context lost

WebGL2 can lose context at any time. The worker should:

- Listen for `webglcontextlost` and `webglcontextrestored`
- On loss:
  - stop issuing GL commands
  - send `gpu:error` with `code: "WEBGL_CONTEXT_LOST"`
- On restore:
  - recreate all GL objects (programs, textures, buffers)
  - resume rendering

If context restoration is unreliable in a target browser, prefer the “restart worker” recovery path.

---

## Known limitations of the WebGL2 fallback

The WebGL2 fallback is intentionally limited. The renderer must assume:

- **No compute shaders** → all compute-based accelerations must have CPU fallbacks.
- **Restricted resource/binding model**:
  - no storage buffers (in wgpu-webgl)
  - smaller uniform limits
  - fewer bind groups / binding slots
- **Reduced texture/format support**:
  - many DXGI formats have no direct WebGL2 equivalent
  - compressed textures require extensions and vary by platform; Aero does not request BC/ETC2/ASTC compression features on wgpu's GL backend by default (CPU decompression fallbacks are used instead)
  - render-to-float requires extensions and is inconsistent
- **Shader translation constraints**:
  - WGSL features that can’t be lowered to GLSL ES 3.0 must be avoided in the shared shader library
  - keep the WebGL2 shader subset small and explicitly tested
- **Performance pitfalls**:
  - texture uploads can become the bottleneck (`texSubImage2D` cost)
  - state changes are more expensive (no pipeline objects)
  - readback (`readPixels`) is slow and should be test-only

This means that in WebGL2 mode, Aero may be restricted to:

- VGA/SVGA framebuffer presentation
- Minimal 2D compositing
- Debug views / diagnostics

Full DirectX 10/11-level translation is out of scope for the fallback.

---

## Testing & benchmark hooks

### Goals

We need automated ways to:

- verify that each backend can initialize and present frames
- catch regressions in shader compilation, resize handling, and device loss recovery
- measure performance differences between backends

### Smoke tests (recommended)

Add a small “GPU backend smoke test” harness that runs in both modes:

1. Initialize GPU backend (`auto`, `webgpu`, `webgl2`)
2. Render a deterministic pattern into `present_src`:
   - solid colors
   - a gradient
   - a small glyph atlas sample (optional)
3. Present to canvas
4. Read back pixels (test-only). Decide which stage you want to validate:
   - **Source framebuffer hashing:** read back the stable internal `present_src` bytes (deterministic; avoids scaling/color-management ambiguity).
   - **Presentation pipeline validation:** read back the pixels that were actually rendered to the surface/canvas (catches scaling/gamma/alpha policy differences).
5. Compare against expected values with a tolerance

### Browser automation (Playwright)

Use Playwright to run the smoke tests across browsers:

- Run once with `?gpu=webgpu`
- Run once with `?gpu=webgl2`
- Assert:
  - initialization succeeded (or is skipped with a known “unsupported” code)
  - a frame was presented
  - the backend reported the expected capability flags

This integrates naturally with the approach described in
[12-testing-strategy.md](./12-testing-strategy.md#browser-testing).

Example developer workflow:

```bash
npm ci
node scripts/playwright_install.mjs chromium --with-deps
node scripts/playwright_install.mjs firefox --with-deps
node scripts/playwright_install.mjs webkit --with-deps
npx playwright test
```

### Benchmarks

Provide a lightweight benchmark mode that reports:

- average `present()` CPU time
- texture upload throughput (MB/s)
- pipeline creation time (ms)
- optional GPU time (WebGPU + timestamp queries)

Recommended trigger mechanisms:

- URL query param: `?bench=gpu&gpu=webgpu` (or `webgl2`)
- A `window.aeroDebug.runGpuBench()` entry point for manual runs

Example manual run:

1. Start the dev server (see the repo’s build docs/tooling; typically Vite).
2. Open `/?bench=gpu&gpu=webgpu` and then `/?bench=gpu&gpu=webgl2`.
3. Record the reported metrics along with the selected backend + capability flags.

---

## Appendix: minimal “GPU worker protocol” sketch

This is not a final API, but provides concrete message shapes to implement.

```ts
// Main -> GPU worker
type GpuInit = {
  type: "gpu:init";
  canvas: OffscreenCanvas;
  backend: "auto" | "webgpu" | "webgl2";
  size: { width: number; height: number };
  dpr: number;
};

type GpuResize = {
  type: "gpu:resize";
  size: { width: number; height: number };
  dpr: number;
};

type GpuShutdown = { type: "gpu:shutdown" };

// GPU worker -> Main
type GpuReady = {
  type: "gpu:ready";
  backend: "webgpu" | "webgl2";
  caps: {
    hasCompute: boolean;
    hasTextureCompressionBC: boolean;
    hasTimestampQuery: boolean;
    maxTextureDimension2D: number;
    surfaceFormat: string;
  };
};

type GpuError = {
  type: "gpu:error";
  code:
    | "WEBGPU_UNSUPPORTED"
    | "WEBGPU_DEVICE_LOST"
    | "WEBGL2_UNSUPPORTED"
    | "WEBGL_CONTEXT_LOST"
    | "CAPS_INSUFFICIENT";
  message: string;
  canRecover: boolean;
};
```
