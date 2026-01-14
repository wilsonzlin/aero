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
* `docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md` — deprecated redirect (kept for link compatibility; points at the focused allocation + Map/Unmap docs).
* `docs/graphics/win7-d3d10-11-umd-allocations.md` — CreateResource-side allocation contract details (allocation-info arrays, `pfnAllocateCb`/`pfnDeallocateCb`, and `DXGI_DDI_PRIMARY_DESC` primaries/backbuffers).
* `docs/graphics/win7-dxgi-swapchain-backbuffer.md` — trace guide + invariants for Win7 DXGI swapchain backbuffer `CreateResource` parameters and allocation flags.
* `docs/graphics/win7-d3d11-map-unmap.md` — Win7 D3D11 `Map`/`Unmap` contract (`pfnLockCb`/`pfnUnlockCb`, DO_NOT_WAIT, staging readback sync).
* `docs/graphics/win7-d3d11ddi-function-tables.md` — D3D11 `d3d11umddi.h` function-table checklist (which entries must be non-null vs safely stubbed for FL10_0 bring-up).
* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md` — Win7 WDK symbol-name reference for D3D10/11 UMD callbacks (submission, fences, `SetErrorCb`, WOW64 gotchas).
* `docs/graphics/win7-d3d10-caps-tracing.md` — how to enable `GetCaps` + entrypoint tracing in the D3D10/11 UMD during Win7 bring-up.
* `docs/graphics/win7-shared-surfaces-share-token.md` — Win7 shared-surface strategy (stable cross-process `share_token` vs user-mode shared `HANDLE` numeric values).

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
* interface version negotiation (e.g. `D3D10DDI_INTERFACE_VERSION`, `D3D11DDI_INTERFACE_VERSION` / `D3D11DDI_SUPPORTED`)

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

For the Win7/WDDM 1.1 **exact** callback/table names involved (`D3D*DDIARG_CREATEDEVICE::pCallbacks->pfnSetErrorCb`, and the related submission + fence wait callbacks in `d3dumddi.h`), see:

* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

For DDI functions that *do* return `HRESULT`, return:

* `S_OK` on success
* `E_OUTOFMEMORY`, `E_INVALIDARG`, or `E_NOTIMPL` as appropriate for unsupported features
  * AeroGPU note: if you see `E_OUTOFMEMORY` “too early” (while the guest still has free RAM), you may be hitting Win7’s WDDM segment budget rather than true exhaustion. AeroGPU is system-memory-backed, but dxgkrnl still enforces the KMD-reported non-local segment size; tune `HKR\Parameters\NonLocalMemorySizeMB` (see `docs/graphics/win7-aerogpu-validation.md` appendix and `drivers/aerogpu/kmd/README.md`).

### 1.5 AeroGPU-specific implementation layering (UMD → KMD → emulator)

This doc focuses on the *API contract* (D3D10/11 DDI) that the Microsoft runtimes will call. The implementation behind those entrypoints in AeroGPU should follow the existing project architecture:

* The UMD is primarily a **state tracker + command encoder**:
  * consume DDI calls
  * validate/normalize state
  * emit an **AeroGPU-specific command stream** (IR) suitable for execution by the emulator
* The KMD is primarily **submission + memory bookkeeping plumbing** (WDDM 1.1):
  * accept DMA buffers / submission packets from the runtime
  * provide a stable fence + interrupt completion path (avoid TDRs)
  * build a per-submission allocation table keyed by stable `alloc_id` values (see `drivers/aerogpu/protocol/aerogpu_ring.h`) and provide it via the submit descriptor, so command packets can reference guest-backed memory via `backing_alloc_id` (ABI details: `drivers/aerogpu/protocol/README.md`)

Practical implication for D3D10/11 bring-up: whenever this doc says “flush/submit”, the concrete implementation should enqueue a bounded unit of work to the emulator and ensure the WDDM-visible fence monotonically advances.

### 1.6 AeroGPU device discovery (UMDRIVERPRIVATE)

UMDs must not assume optional features like vblank timing and fence pages exist, or a specific AeroGPU BAR0 ABI (legacy `"ARGP"` vs versioned `"AGPU"`; legacy is optional and feature-gated behind `emulator/aerogpu-legacy`).

During adapter open, query:

* `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)`

 and decode the returned `aerogpu_umd_private_v1` (see `drivers/aerogpu/protocol/aerogpu_umd_private.h`). Use the reported feature bits to gate optional runtime behavior (e.g. vblank-paced present paths).

When validating the blob, require `struct_version == 1` and `size_bytes >= sizeof(aerogpu_umd_private_v1)` (not an exact match), so v1-compatible extensions that append trailing bytes remain usable.
  
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
  * note: the **GS stage exists in D3D10**, but many “first triangle” tests never create/bind a GS (it is valid to have no GS bound).
  * AeroGPU note: WebGPU has no geometry shader stage. The command stream can encode a GS handle via
    `BIND_SHADERS` (legacy: `aerogpu_cmd_bind_shaders::reserved0`; newer streams may append `{gs,hs,ds}`
    after the stable 24-byte prefix). When appended handles are present they are authoritative; producers
    may optionally mirror `gs` into `reserved0` for best-effort compatibility with legacy hosts.
    GS DXBC is forwarded to the host. The host has compute-prepass plumbing for GS/HS/DS emulation; a minimal translator-backed GS prepass is executed for `PointList`, `LineList`, and `TriangleList` draws (`Draw` and `DrawIndexed`) when supported, but broader GS DXBC execution is still bring-up work.
    See [`geometry-shader-emulation.md`](./geometry-shader-emulation.md) and [`docs/16-d3d10-11-translation.md`](../16-d3d10-11-translation.md).
* Stream-output state / SO buffers (`pfnSoSetTargets`, etc)
* Queries/predication:
  * `pfnCreateQuery` / `pfnDestroyQuery`, `pfnBegin` / `pfnEnd`, `pfnSetPredication`

#### 2.1.3 Mandatory context/state binding + draw path

Minimal pipeline binding (D3D10DDI_DEVICEFUNCS):

Input Assembler
* `pfnIaSetInputLayout`
* `pfnIaSetVertexBuffers`
* `pfnIaSetIndexBuffer`
* `pfnIaSetTopology` (sets `D3D*_PRIMITIVE_TOPOLOGY`; corresponds to `IASetPrimitiveTopology` at the API level)

Shaders
* `pfnVsSetShader`
* `pfnPsSetShader`
* `pfnVsSetConstantBuffers`
* `pfnPsSetConstantBuffers`
* `pfnVsSetShaderResources` / `pfnPsSetShaderResources` (for texture test)
* `pfnVsSetSamplers` / `pfnPsSetSamplers` (for texture test)

Rasterizer / Output merger
* `pfnSetViewports`
* `pfnSetScissorRects` (can be ignored initially if you always clamp to the viewport)
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
  * struct: `D3D10DDIARG_PRESENT` (used by DXGI for both D3D10 and D3D11 devices on Win7)
* `pfnRotateResourceIdentities`
  * used by DXGI swapchains to rotate backbuffer “resource identities” after present without requiring a full copy

Resource update/copy (minimum)
* `pfnMap` + `pfnUnmap` (dynamic VB/IB/CB uploads) — `D3D10DDIARG_MAP`
* `pfnUpdateSubresourceUP` (user-memory upload path; some apps prefer this over map/unmap)
  * struct: `D3D10DDIARG_UPDATESUBRESOURCEUP`
* `pfnCopyResource` / `pfnCopySubresourceRegion` (optional but commonly used internally by runtimes)

See also:

* `docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md` — deprecated redirect (kept for link compatibility; points at the focused docs below).
* `docs/graphics/win7-d3d10-11-umd-allocations.md` — Win7/WDDM 1.1 resource allocation (`CreateResource` → `pfnAllocateCb`) contract.
* `docs/graphics/win7-d3d11-map-unmap.md` — Win7 `Map`/`Unmap` semantics (`LockCb`/`UnlockCb`) for dynamic uploads + staging readback.

Command submission
* `pfnFlush` (or equivalent submit/flush entrypoint in the DDI) to ensure GPU work reaches the KMD/host.

#### 2.1.4 AeroGPU allocation-backed resources (alloc_id semantics + dirty ranges)

For AeroGPU’s command stream, D3D10/11 resources (buffers, textures) are expected to be **backed by real WDDM allocations** so the emulator/host can:

* read CPU-written contents directly from guest memory (uploads), and
* write GPU results back into guest allocations (staging readback / correctness).

**Key decision:** the AeroGPU allocation table `alloc_id` (and `backing_alloc_id` in `AEROGPU_CMD_CREATE_*`) is a **stable driver-defined `u32` ID**, not a per-submit index and not an OS handle value.

* `backing_alloc_id == alloc_id` (stable `u32`; `0` means “host allocated”).
* On Win7/WDDM 1.1, the UMD provides this ID to the KMD via the **allocation private driver data blob** (`aerogpu_wddm_alloc_priv.alloc_id` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
  * This is required because the numeric value of the UMD-visible allocation handle (`D3DKMT_HANDLE` from `pfnAllocateCb`) is **not** the same identity the KMD later sees in `DXGK_ALLOCATIONLIST`.
* On every submission, the UMD must provide a `D3DDDI_ALLOCATIONLIST` containing the referenced `hAllocation` handles so the KMD can read the private driver data and build the per-submit allocation table.
* The KMD then emits a per-submit allocation table mapping `alloc_id → {gpa, size}`; the host resolves guest memory by `alloc_id`, not by allocation-list position.

See also:

* `docs/graphics/aerogpu-backing-alloc-id.md` — authoritative `backing_alloc_id` / `alloc_id` semantics and host-side resolution rules.

**Dirty range notifications (MVP):**

* For resources backed by guest memory (`backing_alloc_id != 0`), any `Map` that permits CPU writing (`WRITE`, `WRITE_DISCARD`, `WRITE_NO_OVERWRITE`) must emit `AEROGPU_CMD_RESOURCE_DIRTY_RANGE` on `Unmap`.
  * For host-owned resources (`backing_alloc_id == 0`, e.g. dynamic buffers in the bring-up path), the UMD must upload bytes explicitly via `AEROGPU_CMD_UPLOAD_RESOURCE` instead of relying on dirty ranges.
* For MVP, mark the entire allocation dirty:
  * `offset_bytes = 0`
  * `size_bytes = allocation_size`
* If `CreateResource` copies initial data into the allocation, emit one `RESOURCE_DIRTY_RANGE` after the upload.

**Staging readback / `CopyResource` (MVP):**

* If the UMD uses a staging resource backed by guest memory (`backing_alloc_id != 0`) for `Map(READ)`-style readback, the `CopyResource`/`CopySubresourceRegion` path must emit:
  * `AEROGPU_CMD_COPY_BUFFER` / `AEROGPU_CMD_COPY_TEXTURE2D` with `AEROGPU_COPY_FLAG_WRITEBACK_DST`, so the host writes the copied bytes back into guest memory before signaling the fence.
* The submission must include an allocation-table entry for the destination resource’s `backing_alloc_id` (Win7: include the WDDM allocation handle in the submit allocation list).
  * The destination allocation must be marked writable for the submission (`WriteOperation` bit set in the WDDM allocation list); otherwise the KMD will mark it `AEROGPU_ALLOC_FLAG_READONLY` and the host will reject the writeback.

### 2.2 D3D11: adapter + device/context entrypoints (D3D11DDI)

For a **table-by-table** checklist of which `d3d11umddi.h` function pointers must be non-null vs safely stubbable for a crash-free Win7 bring-up (FL10_0), see:
* `docs/graphics/win7-d3d11ddi-function-tables.md`

#### 2.2.1 Mandatory exports / adapter functions

* Export: `OpenAdapter11` (from `d3d11umddi.h`)
* Adapter function table (`D3D11DDI_ADAPTERFUNCS`) must minimally provide:
  * `pfnGetCaps` → handles `D3D11DDIARG_GETCAPS`
    * must report supported `D3D_FEATURE_LEVEL` list.
      * Minimal target: **`D3D_FEATURE_LEVEL_10_0` only**.
      * If you are intentionally gating out geometry shaders (e.g. host backend without compute pipelines), advertise only `D3D_FEATURE_LEVEL_9_x` until GS emulation is supported.
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
* Geometry shader (required for `D3D_FEATURE_LEVEL_10_0` and above; can be deferred only if you advertise `D3D_FEATURE_LEVEL_9_x`)
  * `pfnCalcPrivateGeometryShaderSize` + `pfnCreateGeometryShader` + `pfnDestroyGeometryShader`
  * `D3D11DDIARG_CREATEGEOMETRYSHADER`

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
  * **Outdated MVP note:** earlier bring-up guidance suggested “accept GS creation but ignore it”. This is no longer viable once you run real GS workloads and the Win7 regression tests below.
  * **AeroGPU approach (target): forward GS DXBC and emulate on the host (WebGPU).**
    * The guest UMD treats GS like VS/PS: it forwards the DXBC blob to the host and participates in normal shader lifetime + binding.
    * Since WebGPU has **no GS stage**, AeroGPU emulates GS by inserting a **compute prepass** when a GS is bound:
      - The executor already routes draws with a bound GS through a compute-prepass + indirect-draw path.
      - The in-tree GS DXBC→WGSL compute translator exists and is partially integrated: translation is attempted at `CREATE_SHADER_DXBC`, and `PointList`, `LineList`, and `TriangleList` draws (`Draw` and `DrawIndexed`) can execute the translated compute prepass (minimal SM4 subset). If GS translation fails, draws with that GS bound currently return a clear error; other draws still use a synthetic expansion shader for bring-up/coverage (guest GS DXBC does not execute).
      - The intended end state is: VS-as-compute (vertex pulling) → GS-as-compute (primitive expansion) → render expanded buffers with a passthrough VS + the original PS.
    * This is **internal** WebGPU compute; it does *not* require exposing the D3D11 compute shader stage (you can still keep D3D11 CS as `NOT_SUPPORTED` initially).
    * Details: [`geometry-shader-emulation.md`](./geometry-shader-emulation.md).
    * **Current repo status:** the host-side executor’s GS/HS/DS compute-prepass path uses synthetic expansion geometry for most draws, but `PointList`, `LineList`, and `TriangleList` draws (`Draw` and `DrawIndexed`) can execute a minimal translated SM4 GS subset. Feeding GS inputs from the bound VS is partially implemented via a minimal VS-as-compute path (simple VS subset), with an IA-fill fallback for strict passthrough VS. Broader GS DXBC execution is still WIP. Creating a GS that cannot be translated is supported for robustness, but draws with that GS bound currently return a clear “geometry shader not supported” error. See [`docs/graphics/status.md`](./status.md) and [`geometry-shader-emulation.md`](./geometry-shader-emulation.md).
* Win7 regression tests that define the minimum semantics to target:
  * `drivers/aerogpu/tests/win7/d3d11_geometry_shader_smoke` — basic GS create/bind/execute path.
  * `drivers/aerogpu/tests/win7/d3d11_geometry_shader_restart_strip` — validates `TriangleStream::RestartStrip` / DXBC `cut` handling.
    * This matters because the intended AeroGPU GS emulation expands `triangle_strip` output into a list topology for rendering; if you drop the cut/restart markers you can generate “bridging” triangles between strips (visible corruption, and this test fails by detecting pixels filled in the gap between two emitted strips).
* If you are not ready to support GS (e.g. host backend without compute), prefer advertising only `D3D_FEATURE_LEVEL_9_x` for D3D11 (while still supporting D3D10 separately), or be explicit that some FL10_0 apps will fail when they create/bind GS.

#### 2.2.3 Mandatory context/state binding + draw path

At FL10_0, D3D11 essentially needs the D3D10-era pipeline:

Immediate context function table: `D3D11DDI_DEVICECONTEXTFUNCS`

Input Assembler
* `pfnIaSetInputLayout`
* `pfnIaSetVertexBuffers`
* `pfnIaSetIndexBuffer`
* `pfnIaSetTopology` (sets `D3D*_PRIMITIVE_TOPOLOGY`; corresponds to `IASetPrimitiveTopology` at the API level)

Shaders + resource binding (VS/PS, plus GS if advertising FL10_0+)
* `pfnVsSetShader`, `pfnPsSetShader`
* `pfnVsSetConstantBuffers`, `pfnPsSetConstantBuffers`
* `pfnVsSetShaderResources`, `pfnPsSetShaderResources`
* `pfnVsSetSamplers`, `pfnPsSetSamplers`

Geometry shader stage (required for FL10_0+; optional only if you advertise FL9_x)
* `pfnGsSetShader`
* `pfnGsSetConstantBuffers`
* `pfnGsSetShaderResources`
* `pfnGsSetSamplers`

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
* `pfnMap` + `pfnUnmap` (see [`win7-d3d11-map-unmap.md`](./win7-d3d11-map-unmap.md) for the definitive Win7 Map/Unmap + `LockCb`/`UnlockCb` contract)
  * must cover both:
    * dynamic update patterns (`D3D11_MAP_WRITE_DISCARD` / `D3D11_MAP_WRITE_NO_OVERWRITE`) for buffers/constant buffers, and
    * staging readback (`D3D11_MAP_READ` on `D3D11_USAGE_STAGING` resources) for tests and debugging
      * Win7 fence wait reference (exact CB struct + field names): [`win7-d3d10-11-umd-callbacks-and-fences.md`](./win7-d3d10-11-umd-callbacks-and-fences.md)
* `pfnUpdateSubresourceUP` (user-memory upload path for `UpdateSubresource`)
  * struct: `D3D11DDIARG_UPDATESUBRESOURCEUP`
* `pfnCopyResource` / `pfnCopySubresourceRegion`
* `pfnFlush` (submits pending work; corresponds to `ID3D11DeviceContext::Flush`)

See also:

* `docs/graphics/win7-d3d10-11-umd-allocations.md` — Win7/WDDM 1.1 resource allocation (`CreateResource` → `pfnAllocateCb`) contract.
* `docs/graphics/win7-d3d11-map-unmap.md` — Win7 `Map`/`Unmap` semantics (`LockCb`/`UnlockCb`) for dynamic uploads + staging readback.

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

* `pfnPresent` (with `D3D10DDIARG_PRESENT`)
* `pfnRotateResourceIdentities` (rotate swapchain backbuffer resources)

### 3.3 ResizeBuffers / ResizeTarget expectations

Apps commonly call:

* `IDXGISwapChain::ResizeBuffers(0, 0, 0, DXGI_FORMAT_UNKNOWN, Flags)` to “resize to window”
* `IDXGISwapChain::ResizeTarget(&DXGI_MODE_DESC)` sometimes, but can be accepted-and-ignored for windowed-only

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

### 4.3 UAVs (optional for FL10_0 bring-up; buffer UAVs first)

UAVs only become required once you support compute or advanced pixel pipeline features.

**Win7 bring-up recommendation:**

* Implement RTV/DSV/SRV first.
* For an initial FL10_0 path, it is valid to return NOT_SUPPORTED / `E_NOTIMPL` for `CreateUnorderedAccessView` and the `*SetUnorderedAccessViews` family.

When you do add compute + UAV support (FL11_0-era features), start with **buffer UAVs**:

* `u#` bindings for `RWByteAddressBuffer` / `RWStructuredBuffer`-like access (`u0..u7` in D3D11 compute).
* You can still defer **typed UAV textures** (`RWTexture*`) until the required format plumbing exists.

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
* Be prepared for “SM4 level 9” DXBC variants like `vs_4_0_level_9_1` / `ps_4_0_level_9_1` (commonly produced by `fxc` for feature level 9.x compatibility). These still use the DXBC container format; the shader version token simply indicates a more restricted instruction/resource subset.

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

1. **Complete the FL10_0 feature set** (if you started at FL9_x for bring-up)
   * Geometry shader support is expected at FL10_0+:
     * implement `pfnCreateGeometryShader` and the GS bind/resource entrypoints (`pfnGsSet*`)
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
  * includes D3D11 coverage tests that exercise a large chunk of the Win7 DXGI/D3D11 path (swapchain present, offscreen render-to-texture readback, texture sampling, dynamic buffer/constant buffer updates, depth/stencil, etc).

### 7.1 D3D10 triangle (windowed)

**Covers:**

* `OpenAdapter10` → `CreateDevice`
* swapchain creation (`DXGI_SWAP_CHAIN_DESC`)
* `CreateResource` (VB), `CreateRenderTargetView`
* `CreateVertexShader` / `CreatePixelShader` (SM4.0)
* `IASet*`, `VSSetShader`, `PSSetShader`, `OMSetRenderTargets`
* `Draw`, `Present`

**Existing in repo:**

* `drivers/aerogpu/tests/win7/d3d10_triangle/` — swapchain triangle + present.
* `drivers/aerogpu/tests/win7/d3d10_map_do_not_wait/` — validates `Map(READ, DO_NOT_WAIT)` behaves like a non-blocking poll for staging readback.

### 7.2 D3D11 triangle (device at FL10_0)

**Covers:**

* `OpenAdapter11` → `CreateDevice`
* D3D11 binding path (VS/PS only)
* same draw/present path as above, but via D3D11 runtime/DDI

**Existing in repo:**

* `drivers/aerogpu/tests/win7/d3d11_triangle/` — swapchain triangle + present; validates pixels via staging readback.
* `drivers/aerogpu/tests/win7/readback_sanity/` — offscreen render-to-texture + staging readback (no present).
* `drivers/aerogpu/tests/win7/d3d11_map_do_not_wait/` — validates `Map(READ, DO_NOT_WAIT)` behaves like a non-blocking poll for staging readback.
* `drivers/aerogpu/tests/win7/d3d11_compute_smoke/` — compute shader smoke test: binds `b0` constant buffer + SRV/UAV buffers (structured + raw), `Dispatch`, and validates output via staging readback.

### 7.2.1 D3D10.1 coverage (optional, still useful on Win7)

The Windows 7 D3D10.1 runtime can route some Map/Unmap patterns through slightly different DDIs.
Having a dedicated D3D10.1 test helps catch regressions in those entrypoints.

**Existing in repo:**

* `drivers/aerogpu/tests/win7/d3d10_1_triangle/` — swapchain triangle + present via the D3D10.1 runtime.
* `drivers/aerogpu/tests/win7/d3d10_1_map_do_not_wait/` — D3D10.1 variant of the `Map(READ, DO_NOT_WAIT)` non-blocking poll test.

### 7.3 Texture sampling test (2D)

Render a textured quad:

**Covers:**

* `CreateResource` (Texture2D)
* `CreateShaderResourceView`, `CreateSampler`
* texture upload path (`Map/Unmap` or `UpdateSubresource`)
* `PSSetShaderResources`, `PSSetSamplers`
* shader translation handling `sample` ops

**Existing in repo:** `drivers/aerogpu/tests/win7/d3d11_texture_sampling_sanity/` (renders a point-sampled textured quad using an SRV + sampler and validates pixels via staging readback; exercises `IASetIndexBuffer` + `DrawIndexed`).

### 7.4 Depth test

Draw two overlapping triangles with different Z and enable depth:

**Covers:**

* `CreateResource` (depth buffer)
* `CreateDepthStencilView`
* `CreateDepthStencilState` + `SetDepthStencilState`
* `ClearDepthStencilView`
* correct depth compare/write behavior in the host translation layer

**Existing in repo:** `drivers/aerogpu/tests/win7/d3d11_depth_test_sanity/` (offscreen RTV + DSV; clears depth then draws overlapping triangles and validates the center pixel is depth-tested).

### 7.5 Dynamic constant buffer test

Draw using a dynamic constant buffer updated via `Map(WRITE_DISCARD)`:

**Covers:**

* `CreateBuffer` (dynamic constant buffer)
* `Map(WRITE_DISCARD)` + `Unmap` and constant-buffer binding (`*SetConstantBuffers`)
* validation that constant-buffer updates take effect between draws

**Existing in repo:** `drivers/aerogpu/tests/win7/d3d11_dynamic_constant_buffer_sanity/`.

Bring-up note: `Map(WRITE_DISCARD)` on dynamic buffers must work even before the KMD implements `DxgkDdiLock` / `DxgkDdiUnlock` (which back the runtime `pfnLockCb`/`pfnUnlockCb`). AeroGPU supports this by treating dynamic buffers as **host-owned** (`backing_alloc_id = 0`) and mapping an in-UMD shadow buffer, uploading via `AEROGPU_CMD_UPLOAD_RESOURCE` on Unmap.

### 7.6 `Map(READ, DO_NOT_WAIT)` staging readback behavior

Validate that `Map(READ, DO_NOT_WAIT)` behaves like a **non-blocking poll** (returns `DXGI_ERROR_WAS_STILL_DRAWING` while GPU work is still in flight), and that the blocking `Map(READ)` variant waits for GPU completion and returns correct bytes.

**Existing in repo:**

* `drivers/aerogpu/tests/win7/d3d10_map_do_not_wait/`
* `drivers/aerogpu/tests/win7/d3d10_1_map_do_not_wait/`
* `drivers/aerogpu/tests/win7/d3d11_map_do_not_wait/`

### 7.7 Shared resources / DXGI shared handles (cross-process)

On Win7, DWM (D3D9Ex) commonly consumes **DXGI shared handles** produced by D3D10/D3D11 apps. Ensure that:

* shared resources create a stable `share_token` in preserved WDDM allocation private data, and
* cross-process `OpenSharedResource(...)` drives `IMPORT_SHARED_SURFACE` using that stable token.

Canonical contract and rationale: `docs/graphics/win7-shared-surfaces-share-token.md`.

**Existing in repo:**

* `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc/`
* `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc/`
* `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/`

---

### Appendix: practical bring-up order

1. Implement D3D11 DDI at FL10_0 first (many samples use D3D11 even when targeting “DX10 class”).
2. Reuse the same underlying object model to implement D3D10 DDI entrypoints.
3. Only after triangle/texture/dynamic-constant-buffer/depth pass, start expanding caps and feature level.
