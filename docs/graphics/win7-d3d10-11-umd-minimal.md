# Win7 (WDDM 1.1) minimal D3D10 + D3D11 UMD spec (SM4/SM5) for AeroGPU

This document is an implementation-oriented checklist/spec for bringing up **Direct3D 10** and **Direct3D 11** on **Windows 7** in the AeroGPU WDDM stack, targeting a “stub → triangle → real apps” path.

**Scope (minimal):**

* Windows 7 SP1, **WDDM 1.1**, **DXGI 1.1**
* D3D10 runtime + D3D10 UMD DDI (`d3d10umddi.h`)
* D3D11 runtime + D3D11 UMD DDI (`d3d11umddi.h`) with initial feature level **FL10_0**
* Shader models **SM4.x** first (`vs_4_0`, `ps_4_0`, optional `gs_4_0`), roadmap to **SM5.0** (`*_5_0`)
* **Windowed swapchain only** initially (DWM composition)

**Non-goals (initial bring-up):**

* Exclusive fullscreen / flip-model swapchains (Win8+ only)
* Tessellation (HS/DS), UAV-heavy compute, tiled resources, video decode, DXGI 1.2+
* Performance tuning beyond “correct enough” to run common apps

**Related AeroGPU docs (read alongside this one):**

* `docs/graphics/win7-wddm11-aerogpu-driver.md` — KMD+UMD architecture, memory model, fence/vblank requirements, and the guest↔emulator command transport.
* `docs/graphics/win7-aerogpu-validation.md` — bring-up/stability checklist (TDR avoidance, vblank pacing, debug playbook).

> Header references: the names in this doc match the WDK user-mode DDI headers:
> `d3d10umddi.h`, `d3d10_1umddi.h` (optional), `d3d11umddi.h`, and for swapchain/present: `dxgiddi.h`.

---

## 1) Driver model overview (Windows 7 / WDDM 1.1)

### 1.1 Where DXGI, D3D10/11 runtime, and the UMD fit

On Windows 7 the graphics API call flow (windowed) is roughly:

```
App
  ├─ D3D10 / D3D11 API (d3d10.dll / d3d11.dll)
  │    └─ D3D10/11 Runtime (validates, marshals, batching)
  │         └─ UMD DDI calls (your DLL: atidxx*, nvumd*, etc)
  │              └─ KMD via D3DKMT thunks / command submission
  └─ DXGI (dxgi.dll)
       ├─ adapter enumeration (IDXGIFactory/IDXGIAdapter)
       └─ swapchain creation/present (IDXGISwapChain)
            └─ DXGI DDI (dxgiddi.h) into the display driver stack
```

**Key idea:** the D3D runtime drives almost everything via **DDI function tables** you provide. The UMD does *not* expose the D3D API; it exposes **DDI entrypoints** and implements the low-level semantics (object creation, state binding, draws, resource updates, etc).

### 1.2 UMD entrypoints (“exports”) that the OS/runtime loads

At minimum, provide exports matching the DDI your driver supports:

* D3D10: `OpenAdapter10` (and optionally `OpenAdapter10_2` for 10.1)
  * signature: `HRESULT APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER *pOpenData)`
* D3D11: `OpenAdapter11`
  * signature: `HRESULT APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER *pOpenData)`
  * note: on Win7/WDDM 1.1 the D3D11 runtime still uses the `D3D10DDIARG_OPENADAPTER` container for adapter open; the D3D11-specific DDI begins with `D3D11DDIARG_CREATEDEVICE` / `D3D11DDIARG_GETCAPS`.

These are declared in the WDK headers (`d3d10umddi.h`, `d3d11umddi.h`) and receive a single `_Inout_ ...ARG_OPENADAPTER*` which contains:

* pointers to the runtime **callback tables** (`D3D10DDI_ADAPTERCALLBACKS` / `D3D11DDI_ADAPTERCALLBACKS`)
* a place for you to return the **adapter function table** (`D3D10DDI_ADAPTERFUNCS` / `D3D11DDI_ADAPTERFUNCS`)
* interface version negotiation (e.g. `D3D10DDI_INTERFACE_VERSION`, `D3D11DDI_INTERFACE_VERSION`)

From there, the runtime calls your adapter funcs’ `pfnCreateDevice(...)`, and device creation returns a `D3D10DDI_DEVICEFUNCS` / `D3D11DDI_DEVICEFUNCS` table plus your private `D3D*DDI_HDEVICE`.

### 1.3 “Handle + private memory” object model (critical for bring-up)

In the D3D10/11 DDI, most API objects are opaque handles such as:

* `D3D10DDI_HDEVICE`, `D3D10DDI_HRESOURCE`, `D3D10DDI_HRENDERTARGETVIEW`, `D3D10DDI_HVERTEXSHADER`, …
* `D3D11DDI_HDEVICE`, `D3D11DDI_HRESOURCE`, `D3D11DDI_HRENDERTARGETVIEW`, `D3D11DDI_HVERTEXSHADER`, …

The runtime owns handle allocation and typically also allocates **driver-private storage** for each object:

1. Runtime calls `pfnCalcPrivate*Size(...)` (e.g. `pfnCalcPrivateResourceSize`) to ask how many bytes you need.
2. Runtime allocates that many bytes and stores the pointer in the handle (e.g. `hResource.pDrvPrivate`).
3. Runtime calls `pfnCreate*(..., hXxx, hRTXxx)` and you write your object into `hXxx.pDrvPrivate`.

**Implication for AeroGPU:** implement a consistent “private object header” layout that stores:

* a stable object type tag (debugging)
* a host/emulator object ID (for the WebGPU side)
* resource/view descriptors that the translator needs at draw time

### 1.4 Error reporting rules

Many DDI entrypoints are `void` and cannot return `HRESULT`. When failing such a call, the driver must report the error via the runtime callback (commonly `pfnSetErrorCb(...)`) and then return.

For DDI functions that *do* return `HRESULT`, return:

* `S_OK` on success
* `E_OUTOFMEMORY`, `E_INVALIDARG`, or `E_NOTIMPL` as appropriate for unsupported features

### 1.5 AeroGPU-specific implementation layering (UMD → KMD → emulator)

This doc focuses on the *API contract* (D3D10/11 DDI) that the Microsoft runtimes will call. The implementation behind those entrypoints in AeroGPU should follow the existing project architecture:

* The UMD is primarily a **state tracker + command encoder**:
  * consume DDI calls
  * validate/normalize state
  * emit an **AeroGPU-specific command stream** (IR) suitable for execution by the emulator
* The KMD is primarily **submission + memory bookkeeping plumbing** (WDDM 1.1):
  * accept DMA buffers / submission packets from the runtime
  * provide a stable fence + interrupt completion path (avoid TDRs)
  * maintain the “allocation index → guest physical pages” mapping described in `win7-wddm11-aerogpu-driver.md`

Practical implication for D3D10/11 bring-up: whenever this doc says “flush/submit”, the concrete implementation should enqueue a bounded unit of work to the emulator and ensure the WDDM-visible fence monotonically advances.

---

## 2) Minimum D3D10DDI + D3D11DDI entrypoints (Win7 bring-up set)

This section enumerates the **minimum practical** entrypoints to get:

* device creation
* resource creation
* shader creation/binding
* pipeline state
* basic draws
* swapchain-backed present

It also marks which entrypoints can initially return **NOT_SUPPORTED** and what that implies for feature levels / capabilities.

### 2.1 D3D10: adapter + device entrypoints (D3D10DDI)

#### 2.1.1 Mandatory exports / adapter functions

* Export: `OpenAdapter10` (from `d3d10umddi.h`)
* Adapter function table (`D3D10DDI_ADAPTERFUNCS`) must minimally provide:
  * `pfnGetCaps` → handles `D3D10DDIARG_GETCAPS`
  * `pfnCalcPrivateDeviceSize`
  * `pfnCreateDevice` → fills `D3D10DDIARG_CREATEDEVICE` and returns `D3D10DDI_DEVICEFUNCS`
  * `pfnCloseAdapter`

**Initially NOT_SUPPORTED (safe to stub):**

* D3D10.1 specific negotiation (skip `OpenAdapter10_2` / `d3d10_1umddi.h` initially)

#### 2.1.2 Mandatory device/object creation (private-size + create + destroy)

To render anything, implement the “calc/create/destroy” triads for:

Resources
* `pfnCalcPrivateResourceSize` + `pfnCreateResource` + `pfnDestroyResource`
  * struct: `D3D10DDIARG_CREATERESOURCE`
  * note: destroy is typically `pfnDestroyResource(D3D10DDI_HDEVICE, D3D10DDI_HRESOURCE)` (no `*_ARG_DESTROY*` structure)

Views
* `pfnCalcPrivateShaderResourceViewSize` + `pfnCreateShaderResourceView` + `pfnDestroyShaderResourceView`
  * `D3D10DDIARG_CREATESHADERRESOURCEVIEW`
* `pfnCalcPrivateRenderTargetViewSize` + `pfnCreateRenderTargetView` + `pfnDestroyRenderTargetView`
  * `D3D10DDIARG_CREATERENDERTARGETVIEW`
* `pfnCalcPrivateDepthStencilViewSize` + `pfnCreateDepthStencilView` + `pfnDestroyDepthStencilView`
  * `D3D10DDIARG_CREATEDEPTHSTENCILVIEW`

Shaders
* `pfnCalcPrivateVertexShaderSize` + `pfnCreateVertexShader` + `pfnDestroyVertexShader`
  * `D3D10DDIARG_CREATEVERTEXSHADER`
* `pfnCalcPrivatePixelShaderSize` + `pfnCreatePixelShader` + `pfnDestroyPixelShader`
  * `D3D10DDIARG_CREATEPIXELSHADER`

Pipeline state
* `pfnCalcPrivateElementLayoutSize` + `pfnCreateElementLayout` + `pfnDestroyElementLayout`
  * `D3D10DDIARG_CREATEELEMENTLAYOUT`
* `pfnCalcPrivateSamplerSize` + `pfnCreateSampler` + `pfnDestroySampler`
  * `D3D10DDIARG_CREATESAMPLER`
* `pfnCalcPrivateRasterizerStateSize` + `pfnCreateRasterizerState` + `pfnDestroyRasterizerState`
  * `D3D10DDIARG_CREATERASTERIZERSTATE`
* `pfnCalcPrivateBlendStateSize` + `pfnCreateBlendState` + `pfnDestroyBlendState`
  * `D3D10DDIARG_CREATEBLENDSTATE`
* `pfnCalcPrivateDepthStencilStateSize` + `pfnCreateDepthStencilState` + `pfnDestroyDepthStencilState`
  * `D3D10DDIARG_CREATEDEPTHSTENCILSTATE`

**Initially NOT_SUPPORTED (safe to stub):**

* Geometry shader object creation:
  * `pfnCalcPrivateGeometryShaderSize` + `pfnCreateGeometryShader` + `pfnDestroyGeometryShader`
  * note: the **GS stage exists in D3D10**, but many “first triangle” tests never create/bind a GS (it is valid to have no GS bound). Deferring GS support is acceptable for bring-up but will break apps that compile/use GS.
* Stream-output state / SO buffers (`pfnSoSetTargets`, etc)
* Queries/predication:
  * `pfnCreateQuery` / `pfnDestroyQuery`, `pfnBegin` / `pfnEnd`, `pfnSetPredication`

#### 2.1.3 Mandatory context/state binding + draw path

Minimal pipeline binding (D3D10DDI_DEVICEFUNCS):

Input Assembler
* `pfnIaSetInputLayout`
* `pfnIaSetVertexBuffers`
* `pfnIaSetIndexBuffer`
* `pfnIaSetPrimitiveTopology`

Shaders
* `pfnVsSetShader`
* `pfnPsSetShader`
* `pfnVsSetConstantBuffers`
* `pfnPsSetConstantBuffers`
* `pfnVsSetShaderResources` / `pfnPsSetShaderResources` (for texture test)
* `pfnVsSetSamplers` / `pfnPsSetSamplers` (for texture test)

Rasterizer / Output merger
* `pfnSetViewports`
* `pfnSetScissorRects` (can be a no-op if you clamp to viewport initially)
* `pfnSetRasterizerState`
* `pfnSetBlendState`
* `pfnSetDepthStencilState`
* `pfnSetRenderTargets` (RTVs + DSV)

Clears and draws
* `pfnClearRenderTargetView`
* `pfnClearDepthStencilView` (needed for depth test app)
* `pfnDraw`
* `pfnDrawIndexed` (many samples use indexed draws)

Presentation / swapchain integration
* `pfnPresent` (DXGI ultimately drives this from `IDXGISwapChain::Present`)
  * structs: `D3D10DDIARG_PRESENT` (and the corresponding D3D11 variant, if exposed by your D3D11 DDI version)
* `pfnRotateResourceIdentities`
  * used by DXGI swapchains to rotate backbuffer “resource identities” after present without requiring a full copy

Resource update/copy (minimum)
* `pfnMap` + `pfnUnmap` (dynamic VB/IB/CB uploads) — `D3D10DDIARG_MAP`
* `pfnUpdateSubresource` (some apps prefer this over map/unmap)
* `pfnCopyResource` / `pfnCopySubresourceRegion` (optional but commonly used internally by runtimes)

Command submission
* `pfnFlush` (or equivalent submit/flush entrypoint in the DDI) to ensure GPU work reaches the KMD/host.

### 2.2 D3D11: adapter + device/context entrypoints (D3D11DDI)

#### 2.2.1 Mandatory exports / adapter functions

* Export: `OpenAdapter11` (from `d3d11umddi.h`)
* Adapter function table (`D3D11DDI_ADAPTERFUNCS`) must minimally provide:
  * `pfnGetCaps` → handles `D3D11DDIARG_GETCAPS`
    * must report supported `D3D_FEATURE_LEVEL` list (initial: **`D3D_FEATURE_LEVEL_10_0` only**)
  * `pfnCalcPrivateDeviceSize`
  * `pfnCreateDevice` → uses `D3D11DDIARG_CREATEDEVICE`
    * `D3D11DDIARG_CREATEDEVICE` is where the driver returns both:
      * `D3D11DDI_DEVICEFUNCS` (device/object creation)
      * `D3D11DDI_DEVICECONTEXTFUNCS` (immediate context draw/state/update entrypoints)
  * `pfnCloseAdapter`

#### 2.2.2 Mandatory device/object creation

The D3D11 DDI is structurally similar to D3D10, with additional shader stages and optional view types.

**Important D3D11 DDI split (device vs immediate context):**

* Object creation/destruction lives on the **device function table** (`D3D11DDI_DEVICEFUNCS`) and is typically called with a `D3D11DDI_HDEVICE`.
* Most draw/clear/update/state-binding calls live on the **immediate context function table** (`D3D11DDI_DEVICECONTEXTFUNCS`) and are called with a `D3D11DDI_HDEVICECONTEXT`.

When implementing the Win7 D3D11 UMD, treat “device” and “context” as separate state holders even if your backend is single-threaded; it avoids conflating lifetime (device objects) with per-command-stream state (bindings and draws).

Resources
* `pfnCalcPrivateResourceSize` + `pfnCreateResource` + `pfnDestroyResource`
  * `D3D11DDIARG_CREATERESOURCE`

Views
* `pfnCalcPrivateShaderResourceViewSize` + `pfnCreateShaderResourceView` + `pfnDestroyShaderResourceView`
  * `D3D11DDIARG_CREATESHADERRESOURCEVIEW`
* `pfnCalcPrivateRenderTargetViewSize` + `pfnCreateRenderTargetView` + `pfnDestroyRenderTargetView`
  * `D3D11DDIARG_CREATERENDERTARGETVIEW`
* `pfnCalcPrivateDepthStencilViewSize` + `pfnCreateDepthStencilView` + `pfnDestroyDepthStencilView`
  * `D3D11DDIARG_CREATEDEPTHSTENCILVIEW`

Shaders (initial bring-up)
* `pfnCalcPrivateVertexShaderSize` + `pfnCreateVertexShader` + `pfnDestroyVertexShader`
  * `D3D11DDIARG_CREATEVERTEXSHADER`
* `pfnCalcPrivatePixelShaderSize` + `pfnCreatePixelShader` + `pfnDestroyPixelShader`
  * `D3D11DDIARG_CREATEPIXELSHADER`

Pipeline state
* `pfnCalcPrivateElementLayoutSize` + `pfnCreateElementLayout` + `pfnDestroyElementLayout`
  * `D3D11DDIARG_CREATEELEMENTLAYOUT`
* `pfnCalcPrivateSamplerSize` + `pfnCreateSampler` + `pfnDestroySampler`
  * `D3D11DDIARG_CREATESAMPLER`
* `pfnCalcPrivateRasterizerStateSize` + `pfnCreateRasterizerState` + `pfnDestroyRasterizerState`
  * `D3D11DDIARG_CREATERASTERIZERSTATE`
* `pfnCalcPrivateBlendStateSize` + `pfnCreateBlendState` + `pfnDestroyBlendState`
  * `D3D11DDIARG_CREATEBLENDSTATE`
* `pfnCalcPrivateDepthStencilStateSize` + `pfnCreateDepthStencilState` + `pfnDestroyDepthStencilState`
  * `D3D11DDIARG_CREATEDEPTHSTENCILSTATE`

**Initially NOT_SUPPORTED (recommended):**

These can return `E_NOTIMPL` / set error until the driver claims a higher feature level (or otherwise advertises the corresponding capability as unsupported).

* Tessellation stages (requires FL11_0):
  * `pfnCreateHullShader` / `D3D11DDIARG_CREATEHULLSHADER`
  * `pfnCreateDomainShader` / `D3D11DDIARG_CREATEDOMAINSHADER`
  * `pfnHsSetShader`, `pfnDsSetShader`, and related CB/SRV/sampler bind calls
* Compute shader stage (roadmap item):
  * `pfnCreateComputeShader` / `D3D11DDIARG_CREATECOMPUTESHADER`
  * `pfnCsSetShader`, UAV binding, dispatch calls
* UAVs:
  * `pfnCalcPrivateUnorderedAccessViewSize` / `pfnCreateUnorderedAccessView` / `pfnDestroyUnorderedAccessView`
  * `D3D11DDIARG_CREATEUNORDEREDACCESSVIEW`

Geometry shader note:

* Geometry shaders are part of the D3D10-class pipeline and are expected at `D3D_FEATURE_LEVEL_10_0` and above.
* If you advertise **FL10_0** (or higher) from `pfnGetCaps`, implement `pfnCreateGeometryShader` / `D3D11DDIARG_CREATEGEOMETRYSHADER` (and the corresponding bind/state entrypoints) even if the first implementation is “limited but functional”.
* If you are not ready to support GS yet, prefer advertising only `D3D_FEATURE_LEVEL_9_x` for D3D11 (while still supporting D3D10 separately), or be explicit that some FL10_0 apps will fail when they create/bind GS.

#### 2.2.3 Mandatory context/state binding + draw path

At FL10_0, D3D11 essentially needs the D3D10-era pipeline:

Immediate context function table: `D3D11DDI_DEVICECONTEXTFUNCS`

Input Assembler
* `pfnIaSetInputLayout`
* `pfnIaSetVertexBuffers`
* `pfnIaSetIndexBuffer`
* `pfnIaSetPrimitiveTopology`

Shaders + resource binding (VS/PS only)
* `pfnVsSetShader`, `pfnPsSetShader`
* `pfnVsSetConstantBuffers`, `pfnPsSetConstantBuffers`
* `pfnVsSetShaderResources`, `pfnPsSetShaderResources`
* `pfnVsSetSamplers`, `pfnPsSetSamplers`

Rasterizer / Output merger
* `pfnSetViewports`, `pfnSetScissorRects`
* `pfnSetRasterizerState`
* `pfnSetBlendState`
* `pfnSetDepthStencilState`
* `pfnSetRenderTargets` (or the D3D11 DDI equivalent of OMSetRenderTargets)

Clears and draws
* `pfnClearRenderTargetView`
* `pfnClearDepthStencilView`
* `pfnDraw`, `pfnDrawIndexed`

Resource updates
* `pfnMap` + `pfnUnmap` — `D3D11DDIARG_MAP`
* `pfnUpdateSubresource`
* `pfnCopyResource` / `pfnCopySubresourceRegion`

---

## 3) Swapchain + Present path (DXGI 1.1 expectations)

### 3.1 Minimum DXGI swapchain behavior to target first

For Windows 7, a minimal implementation should accept (and test against) swapchains created with:

* `DXGI_SWAP_CHAIN_DESC::Windowed = TRUE`
* `DXGI_SWAP_CHAIN_DESC::SwapEffect = DXGI_SWAP_EFFECT_DISCARD`
* `DXGI_SWAP_CHAIN_DESC::BufferCount = 1` (most common on Win7 for DISCARD)
* `DXGI_SWAP_CHAIN_DESC::SampleDesc.Count = 1` (no MSAA initially)
* Common formats:
  * `DXGI_FORMAT_B8G8R8A8_UNORM` (very common for DWM)
  * `DXGI_FORMAT_R8G8B8A8_UNORM`
* `DXGI_USAGE_RENDER_TARGET_OUTPUT` (plus optionally `DXGI_USAGE_SHADER_INPUT`)

**Initially NOT_SUPPORTED:**

* `IDXGISwapChain::SetFullscreenState(TRUE, ...)` → return `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`
* `DXGI_SWAP_EFFECT_SEQUENTIAL` (can be implemented later, but DISCARD first is enough)
* MSAA swapchains (`SampleDesc.Count > 1`)

### 3.2 Present semantics to match common apps

Apps will call `IDXGISwapChain::Present(SyncInterval, Flags)`:

* `SyncInterval = 0` (immediate) and `SyncInterval = 1` (vsync) are the most common.
* The driver must not crash on other values; clamp to 1 initially.
* `Flags` to handle:
  * `0` (normal)
  * `DXGI_PRESENT_TEST`: do not present; only validate (return `S_OK` if it *would* succeed)
  * `DXGI_PRESENT_DO_NOT_WAIT`: if you can’t queue immediately, return `DXGI_ERROR_WAS_STILL_DRAWING`

On Win7 **windowed** swapchains, Present is effectively “make the backbuffer visible to DWM”; the minimal path for AeroGPU is:

1. Ensure all pending rendering to the backbuffer is flushed/submitted (`pfnFlush` or equivalent).
2. Signal the host/emulator with the presented resource ID and dirty rectangle (if any).
3. Host composites to the browser canvas.

Implementation note: in practice DXGI will often call into the UMD’s D3D10/11 DDI device functions for present and (for multi-buffer scenarios) buffer rotation:

* `pfnPresent` (with `D3D10DDIARG_PRESENT` or equivalent)
* `pfnRotateResourceIdentities` (rotate swapchain backbuffer resources)

### 3.3 ResizeBuffers / ResizeTarget expectations

Apps commonly call:

* `IDXGISwapChain::ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, Flags)` to “resize to window”
* `IDXGISwapChain::ResizeTarget(&DXGI_MODE_DESC)` sometimes, but can be a no-op for windowed-only

Minimum rules:

* Old backbuffer resources must become invalid for rendering once resized.
* Any views created on old buffers must be destroyed/recreated by the runtime/app; driver should handle destruction cleanly.
* If the runtime creates the new buffers via the DXGI DDI (`dxgiddi.h`), ensure resource IDs change and the host knows the new size.

---

## 4) Resource binding model (CBs, SRVs/UAVs, samplers, RTV/DSV)

### 4.1 What the D3D10/11 runtime expects

The runtime sets pipeline bindings by stage, using arrays of handles and slot ranges. The driver must implement “overwrite slots” semantics:

* A bind call updates `[StartSlot, StartSlot + Num* )`.
* `NULL` handles in the array **unbind** that slot.
* Bindings persist until overwritten.

### 4.2 Minimal state to track in the UMD

For an initial implementation, track only what basic rendering needs:

Input Assembler
* `ElementLayout` (input layout)
* Vertex buffers: handles + `(Stride, Offset)` per slot
* Index buffer: handle + format + offset
* Primitive topology

Shader stages (minimum VS/PS)
* Current VS/PS objects (store DXBC hash/ID)
* Constant buffer bindings (resources) per stage
* SRV bindings per stage
* Sampler bindings per stage

Output merger / rasterizer
* RTV array (up to `D3D11_SIMULTANEOUS_RENDER_TARGET_COUNT`, usually 8)
* One DSV
* Blend state + blend factor + sample mask
* Depth/stencil state + stencil ref
* Rasterizer state
* Viewports + scissor rects

### 4.3 UAVs (defer until FL11_0)

UAVs only become required once you support compute or advanced pixel pipeline features.

**Win7 bring-up recommendation:**

* Implement RTV/DSV/SRV first.
* Return NOT_SUPPORTED for `CreateUnorderedAccessView` and any `*SetUnorderedAccessViews` style bindings until you claim FL11_0.

### 4.4 Hazard rules (minimal correctness)

D3D10/11 disallow (or define undefined behavior for) some simultaneous bindings, notably:

* A resource bound as an RTV/DSV cannot be simultaneously bound as an SRV in the same pipeline where it would be read.

Minimum viable driver behavior:

* When binding an RTV/DSV, **auto-unbind** that resource from SRV slots (VS/PS) to avoid feedback loops.
* When binding an SRV, auto-unbind it from RTV slots if already bound.

This matches the “helpful driver” behavior many apps implicitly rely on, and avoids host-side validation errors (e.g. WebGPU).

---

## 5) Shader handling (DXBC → translator) and reflection needs

### 5.1 DXBC handling in the DDI

Shader creation entrypoints provide DXBC bytecode through the `...ARG_CREATE*SHADER` structures:

* D3D10: `D3D10DDIARG_CREATEVERTEXSHADER`, `D3D10DDIARG_CREATEPIXELSHADER` (and `...CREATEGEOMETRYSHADER` if supported)
* D3D11: `D3D11DDIARG_CREATEVERTEXSHADER`, `D3D11DDIARG_CREATEPIXELSHADER`, etc.

Rules:

* Treat incoming pointers as read-only and short-lived; copy the DXBC blob into driver-owned memory (or immediately hash it and forward to host).
* Cache translation results by **(shader model, DXBC hash)**; shaders are frequently recreated across device loss/reset paths.

### 5.2 Forwarding strategy to the AeroGPU emulator translator

Minimal architecture that works well with “UMD in guest ↔ WebGPU in host”:

1. On `Create*Shader`, compute a content hash of the DXBC, store a small private object:
   * `{ shader_stage, dxbc_hash, bytecode_length, optional_cached_host_id }`
2. Send `{ dxbc_hash, stage, dxbc_bytes }` to the host translator once (or lazily on first bind).
3. Host translates DXBC → internal IR → WGSL (or other), creates a WebGPU shader module/pipeline key.
4. On draw, UMD sends the bound `dxbc_hash` (or host shader ID) plus current pipeline state.

### 5.3 Minimal “reflection” requirements

For a minimal driver, do **not** depend on D3DCompiler reflection APIs at runtime. Instead:

* **Input layout validation:** needed to map `D3D10DDIARG_CREATEELEMENTLAYOUT` / `D3D11DDIARG_CREATEELEMENTLAYOUT` to what the vertex shader expects.
  * Option A (minimal): do not validate; accept the layout and rely on app correctness.
  * Option B (recommended): parse DXBC `ISGN` (input signature) chunk to validate semantics/types.

* **Resource bindings:** the runtime binds SRVs/CBs/samplers by slot; your translator needs to know which slots are referenced.
  * Minimal: infer referenced slots by scanning DXBC instructions for resource operands.
  * Recommended: parse the DXBC `RDEF` chunk (resource definitions) to learn declared bindings (t/s/b registers) and their dimensions.

No other reflection is required for “triangle/texture/depth” bring-up.

---

## 6) Compatibility target (feature level) and roadmap

### 6.1 Minimal feature level to claim first: **FL10_0**

**Why FL10_0 first:**

* D3D10 runtime requires a D3D10-capable driver (effectively 10_0/10_1 class).
* D3D11 apps often ship with a **10_0 fallback** path on Windows 7-era GPUs.
* FL10_0 avoids tessellation requirements and keeps the pipeline close to D3D10 (VS/GS/PS only, SM4.0).

**What you must support for a credible FL10_0 path:**

* Render-to-texture (RTV) + depth (DSV)
* `Draw` and `DrawIndexed`
* Viewports/scissors, rasterizer state, blend state, depth/stencil state
* Texture2D sampling + samplers
* Constant buffers (as resources) and updates (Map/Unmap or UpdateSubresource)
* Geometry shader plumbing (required by the FL10_0 pipeline even if your initial apps never bind a GS):
  * `pfnCreateGeometryShader` and the corresponding `*SetShader`/resource-binding entrypoints
  * it is valid for apps to keep the GS stage unbound; but the entrypoints should exist and work when used

**What can be NOT_SUPPORTED at FL10_0 bring-up (but will limit apps):**

* Queries/predication
* MSAA
* UAVs and compute
* Deferred contexts / command lists (if the runtime exposes them through your chosen D3D11 DDI interface version)

If you claim `D3D_FEATURE_LEVEL_10_0` but do not implement compute shaders, ensure the corresponding capability is reported as unsupported (for API-facing caps this is typically via `D3D11_FEATURE_DATA_D3D10_X_HARDWARE_OPTIONS::ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x = FALSE`).

### 6.2 Roadmap to FL11_0 (SM5.0)

To claim `D3D_FEATURE_LEVEL_11_0`, plan these increments:

1. **Geometry shader support** (if not already)
   * implement `pfnCreateGeometryShader` + bind calls
2. **UAV plumbing + compute shaders**
   * `pfnCreateUnorderedAccessView` / `D3D11DDIARG_CREATEUNORDEREDACCESSVIEW`
   * `pfnCreateComputeShader` / `D3D11DDIARG_CREATECOMPUTESHADER`
   * UAV binding APIs and `Dispatch`
3. **Tessellation**
   * `pfnCreateHullShader` / `pfnCreateDomainShader`
   * HS/DS set calls + fixed-function tessellation state
4. **More formats and caps**
   * BC6H/BC7 (common in later D3D11 titles)
   * more `pfnGetCaps` coverage (format support, threading caps, etc)

Keep `pfnGetCaps` truthful: only advertise a feature level once the corresponding shader stages and bindings are implemented end-to-end.

---

## 7) Testing plan (minimal apps and expected coverage)

The goal is to validate the UMD is functional *without* needing a full game engine.

For system-level smoke testing (separate from these app-level tests), use the validation checklist in `docs/graphics/win7-aerogpu-validation.md` (TDR/vblank stability, `dxdiag` checks, etc).

This repository also contains a guest-side test harness you can use as a starting point:

* `drivers/aerogpu/tests/win7/` (see `drivers/aerogpu/tests/win7/README.md`)
  * includes `d3d11_triangle` and `readback_sanity` tests that already exercise a large chunk of the Win7 DXGI/D3D11 path.

### 7.1 D3D10 triangle (windowed)

**Covers:**

* `OpenAdapter10` → `CreateDevice`
* swapchain creation (`DXGI_SWAP_CHAIN_DESC`)
* `CreateResource` (VB), `CreateRenderTargetView`
* `CreateVertexShader` / `CreatePixelShader` (SM4.0)
* `IASet*`, `VSSetShader`, `PSSetShader`, `OMSetRenderTargets`
* `Draw`, `Present`

### 7.2 D3D11 triangle (device at FL10_0)

**Covers:**

* `OpenAdapter11` → `CreateDevice`
* D3D11 binding path (VS/PS only)
* same draw/present path as above, but via D3D11 runtime/DDI

**Existing in repo:** `drivers/aerogpu/tests/win7/d3d11_triangle/` (builds shaders with `fxc.exe` and validates pixels via staging readback).

### 7.3 Texture sampling test (2D)

Render a textured quad:

**Covers:**

* `CreateResource` (Texture2D)
* `CreateShaderResourceView`, `CreateSampler`
* texture upload path (`Map/Unmap` or `UpdateSubresource`)
* `PSSetShaderResources`, `PSSetSamplers`
* shader translation handling `sample` ops

### 7.4 Depth test

Draw two overlapping triangles with different Z and enable depth:

**Covers:**

* `CreateResource` (depth buffer)
* `CreateDepthStencilView`
* `CreateDepthStencilState` + `SetDepthStencilState`
* `ClearDepthStencilView`
* correct depth compare/write behavior in the host translation layer

---

### Appendix: practical bring-up order

1. Implement D3D11 DDI at FL10_0 first (many samples use D3D11 even when targeting “DX10 class”).
2. Reuse the same underlying object model to implement D3D10 DDI entrypoints.
3. Only after triangle/texture/depth pass, start expanding caps and feature level.
