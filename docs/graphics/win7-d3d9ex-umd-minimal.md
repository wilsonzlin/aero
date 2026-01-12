# Windows 7 DWM/Aero: Minimal Direct3D 9Ex UMD (WDDM 1.1) Implementation Guide

This document is a **focused, implementation-ready spec** for the *user-mode driver (UMD)* side of a **Direct3D 9Ex** WDDM stack that is “just enough” to get **Windows 7 Desktop Window Manager (dwm.exe) + Aero Glass** running.

It is intentionally not a full D3D9 driver guide. It enumerates the **DDI entrypoints**, **caps**, and **behavioral contracts** that matter for DWM, and suggests conservative defaults that avoid DWM falling back to the Basic theme.

> Header references: symbol names match Windows 7-era WDK headers (`d3d9types.h`, `d3d9caps.h`, `d3dumddi.h` / `d3d9umddi.h` / `d3dhal.h` depending on kit). Some structures have multiple “v1/v2” variants across WDDM revisions; when in doubt, implement the newest version your chosen `D3D_UMD_INTERFACE_VERSION` requires and provide compatible fallbacks.

For the next API tier up (D3D10/D3D11 on Win7: SM4/SM5 + DXGI swapchains), see:

* `docs/graphics/win7-d3d10-11-umd-minimal.md`

---

## Scope / assumptions (read this first)

This guide assumes:

* **Windows 7 SP1**, **WDDM 1.1**.
* You are implementing a **D3D9Ex-capable** UMD (i.e. the runtime can create `IDirect3D9Ex` / `IDirect3DDevice9Ex`).
* You want **dwm.exe composition enabled** (Aero), not just “a D3D9 device exists”.
* Kernel-mode details (VidMm, scheduling, allocations) are referenced only insofar as they affect UMD behavior; this document is still **UMD-focused**.

Non-goals for a first pass:

* Full D3D9 game compatibility.
* Complex fixed-function pipeline quirks.
* Advanced queries (occlusion, pipeline stats), multi-sampling, cubemaps/volume textures, D3D9Ex video decode, etc.

---

## 1) How Windows 7 uses D3D9Ex for DWM (pipeline expectations)

### 1.1 High-level composition model

On Windows 7, **DWM is the compositor**. Applications render window contents into **redirected surfaces**; DWM composites those surfaces into the final desktop image each frame.

Key implications for the D3D9Ex UMD:

* DWM runs a **continuous render loop** (typically **~60 Hz**, vsync paced).
* DWM uses **render-to-texture + textured quad composition**:
  * Window surfaces are sampled as **textures**.
  * The desktop backbuffer is a **render target**.
  * Composition is mostly **2D**: screen-space triangles/quads with alpha blending.
* DWM uses **shaders**, not only fixed function:
  * **Vertex shader**: transforms a simple vertex format to clip space; can apply per-window transforms.
  * **Pixel shader**: samples window textures; applies alpha, color matrix, and effects (including blur for glass regions).
* DWM relies heavily on **resource sharing**:
  * DWM must be able to **open textures/surfaces shared by other processes** (each app’s redirected surface).

### 1.2 Swap chain / present cadence

DWM typically creates:

* One `IDirect3DSwapChain9Ex` per monitor (or a single swap chain spanning the desktop, depending on topology).
* **Windowed** swap chains that target the desktop/primary.
* Presentation with **vsync interval 1** is the normal steady-state.

DDI expectations:

* `Present`/`PresentEx` is called **once per composed frame**.
* DWM can use **dirty rects** and **scroll rects** (don’t require perfect optimization; correctness matters first).
* Present must not block indefinitely; if you block inside Present waiting for work that never completes, DWM will hang and may trigger **TDR**.

#### 1.2.1 Typical swap chain parameters (what you should be ready to see)

While exact values differ by GPU/driver, DWM commonly uses a presentation setup equivalent to:

* `Windowed = TRUE`
* `SwapEffect = D3DSWAPEFFECT_FLIPEX` (preferred on Ex) or `D3DSWAPEFFECT_DISCARD`
* `BackBufferFormat = D3DFMT_X8R8G8B8` (sometimes `A8R8G8B8`)
* `BackBufferCount = 1` (double-buffered) or occasionally more
* `PresentationInterval = D3DPRESENT_INTERVAL_ONE`
* `hDeviceWindow = <DWM's top-level composition window>`

At the DDI level this translates into `pfnCreateSwapChain` + `pfnPresent` calls. Even if you implement presentation as a blit/copy internally, **behave like flip** when `D3DSWAPEFFECT_FLIPEX` is used: the presented buffer becomes the new front buffer and must not be rewritten until it is no longer queued for scanout.

### 1.3 Surface usage patterns (what DWM actually does)

Common DWM operations that should work:

* `Clear` the backbuffer.
* `StretchRect` / blit between surfaces (used for scaling and intermediate passes).
* Fullscreen (or window-rect) quad draws with:
  * Alpha blending.
  * Scissor clipping.
  * Linear filtering for scaled window textures.
* Occasional `ColorFill`.
* Upload/update paths for window content (coming from other processes); DWM itself usually **samples** those surfaces, not CPU-locks them.

### 1.4 Synchronization expectations (DWM “don’t hang me” rules)

DWM throttles itself and checks GPU progress using **queries/fences**. A minimal UMD must provide:

* An **event-like query** that can be “ended” and later polled for completion.
* `Flush` that actually submits work so queries can complete.
* `GetQueryData` that returns “not ready yet” without blocking.

If these are missing or never signal, DWM can spin at 100% CPU, stall forever, or decide the GPU is hung and disable composition.

---

## 2) Minimum D3D9 DDI entrypoints to implement (for DWM)

This section enumerates the **minimal adapter/device entrypoints** a D3D9Ex-capable UMD should implement for DWM.

The exact entrypoint surface is controlled by the UMD interface version you advertise (e.g. `D3D_UMD_INTERFACE_VERSION`). Regardless of the versioning, the *functional* requirements below remain the same.

### 2.1 UMD exports: adapter open/close

Implement these exported entrypoints (names as in WDK):

| Export | Prototype typedef | Required? | Minimal semantics |
|---|---|---:|---|
| `OpenAdapter` | `PFND3DDDI_OPENADAPTER` | Yes | Provide adapter handle + fill `D3DDDI_ADAPTERFUNCS` with pointers; store `D3DDDI_ADAPTERCALLBACKS`. |
| `OpenAdapterFromHdc` | `PFND3DDDI_OPENADAPTERFROMHDC` | Highly recommended | Many components still open adapters via HDC path; forward to common open logic. |
| `OpenAdapterFromLuid` | `PFND3DDDI_OPENADAPTERFROMLUID` | Yes for Win7 robustness | D3D9Ex paths frequently open adapters by LUID. |

Notes:

* “Close” is typically through adapter function table (e.g. `pfnCloseAdapter`), not an export.
* Always support **one adapter instance per LUID**; DWM and multiple D3D clients expect consistent identity.

**AeroGPU-specific discovery:** during adapter open, query the KMD for the active AeroGPU device ABI + feature bits via:

* `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)`

Decode the returned `aerogpu_umd_private_v1` from:

* `drivers/aerogpu/protocol/aerogpu_umd_private.h`

This allows the UMD to gate optional paths (vblank pacing, fence pages, etc.) on reported feature bits, and to determine whether it is running against the legacy `"ARGP"` device (optional; feature-gated behind `emulator/aerogpu-legacy`) or the versioned `"AGPU"` ABI.

### 2.2 Adapter function table: minimum `D3DDDI_ADAPTERFUNCS`

Your `OpenAdapter*` must return an adapter funcs table with (at least) the following implemented:

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnGetCaps` | Yes | Handle the caps queries D3D9Ex + DWM uses (see §3). Return consistent caps across calls. | Unused caps types can return `D3DDDIERR_INVALIDPARAMS`/`E_INVALIDARG` *if you are sure they aren’t queried*, but safest is to return “not supported” sizes/zeros for unknown types. |
| `pfnCreateDevice` | Yes | Create a device/context; return `hDevice`; fill `D3DDDI_DEVICEFUNCS`. | N/A |
| `pfnCloseAdapter` | Yes | Free adapter-private allocations; invalidate handles. | N/A |
| `pfnQueryAdapterInfo` | Recommended | Return stable answers for core `D3DDDIQUERYADAPTERINFO_*` requests used by runtime (driver info, WDDM model, etc.). | For unknown query types, prefer returning `S_OK` with a zeroed output buffer (and logging once) so unexpected runtime/DWM probes do not break device bring-up. |

Practical note: if `pfnQueryAdapterInfo` is wrong/incomplete, the runtime can still create devices, but DWM may refuse composition due to missing “driver model / feature” information.

#### 2.2.1 `pfnGetCaps`: minimum query types to support

`pfnGetCaps` is where D3D9Ex learns what you support. The runtime may call it directly and indirectly (to implement API calls like `CheckDeviceFormat`).

Support at least these `D3DDDICAPS_TYPE`-style requests (names vary slightly by header/version):

* **D3D9 device caps**: `D3DDDICAPS_GETD3D9CAPS` / `D3DDDICAPS_D3D9CAPS`
  * Output: `D3DCAPS9`
* **Format enumeration**: `D3DDDICAPS_GETFORMATCOUNT` and `D3DDDICAPS_GETFORMAT`
  * Used to enumerate `D3DDDIFORMAT` / `D3DFORMAT` support across usages.
* **StretchRect filter caps** and related blit capabilities (often folded into the `D3DCAPS9` output).
* **Multisample quality levels**: `D3DDDICAPS_GETMULTISAMPLEQUALITYLEVELS`
  * You can return “none supported” if you do not advertise MSAA, but the query itself should not fail.

Treat unknown caps types conservatively:

* If you don’t recognize a caps request, log it and return a clean “unsupported” result rather than crashing.
* Prefer returning `S_OK` with a zeroed output buffer (or otherwise deterministic “unsupported” output) over failing the call. If you must fail, prefer `E_INVALIDARG` over `E_FAIL`.

### 2.3 Device function table: minimum `D3DDDI_DEVICEFUNCS`

Below is the minimal functional surface. Many functions are “simple state setters”: they must accept calls, store state, and feed state to your backend at draw time.

#### 2.3.1 Device lifecycle / submission

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnDestroyDevice` | Yes | Free all device state; ensure no outstanding callbacks reference freed memory. | N/A |
| `pfnFlush` | Yes | Submit accumulated work so it becomes visible to the scheduler and queries can complete. Must return quickly. | Do not block waiting for GPU; keep it async. |
| `pfnWaitForIdle` | Recommended | Block until previously submitted work completes (used by some reset paths). | Can be implemented as “flush + wait on last fence”; avoid busy-wait. |
| `pfnReset` / `pfnResetEx` (version-dependent) | Recommended | Handle display mode / swap chain recreation without requiring a full device destroy. | For a first pass you can internally destroy and recreate swap chain resources, but keep the same `hDevice`. |

#### 2.3.2 Swap chain and presentation

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnCreateSwapChain` | Yes | Create swap chain + backbuffers. Support at least one backbuffer (double buffering recommended). | Multi-sample can be rejected if not advertised. |
| `pfnDestroySwapChain` | Yes | Destroy swap chain and its implicit resources. | N/A |
| `pfnCheckDeviceState` (D3D9Ex) | Recommended | Return `S_OK`, `S_PRESENT_OCCLUDED`, or `S_PRESENT_MODE_CHANGED` based on the destination window/monitor state. DWM uses this to decide whether to keep composing. | For early bring-up you can conservatively return `S_OK` always (composition stays enabled), but you must never block. |
| `pfnPresent` | Yes | Present composed backbuffer to the desktop. Implement `PresentEx`-style flags if surfaced through `D3DDDIARG_PRESENT::Flags`. | You can ignore dirty rect optimizations initially, but must obey clipping/rect correctness (i.e. don’t read invalid memory). |
| `pfnGetPresentStats` | Recommended | Return monotonically increasing present counters/timestamps. DWM may use this for pacing/diagnostics. | Can return zeros except a monotonic present count; don’t fail the call. |
| `pfnWaitForVBlank` | Optional | If called, block until next vblank or emulate a vblank tick. | Safe stub: sleep for ~1 refresh interval if you have timing; otherwise return `S_OK`. |

**Present return codes (important):**

* For occlusion/minimized scenarios, `PresentEx` paths expect `S_PRESENT_OCCLUDED`.
* For mode changes, `S_PRESENT_MODE_CHANGED` can be returned.
* Avoid returning `D3DERR_DEVICELOST`/`D3DDDIERR_DEVICEHUNG` unless truly fatal; DWM may disable composition.

#### 2.3.3 Resource creation / destruction / sharing

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnCreateResource` | Yes | Create textures/surfaces/buffers with correct bind flags: render target, texture sampling, dynamic/lockable. Must support **non-power-of-two** sizes. | Reject resource types you don’t advertise (volume/cube). |
| `pfnOpenResource` / `pfnOpenResource2` (version-dependent) | Yes for Aero | Open a shared resource created in another process. This is critical for redirected surfaces. | If you don’t support cross-process sharing, DWM won’t compose real apps. |
| `pfnDestroyResource` | Yes | Free resource and associated allocations; handle refcounting for shared resources (close vs destroy). | N/A |
| `pfnSetPriority` / `pfnQueryResourceResidency` | Optional | Priorities/residency queries can be conservative. | Safe stub: treat everything as resident, fixed priority. |

Resource types DWM commonly needs:

* `D3DDDIRESTYPE_SURFACE`
* `D3DDDIRESTYPE_TEXTURE`
* `D3DDDIRESTYPE_VERTEXBUFFER` (small dynamic buffers for quads)
* `D3DDDIRESTYPE_INDEXBUFFER` (optional; DWM may use non-indexed draws)

#### 2.3.4 CPU access: Lock/Unlock and update paths

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnLock` | Yes (for robustness) | Provide CPU mapping for lockable resources. Respect discard/no-overwrite semantics for dynamic buffers/textures where possible. | You may reject locks on DEFAULT render targets if you don’t advertise lockability. |
| `pfnUnlock` | Yes | Commit CPU writes; mark dirty ranges/rects (see §4). | N/A |
| `pfnUpdateSurface` / `pfnUpdateTexture` | Recommended | Provide fast upload/copy paths that the runtime uses as an alternative to lock/copy/unlock. | Can be internally implemented as a blit. |
| `pfnBlt` (StretchRect) | Yes | Copy/scale between surfaces; support filtering (point/linear) as requested. | If you don’t support scaling, don’t advertise it; but DWM uses scaling frequently, so implement at least point/linear. |
| `pfnColorFill` | Recommended | Fill a rect with a color. | Can be implemented as a tiny draw/blit; don’t fail. |
| `pfnClear` | Yes | Clear color target (and depth/stencil if you provide them). | If you don’t support depth/stencil, ignore those flags. |

#### 2.3.5 Shader creation and binding

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnCreateVertexShader` | Yes | Accept D3D9 shader token stream (vs_2_0 minimum). Compile/translate to backend. | You may reject shader models you don’t advertise (e.g. vs_3_0). |
| `pfnDeleteVertexShader` | Yes | Free shader. | N/A |
| `pfnSetVertexShader` | Yes | Bind current VS for subsequent draws. | N/A |
| `pfnSetVertexShaderConstF` | Yes | Store float constant registers; DWM uses these for transforms/effect params. | Int/bool variants can be stubbed if never called, but implementing them is easy and reduces risk. |
| `pfnCreatePixelShader` | Yes | Accept ps_2_0 minimum; used heavily by DWM for sampling/blend/effects. | Same as VS. |
| `pfnDeletePixelShader` | Yes | Free shader. | N/A |
| `pfnSetPixelShader` | Yes | Bind current PS. | N/A |
| `pfnSetPixelShaderConstF` | Yes | Store float constants. | Same as VS consts. |

Practical note: for a first-pass compositor, you can implement a narrow “shader translator” that supports only the instruction subset DWM emits. Use tracing (see §6) to expand.

#### 2.3.6 Fixed-function and render state (DWM uses only a subset)

Implement the following state-setting entrypoints and treat unknown state values as “store and ignore” rather than failing.

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnSetRenderTarget` | Yes | Bind the active render target(s). DWM typically uses MRT=1. | MRT>1 can be rejected if not advertised. |
| `pfnSetDepthStencil` | Optional | DWM is mostly 2D; but depth/stencil may be used for clip masks. | If unsupported, avoid advertising depth/stencil formats and accept calls as no-ops. |
| `pfnSetViewport` | Yes | Track viewport; map to backend viewport/scissor. | N/A |
| `pfnSetScissorRect` | Yes | Required for window clipping. | N/A |
| `pfnSetRenderState` | Yes | Must support alpha blend, cull, z-enable (even if ignored), color write mask. | Don’t fail unknown render states; store them. |
| `pfnSetTexture` | Yes | Bind textures for sampling (stage 0..N). DWM uses few stages. | Higher stages can be accepted and ignored if never used. |
| `pfnSetSamplerState` | Yes | Must support MIN/MAG filters and address modes; DWM relies on linear filtering. | Anisotropy can be ignored unless advertised. |
| `pfnSetTextureStageState` | Optional | If DWM uses shaders, this may be unused. | Safe behavior: accept and ignore. |
| `pfnSetFVF` | Optional | If the runtime routes DWM through FVF-based vertex formats, map FVF to an internal vertex layout (or synthesize a vertex declaration). | Safe behavior: accept and ignore only if you are sure DWM uses vertex declarations instead. |
| `pfnCreateVertexDeclaration` | Yes | Convert `D3DVERTEXELEMENT9`-style declarations into backend layouts. | N/A |
| `pfnDeleteVertexDeclaration` | Yes | Free declaration. | N/A |
| `pfnSetVertexDeclaration` | Yes | Bind declaration. | N/A |
| `pfnSetStreamSource` | Yes | Bind vertex buffer + stride/offset. | N/A |
| `pfnSetIndices` | Optional | If DWM uses indexed draws. | Implement anyway; it’s commonly exercised. |

Other D3D9-era state APIs (lights, materials, fog, texture transform matrices) can generally be stubbed as no-ops if you avoid advertising fixed-function reliance and DWM sticks to shaders.

**Render/sampler state that should work correctly for DWM:**

At minimum, implement the behavior of these states (store them and apply in your backend pipeline):

* Render states (`D3DRENDERSTATETYPE` via `pfnSetRenderState`):
  * `D3DRS_ALPHABLENDENABLE`
  * `D3DRS_SRCBLEND`, `D3DRS_DESTBLEND`, `D3DRS_BLENDOP`
  * `D3DRS_SEPARATEALPHABLENDENABLE` (if enabled by DWM; safe to support)
  * `D3DRS_SRCBLENDALPHA`, `D3DRS_DESTBLENDALPHA`, `D3DRS_BLENDOPALPHA`
  * `D3DRS_COLORWRITEENABLE`
  * `D3DRS_SCISSORTESTENABLE`
  * `D3DRS_CULLMODE`
  * `D3DRS_ZENABLE`, `D3DRS_ZWRITEENABLE` (even if you effectively ignore depth)
  * `D3DRS_SRGBWRITEENABLE` (optional; improves correctness for desktop gamma)
* Sampler states (`D3DSAMPLERSTATETYPE` via `pfnSetSamplerState`):
  * `D3DSAMP_ADDRESSU`, `D3DSAMP_ADDRESSV` (DWM commonly uses `D3DTADDRESS_CLAMP`)
  * `D3DSAMP_MINFILTER`, `D3DSAMP_MAGFILTER`, `D3DSAMP_MIPFILTER` (DWM commonly uses LINEAR for scaling)
  * `D3DSAMP_SRGBTEXTURE` (optional; only if you also implement sRGB sampling correctly)

If you implement only one blend mode initially, make sure you cover standard premultiplied-alpha composition:

* `SRCBLEND = ONE`, `DESTBLEND = INVSRCALPHA`, `BLENDOP = ADD`

#### 2.3.7 Draw calls

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnBeginScene` / `pfnEndScene` | Recommended | Track scene bracketing; some runtimes expect it for validation/flush decisions. | Can be no-ops returning `S_OK`. |
| `pfnDrawPrimitive` | Yes | Translate triangles/triangle strips for screen-space geometry. | N/A |
| `pfnDrawIndexedPrimitive` | Yes (robustness) | Same as above with indices. | N/A |
| `pfnDrawPrimitive2` / `pfnDrawIndexedPrimitive2` | Recommended | Handles `Draw*UP` paths where vertex data comes from user memory. | If you don’t implement, ensure runtime never routes DWM through UP draws (hard to guarantee). |

#### 2.3.8 Queries and synchronization (must not hang)

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnCreateQuery` | Yes | Support at least `D3DQUERYTYPE_EVENT` (or the DDI equivalent query type). | Reject unsupported query types with a clean failure (e.g. `D3DERR_NOTAVAILABLE`). |
| `pfnDestroyQuery` | Yes | Free query object. | N/A |
| `pfnIssueQuery` | Yes | On `END`, insert a fence into your backend queue. | `BEGIN` can be ignored for event queries. |
| `pfnGetQueryData` | Yes | Non-blocking poll: return `S_OK` when complete, else `S_FALSE`/`D3DERR_WASSTILLDRAWING` depending on API contract. Must not spin inside. | Timestamp queries etc can be unimplemented. |

**Rule:** if your backend is async (WebGPU, Vulkan, etc), the query completion path must be backed by a real fence/timeline so progress is guaranteed.

**Practical flag quirk:** for EVENT queries, be permissive about the `IssueQuery` flag encoding. Some D3D9Ex paths have been observed to pass `flags=0` for “END”, and some DDI header vintages use `0x2` for END at the DDI boundary. Treat `(flags == 0) || (flags & 0x1) || (flags & 0x2)` as END for EVENT queries.

Guest-side validation:

  * `drivers/aerogpu/tests/win7/d3d9ex_event_query` verifies `D3DQUERYTYPE_EVENT` completion behavior and that `GetData(D3DGETDATA_DONOTFLUSH)` remains non-blocking (including an initial poll before `Flush`; DWM relies on this polling pattern).

#### 2.3.9 State blocks and validation helpers (recommended for app compatibility)

While DWM itself typically relies on shaders + explicit state setting, many D3D9 apps (and some runtimes) use **state blocks** and **ValidateDevice**:

| Func pointer | Required? | Minimal semantics | What can be stubbed |
|---|---:|---|---|
| `pfnBeginStateBlock` / `pfnEndStateBlock` | Recommended | Begin/finish capturing a stateblock via subsequent DDI calls. | If unimplemented, return a clean failure (`D3DERR_INVALIDCALL`/`D3DERR_NOTAVAILABLE`) rather than crashing. |
| `pfnCreateStateBlock` | Recommended | Create a stateblock for `D3DSBT_ALL` / `D3DSBT_PIXELSTATE` / `D3DSBT_VERTEXSTATE`. | Same as above. |
| `pfnCaptureStateBlock` / `pfnApplyStateBlock` | Recommended | Capture current state into a block, and apply a block back to the device state. | N/A |
| `pfnDeleteStateBlock` | Recommended | Free a state block. | N/A |
| `pfnValidateDevice` | Recommended | Return a conservative pass count (typically `1`) for the supported shader pipeline. | Avoid hard-failing unless truly unsupported; callers often treat ValidateDevice as an advisory probe. |

Practical notes:

- State blocks are primarily about *state capture/restore*, not rendering. A minimal but robust first pass can cache the state you already track for your command stream and replay it on Apply.
- Some apps create a new stateblock by doing `BeginStateBlock → Apply(existing) → EndStateBlock`. In this scenario, **Apply must record state** into the in-progress capture even if the apply is a no-op.

Guest-side validation:

* `drivers/aerogpu/tests/win7/d3d9ex_stateblock_sanity` covers Begin/End + Create/Capture/Apply and includes the “nested Apply while recording” pattern.
* `drivers/aerogpu/tests/win7/d3d9_validate_device_sanity` covers `ValidateDevice`.

---

## 3) Capability reporting (what to report so Aero enables)

Windows 7 enables DWM composition only if it believes the adapter can sustain the compositor workload. You want to report **the minimum set that satisfies DWM** while avoiding caps that cause the runtime to exercise unimplemented paths.

### 3.1 Core “Aero gate” requirements (practical)

At the API level, DWM expects (indirectly via DDI caps):

* **WDDM driver model** (not XDDM).
* **D3D9Ex availability** (device creation succeeds through Ex path).
* **Shader Model 2.0 minimum**:
  * `D3DCAPS9::VertexShaderVersion >= D3DVS_VERSION(2,0)`
  * `D3DCAPS9::PixelShaderVersion  >= D3DPS_VERSION(2,0)`
* **Windowed rendering support**:
  * `D3DCAPS9::Caps2` includes `D3DCAPS2_CANRENDERWINDOWED`.
* **Shared resources** (redirected surfaces):
  * `D3DCAPS9::Caps2` includes `D3DCAPS2_CANSHARERESOURCE` (and you must implement `pfnOpenResource*` correctly).
* **Non-power-of-two textures** for arbitrary window sizes.
* **Alpha blending** and **linear filtering**.

### 3.2 Required formats (conservative minimal set)

Make the following formats work for the usage DWM needs:

**Render target / swap chain:**

* `D3DFMT_X8R8G8B8` (desktop backbuffer common)
* `D3DFMT_A8R8G8B8` (composition surfaces with alpha)

**Texture sampling:**

* `D3DFMT_A8R8G8B8` (window textures)
* `D3DFMT_X8R8G8B8` (opaque surfaces)

**Depth/stencil (optional but increases robustness):**

* `D3DFMT_D24S8` or `D3DFMT_D16`

If you do not implement depth/stencil correctly, do **not** report these as supported; prefer a pure-2D compositor first.

### 3.3 Caps knobs to set (D3DCAPS9-focused)

These are commonly required by compositor-style workloads:

* `D3DCAPS9::RasterCaps`:
  * `D3DPRASTERCAPS_SCISSORTEST` (DWM clips constantly)
* `D3DCAPS9::TextureFilterCaps` (at least):
  * `D3DPTFILTERCAPS_MINFPOINT`, `D3DPTFILTERCAPS_MINFLINEAR`
  * `D3DPTFILTERCAPS_MAGFPOINT`, `D3DPTFILTERCAPS_MAGFLINEAR`
* `D3DCAPS9::SrcBlendCaps` / `DestBlendCaps`:
  * `D3DPBLENDCAPS_ONE`, `D3DPBLENDCAPS_ZERO`
  * `D3DPBLENDCAPS_SRCALPHA`, `D3DPBLENDCAPS_INVSRCALPHA`
* `D3DCAPS9::MaxTextureWidth/Height`:
  * Must be at least the maximum expected window size (recommend **4096** to start; 8192 if easy).
* `D3DCAPS9::MaxSimultaneousTextures`:
  * DWM typically uses 1–4; advertise **4** safely.
* `D3DCAPS9::MaxStreams`:
  * At least 1; advertise **1–4**.
* `D3DCAPS9::StretchRectFilterCaps`:
  * Include at least point+linear (DWM uses `StretchRect` for scaling and intermediate passes).
* `D3DCAPS9::PresentationIntervals`:
  * Include at least `D3DPRESENT_INTERVAL_ONE` and optionally `D3DPRESENT_INTERVAL_IMMEDIATE` (don’t advertise intervals you can’t honor).
* Non-power-of-two textures:
  * Ensure `D3DCAPS9::TextureCaps` does **not** force `D3DPTEXTURECAPS_POW2`.
  * If you only support restricted NPOT, set `D3DPTEXTURECAPS_NONPOW2CONDITIONAL` and follow its rules; otherwise support full NPOT.

### 3.4 What to avoid advertising initially (to reduce surface area)

Do **not** advertise features until you have test coverage and tracing proving correctness:

* Multi-sampling / MSAA.
* `D3DFMT_A16B16G16R16F`, HDR, wide gamut.
* Cubemaps, volume textures.
* Autogen mipmaps (`D3DUSAGE_AUTOGENMIPMAP`).
* Advanced blend ops if you don’t implement them (`D3DBLENDOP_*` beyond ADD).
* Query types beyond event (timestamps, occlusion).
* Any “driver-managed memory tricks” that change lock semantics (unless implemented).

The general strategy is:

1. **Advertise only the formats/usages you implement**.
2. If you see DWM calling an unimplemented DDI path, either:
   * implement it, or
   * stop advertising the capability that triggers it.

---

## 4) Resource + memory update model (Lock/Unlock, dirty tracking)

This is the “make it not glitch” contract for a compositor workload.

### 4.1 Treat `Unlock` as the commit point

For lockable resources:

* `pfnLock` returns:
  * a CPU pointer (or “pitch + pointer” for surfaces),
  * plus whatever metadata the runtime expects (row pitch, slice pitch).
* `pfnUnlock` must:
  * **record dirty ranges/rects**,
  * schedule upload to the backend before the resource is next used for drawing/sampling.

A minimal but robust model:

* Keep a per-subresource `dirty` flag and `dirty_rect` (union of all writes) or `dirty_range` for buffers.
* On unlock, merge the region.
* On first use after dirty, upload the dirty region (or whole resource if simpler).

### 4.2 Dynamic buffers: discard/no-overwrite semantics

Even if DWM is not a game, it often uses small dynamic vertex buffers.

Support these flags if present in `D3DDDIARG_LOCK::Flags`:

* **Discard**: allocate a fresh backing store (or advance a ring buffer) so GPU reads aren’t stalled.
* **NoOverwrite**: allow CPU writes without forcing a GPU sync; safe if you use a ring buffer strategy.

If you can’t implement them correctly yet, it is better to:

* allow `Discard` and treat it as “new allocation” always, and
* treat `NoOverwrite` the same as a normal lock (conservative).

### 4.3 Render targets: avoid CPU readback

DWM mainly uses render targets as GPU-only intermediates. For a first pass:

* Make DEFAULT render targets **not lockable**.
* If the runtime tries to lock them, return `D3DERR_INVALIDCALL` *only if you are sure this path is not required for DWM*; otherwise provide a slow readback path.

### 4.4 Shared resources (critical for redirected surfaces)

When implementing `pfnOpenResource*`:

* The opened resource must alias the same underlying storage as the creator’s resource.
* Reference counting matters:
  * “Destroy resource” in one process should not free storage if another process has it open.
* Synchronization:
  * DWM will sample a window texture while the app is rendering into it.
  * Correctness for the first pass can be “last completed content”; perfect cross-process fencing can come later, but avoid tearing by ensuring present/flush boundaries publish updates.

Pragmatic approach for early bring-up:

* Treat `Present`/`Flush` as publishing points:
  * When the producing process presents or flushes, the shared surface content becomes visible to DWM on the next frame.

Guest-side validation:

* `drivers/aerogpu/tests/win7/d3d9ex_shared_surface` exercises the cross-process “create shared → open shared” path.
  * Validates cross-process pixel sharing via readback by default.
  * Pass `--no-validate-sharing` to focus on open + minimal submit (`ColorFill` + `Flush`) only (`--dump` always validates).

---

## 5) Error handling & stability (avoid TDR, avoid falling back to Basic)

### 5.1 Never block indefinitely inside a DDI call

Rules for DWM stability:

* All DDI entrypoints must return in **milliseconds**, not seconds.
* Any waiting must be bounded and tied to a real fence.
* If a query isn’t ready, return “not ready” (`S_FALSE` / `D3DERR_WASSTILLDRAWING`) rather than blocking.

Guest-side validation:

* `drivers/aerogpu/tests/win7/d3d9ex_dwm_ddi_sanity` exercises the DWM-critical D3D9Ex probes (device state checks, PresentEx throttling, vblank waits, present stats, residency, etc.) and asserts that each call remains non-blocking (per-call latency bound).

### 5.2 Keep GPU work chunks small

Windows TDR is designed to reset GPUs that appear hung. Even in emulation/translation:

* Break long shader compilations out of the render thread if possible (compile async, cache results).
* Avoid submitting huge command buffers in a single flush.
* Ensure `pfnFlush` actually advances some “completed fence” over time.

### 5.3 Return codes DWM tends to tolerate vs. ones that disable composition

**Generally tolerated (use for non-fatal conditions):**

* `S_OK`
* `S_FALSE` / `D3DERR_WASSTILLDRAWING` (query/data not ready)
* `S_PRESENT_OCCLUDED` (present when minimized/occluded)
* `S_PRESENT_MODE_CHANGED` (display mode changed; DWM will rebuild)

**Dangerous (often triggers Basic theme or device reset paths):**

* `D3DERR_DEVICELOST`
* `D3DERR_DEVICEREMOVED`
* `D3DDDIERR_DEVICEHUNG`
* `E_FAIL` in core rendering paths (CreateDevice, Present, CreateResource)

Strategy:

* For **unimplemented state**, prefer “accept + ignore + return `S_OK`”.
* For **unimplemented features**, avoid advertising the cap so the runtime never calls it.
* For **true allocation failures**, `E_OUTOFMEMORY` / `D3DERR_OUTOFVIDEOMEMORY` is better than generic `E_FAIL`, but expect DWM to potentially disable composition if resources can’t be created.

---

## 6) Suggested tracing methodology (Win7 VM bring-up plan)

Goal: confirm that your “minimal subset” matches what DWM actually calls, and catch the first missing function/cap quickly.

### 6.1 UMD-side instrumentation (fastest feedback loop)

Add structured logging at every DDI entrypoint.

In this repo, the AeroGPU D3D9 UMD already includes an **in-process DDI call trace facility** (ring buffer + one-shot dump triggers):

* `docs/graphics/win7-d3d9-umd-tracing.md`

For example, to quickly identify the first stubbed DDI hit:

```cmd
set AEROGPU_D3D9_TRACE=1
set AEROGPU_D3D9_TRACE_MODE=unique
set AEROGPU_D3D9_TRACE_FILTER=stub
set AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1
set AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1
```

* Print:
  * function name,
  * thread id,
  * key handles (`hDevice`, `hResource`, `hSwapChain`, `hQuery`),
  * sizes/formats/usages,
  * and return `HRESULT`.
* Include a monotonically increasing **frame id** incremented on `pfnPresent`.
* Use a ring buffer + conditional flush to avoid slowing down DWM.

This alone is usually enough to discover:

* which caps queries you must answer,
* which DDI functions get hit during logon,
* and which state/render paths DWM uses on your configuration.

### 6.2 ETW/GPUView: validate scheduling and “no hangs”

On Windows 7, use ETW to correlate DWM pacing with GPU work submission:

* Providers to capture (common names):
  * `Microsoft-Windows-Dwm-Core`
  * `Microsoft-Windows-DxgKrnl`
  * `Microsoft-Windows-Win32k`
* Tools:
  * `xperf`/`wpr` (depending on what’s installed in the VM)
  * GPUView for visualization

Look for:

* regular present cadence,
* no multi-second stalls,
* queries completing and allowing DWM to advance frames.

### 6.3 PIX for Windows (D3D9) call-level inspection

PIX for Windows 7 can capture D3D9 call streams (API-level, not DDI-level). It’s useful to learn:

* what shaders DWM uses,
* what render states are set,
* what resources/formats are created.

Practical approach:

1. Disable composition (switch to Basic) so you can safely restart DWM.
2. Launch/attach PIX to `dwm.exe` (or a small D3D9Ex test app that mimics DWM’s usage).
3. Re-enable composition and capture a few frames.

Even if PIX cannot attach to the system DWM process in your environment, capturing a test app still validates that your DDI supports the expected D3D9Ex patterns.

### 6.4 Expected call sequence (sanity checklist)

During logon / enabling Aero you should see roughly:

1. Adapter open:
   * `OpenAdapterFromLuid` (common) + `pfnGetCaps` / `pfnQueryAdapterInfo`
2. Device bring-up:
   * `pfnCreateDevice`
   * `pfnCreateSwapChain` (+ backbuffer resources)
3. Pipeline setup:
   * `pfnCreateVertexDeclaration`
   * `pfnCreateVertexShader`, `pfnCreatePixelShader`
   * A handful of `pfnCreateResource` for intermediate surfaces
4. Frame loop:
   * `pfnBeginScene` (optional)
   * many state setters (`pfnSet*`)
   * `pfnDrawPrimitive` / `pfnDrawIndexedPrimitive`
   * queries: `pfnIssueQuery` / `pfnGetQueryData`
   * `pfnEndScene` (optional)
   * `pfnPresent`

If you do not reach a steady Present loop, DWM likely failed composition and fell back. The first failing HRESULT in your logs is usually the reason.
