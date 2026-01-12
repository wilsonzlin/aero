# Win7 D3D11 UMD DDI function tables (FL10_0): required vs stubbable checklist

This is an implementation-grade reference for bringing up a **crash-free** D3D11 UMD on
**Windows 7 SP1 (WDDM 1.1 / DXGI 1.1)**.

It answers a very practical question:

> When the Win7 D3D11 runtime loads your UMD, which `d3d11umddi.h` function table entries
> must be non-null, which ones must actually *work* for `D3D_FEATURE_LEVEL_10_0`, and which
> ones can be safely stubbed with `E_NOTIMPL` / `SetErrorCb(E_NOTIMPL)` until later?

This doc is intentionally biased toward a **safe skeleton**:

* **Never leave a DDI function pointer NULL.** If the runtime calls a NULL pointer, you crash the process.
* Prefer “present but failing cleanly” over “missing”.
* Keep `pfnGetCaps` conservative: do not advertise features you don’t implement end-to-end.

> Related bring-up doc: `docs/graphics/win7-d3d10-11-umd-minimal.md` (conceptual bring-up plan and minimal feature set).
>
> Callback/fence symbol-name reference (Win7 WDK): `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`
>
> Repo pointers (AeroGPU implementation):
> * UMD code: `drivers/aerogpu/umd/d3d10_11/`
> * Win7 D3D11 guest tests referenced below:
>   * `drivers/aerogpu/tests/win7/d3d11_caps_smoke`
>   * `drivers/aerogpu/tests/win7/d3d11_triangle`
>   * `drivers/aerogpu/tests/win7/d3d11_texture_sampling_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_dynamic_constant_buffer_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_depth_test_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_update_subresource_texture_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_map_dynamic_buffer_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_rs_om_state_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_swapchain_rotate_sanity`
>   * `drivers/aerogpu/tests/win7/d3d11_geometry_shader_smoke`
>   * `drivers/aerogpu/tests/win7/readback_sanity`

---

## TL;DR: minimal non-null + must-work set (FL10_0 + core Win7 D3D11 tests)

If your goal is “the Win7 runtime creates a D3D11 device at **FL10_0** and the core Win7 D3D11 tests don’t crash”,
this is the smallest practical set to treat as **must be non-null and must succeed**.

> Tests referenced:
> * `drivers/aerogpu/tests/win7/d3d11_triangle`
> * `drivers/aerogpu/tests/win7/d3d11_texture_sampling_sanity`
> * `drivers/aerogpu/tests/win7/d3d11_dynamic_constant_buffer_sanity`
> * `drivers/aerogpu/tests/win7/d3d11_depth_test_sanity`
> * `drivers/aerogpu/tests/win7/readback_sanity`
>
> See §6.6+ for additional Win7 D3D11 tests that tighten semantics (swapchain rotation, scissor/blend, geometry shaders, etc).

### Adapter (`D3D11DDI_ADAPTERFUNCS`)

Must be non-null and must succeed:

* `pfnGetCaps`
* `pfnCalcPrivateDeviceSize`
* `pfnCalcPrivateDeviceContextSize` (if present in your `D3D11DDI_ADAPTERFUNCS` layout)
* `pfnCreateDevice` (must fill both device + immediate context tables)
* `pfnCloseAdapter`

### Device funcs (`D3D11DDI_DEVICEFUNCS`)

Must be non-null and must succeed for the tests:

* Device lifetime:
  * `pfnDestroyDevice`
* Resources:
  * `pfnCalcPrivateResourceSize`, `pfnCreateResource`, `pfnDestroyResource`
  * must handle (at minimum):
    * `D3D11_USAGE_DEFAULT` buffers created with `D3D11_SUBRESOURCE_DATA` (initial data upload)
    * `D3D11_USAGE_DEFAULT` `Texture2D` render targets (BGRA)
    * `D3D11_USAGE_STAGING` `Texture2D` with `CPU_ACCESS_READ` (staging readback)
* RTV:
  * `pfnCalcPrivateRenderTargetViewSize`, `pfnCreateRenderTargetView`, `pfnDestroyRenderTargetView`
* SRV + sampler:
  * `pfnCalcPrivateShaderResourceViewSize`, `pfnCreateShaderResourceView`, `pfnDestroyShaderResourceView`
  * `pfnCalcPrivateSamplerSize`, `pfnCreateSampler`, `pfnDestroySampler`
* Depth/stencil:
  * `pfnCalcPrivateDepthStencilViewSize`, `pfnCreateDepthStencilView`, `pfnDestroyDepthStencilView`
  * `pfnCalcPrivateDepthStencilStateSize`, `pfnCreateDepthStencilState`, `pfnDestroyDepthStencilState`
* Shaders:
  * `pfnCalcPrivateVertexShaderSize`, `pfnCreateVertexShader`, `pfnDestroyVertexShader`
  * `pfnCalcPrivatePixelShaderSize`, `pfnCreatePixelShader`, `pfnDestroyPixelShader`
* Input layout:
  * `pfnCalcPrivateElementLayoutSize`, `pfnCreateElementLayout`, `pfnDestroyElementLayout`

Everything else should still be **non-null** (stubbed), but may return `E_NOTIMPL`.

### Immediate context (`D3D11DDI_DEVICECONTEXTFUNCS`)

Must be non-null and must succeed for the tests:

* Binding/state:
  * `pfnSetRenderTargets`
  * `pfnSetViewports`
  * `pfnIaSetInputLayout`, `pfnIaSetTopology`, `pfnIaSetVertexBuffers`, `pfnIaSetIndexBuffer`
  * `pfnVsSetShader`, `pfnPsSetShader`
  * `pfnVsSetShaderResources`, `pfnPsSetShaderResources`
  * `pfnVsSetSamplers`, `pfnPsSetSamplers`
  * `pfnVsSetConstantBuffers`, `pfnPsSetConstantBuffers` (dynamic constant buffer binding)
  * `pfnSetDepthStencilState`
* Clears/draws:
  * `pfnClearRenderTargetView`
  * `pfnClearDepthStencilView`
  * `pfnDraw`, `pfnDrawIndexed`
* Resource updates:
  * `pfnUpdateSubresourceUP`
* Readback path:
  * `pfnCopyResource`
  * `pfnFlush`
  * `pfnMap`, `pfnUnmap`
* Win7 DXGI present integration (swapchains):
  * `pfnPresent` (DXGI uses `D3D10DDIARG_PRESENT` even for D3D11 devices on Win7)
  * `pfnRotateResourceIdentities`

Everything else should still be **non-null** (stubbed, usually via `SetErrorCb(E_NOTIMPL)` for `void` DDIs),
because the runtime may call “reset to default” entrypoints like `ClearState` during initialization.

Practical stub tip:

* Many state-setting DDIs are called with **NULL** handles specifically to unbind/reset state.
  For those “unbind” patterns, it’s usually better to treat the call as a no-op success (no `SetErrorCb`),
  otherwise you can end up reporting errors during normal initialization/`ClearState` even though the app is not using the missing feature.
  * Examples seen in practice on Win7: HS/DS/CS binds (tessellation/compute), `SoSetTargets`, `SetPredication`,
    and debug markers / discards (`SetMarker`/`BeginEvent`/`EndEvent`, `DiscardResource`, `DiscardView`).

---

## 0) Terminology and rules used in this checklist

### Status tags

Each entrypoint is marked as one of:

* **REQUIRED**: must be non-null and implemented correctly for a functional **FL10_0** device (and the “core bring-up” tests listed in the TL;DR section above).
* **REQUIRED-BUT-STUBBABLE**: must be non-null (the runtime *may* call it), but it can fail cleanly until the feature is implemented.
* **OPTIONAL**: not required for FL10_0 bring-up; can usually be stubbed and may never be called unless the app opts into the feature.

### Stubbing failure modes (`HRESULT` vs `SetErrorCb`)

The D3D11 UMD DDI has two error-reporting styles:

* **`HRESULT`-returning DDIs**: return `E_NOTIMPL` / `E_INVALIDARG` / `E_OUTOFMEMORY` as appropriate.
* **`void` DDIs**: report failure through the runtime callback (commonly `pfnSetErrorCb(...)`) and return.

Practical rule:

* If the DDI is `void`, use: `pfnSetErrorCb(<device-handle>, E_NOTIMPL)` (or `E_INVALIDARG`) using whatever handle type your headers declare (`D3D11DDI_HRTDEVICE` vs `D3D11DDI_HDEVICE`).
* If the DDI returns `HRESULT`, return the error code directly.

Do **not** “half-stub” a `void` DDI by silently doing nothing if it is supposed to create/modify state the runtime relies on; that often leads to later GPU hangs or invalid command streams.

Important detail: most `void` DDIs live on the **device context table** and are called as:

* `pfnSomething(D3D11DDI_HDEVICECONTEXT hCtx, ...)` (no `hDevice` parameter)

But the error callback is device-scoped and Win7-era WDK headers disagree on whether it expects the **runtime device handle** (`D3D11DDI_HRTDEVICE`) or the driver `D3D11DDI_HDEVICE`.

In practice that means your context-private struct should point back to the parent device object so you can reach the stored device handle and call:

* `pfnSetErrorCb(<device-handle>, E_NOTIMPL);`

For exact Win7 WDK symbol names/fields (`D3D11DDIARG_CREATEDEVICE::hRTDevice`, `...::pCallbacks->pfnSetErrorCb`, etc), see:

* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

### Non-null discipline: stub-fill, then override

For Win7 stability, the simplest pattern is:

1. Build a “fully stubbed” `D3D11DDI_ADAPTERFUNCS` / `D3D11DDI_DEVICEFUNCS` / `D3D11DDI_DEVICECONTEXTFUNCS` where **every field is non-null**.
2. In `OpenAdapter11` and `pfnCreateDevice`, start from the stub table and overwrite only the functions you’ve implemented.

This is robust against:

* “surprise” runtime calls into rarely-used entrypoints during initialization, and
* adding fields when you switch `D3D11DDI_INTERFACE_VERSION` (new fields defaulting to NULL is a common crash source).

Pseudocode shape:

```c
// 1) A stub that matches the failure style of the DDI entrypoint.
static HRESULT APIENTRY Stub_HRESULT(...) { return E_NOTIMPL; }
static void APIENTRY Stub_VOID(D3D11DDI_HDEVICECONTEXT hCtx, ...) {
  g_DeviceCallbacks.pfnSetErrorCb(DeviceHandleFromContext(hCtx), E_NOTIMPL);
}

// 2) A fully-populated table (every field assigned).
static const D3D11DDI_DEVICEFUNCS kStubDeviceFuncs = { /* ...all fields... */ };
static const D3D11DDI_DEVICECONTEXTFUNCS kStubCtxFuncs = { /* ...all fields... */ };

// 3) In CreateDevice: copy then override.
*pCreateDevice->pDeviceFuncs = kStubDeviceFuncs;
*pCreateDevice->pDeviceContextFuncs = kStubCtxFuncs;
pCreateDevice->pDeviceFuncs->pfnCreateResource = &MyCreateResource;
pCreateDevice->pDeviceContextFuncs->pfnDraw = &MyDraw;
```

Don’t overthink the stub implementation: `E_NOTIMPL` + `SetErrorCb(E_NOTIMPL)` is enough as long as it never dereferences invalid handles.

### Stub templates by signature (copy/paste starting point)

Most of the D3D11 UMD DDI surface fits into a few signature patterns. For a skeleton driver, it’s common to implement a small set of generic stubs and use them to populate the tables.

```c
// CalcPrivate*Size: runtime uses this to allocate hXxx.pDrvPrivate storage.
static SIZE_T APIENTRY Stub_CalcPrivateSize(...) {
  return sizeof(uint64_t); // keep non-zero; easiest to reason about
}

// Create*: HRESULT-returning (common for object creation).
static HRESULT APIENTRY Stub_Create_HRESULT(...) {
  return E_NOTIMPL;
}

// Destroy*: void-returning (common for object destruction).
static void APIENTRY Stub_Destroy_VOID(...) {
  // Must be safe on partially-initialized objects.
}

// Context-state setters and draws are usually void and take HDEVICECONTEXT first.
//
// For bring-up, consider special-casing “unbind” calls as a no-op (do not set
// an error) since the runtime will frequently reset state by binding NULLs.
static void APIENTRY Stub_Ctx_VOID(D3D11DDI_HDEVICECONTEXT hCtx, ...) {
  g_DeviceCallbacks.pfnSetErrorCb(DeviceHandleFromContext(hCtx), E_NOTIMPL);
}
```

These are intentionally “dumb but safe”. Once you start implementing a feature, override the specific entrypoints while leaving unrelated ones stubbed.

---

## 1) Win7 loader flow (what calls what, in what order)

On Win7, the D3D11 runtime loads your UMD (a DLL) and uses the exported `OpenAdapter11` entrypoint to obtain an adapter function table.

High-level call flow:

```text
LoadLibrary(<your_umd>.dll)
  GetProcAddress("OpenAdapter11")
    OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData)
      -> driver fills: D3D11DDI_ADAPTERFUNCS (adapter function table)
      -> driver stores: runtime callback tables (adapter/device callbacks; used for `SetErrorCb`, allocation callbacks, etc)

    runtime calls adapter->pfnGetCaps(...)  [multiple queries]
    runtime calls adapter->pfnCalcPrivateDeviceSize(...)
    runtime allocates driver-private memory for the handles it passes to CreateDevice
    (at least a `D3D11DDI_HDEVICE`, and typically an immediate `D3D11DDI_HDEVICECONTEXT` as well).

    runtime calls adapter->pfnCreateDevice(...)
      -> driver constructs device + immediate context in provided private memory
      -> driver fills BOTH:
           D3D11DDI_DEVICEFUNCS         (device/object creation & lifetime)
           D3D11DDI_DEVICECONTEXTFUNCS  (immediate context: state, draws, copies, map/unmap, flush)
```

### 1.1 Callback tables: what you must store to report errors safely

The runtime provides callback tables at adapter/device creation time. You must store them in your private adapter/device objects and treat them as **valid only until the corresponding Close/Destroy call**.

At minimum you need the callback that reports errors from `void` DDIs:

* `pfnSetErrorCb` (device-scoped; see §0 “Stubbing failure modes” for the context-vs-device detail)

Practical guidance:

* Store callbacks in the object that “owns” the handle they are associated with:
  * adapter callbacks in the adapter private struct
  * device callbacks in the device private struct
  * context private struct should point back to the parent device (so it can reach the stored device handle and call `pfnSetErrorCb`)
* Never call callbacks after `pfnCloseAdapter` / `pfnDestroyDevice`.

Win7-specific gotchas:

* `OpenAdapter11` is declared as `HRESULT APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER *pOpenData)` on Win7:
  the container is still `D3D10DDIARG_OPENADAPTER` even though you return **D3D11** tables.
* DXGI 1.1 swapchains drive present through the D3D10-style present structures:
  * `D3D10DDIARG_PRESENT` is used even for D3D11 devices.
  * buffer rotation uses `pfnRotateResourceIdentities`.

### 1.2 Interface version negotiation (`D3D11DDI_INTERFACE_VERSION`)

The Win7 D3D11 runtime uses `D3D10DDIARG_OPENADAPTER::Interface` / `::Version` as an ABI negotiation step:

* `Interface` must match the D3D11 DDI selector (commonly `D3D11DDI_INTERFACE_VERSION`; some WDKs also expose `D3D11DDI_INTERFACE`)
* `Version` determines the expected struct layout for the device/context function tables

If you accept an unsupported `Version`, the runtime may interpret your filled
`D3D11DDI_DEVICEFUNCS` / `D3D11DDI_DEVICECONTEXTFUNCS` with the wrong layout and crash.

Recommended driver behavior:

* `OpenAdapter11` validates the incoming interface/version.
* If the runtime requests a newer `Version` than you support, clamp `pOpenData->Version` down to your supported version (commonly `D3D11DDI_SUPPORTED` or `D3D11DDI_INTERFACE_VERSION`, depending on the WDK), matching the D3D10.x negotiation pattern.
* Store the negotiated `Version` in adapter-private state and ensure `pfnCreateDevice` fills
  `D3D11DDI_DEVICEFUNCS` / `D3D11DDI_DEVICECONTEXTFUNCS` matching that struct layout.

---

## 2) Adapter function table: `D3D11DDI_ADAPTERFUNCS`

You return `D3D11DDI_ADAPTERFUNCS` from `OpenAdapter11`. On Win7, treat every field as **must be non-null**.

If your chosen `D3D11DDI_INTERFACE_VERSION` adds adapter-func fields beyond the ones listed here, apply the same rule:

* keep the pointer **non-null**, and
* return a clean failure (`E_NOTIMPL` / `E_INVALIDARG`) rather than leaving it NULL.

| Field | Status | Must succeed? | Notes / failure guidance |
|---|---|---:|---|
| `pfnGetCaps` | REQUIRED | **Yes** for the “minimum caps set” in §3 | Return conservative answers; unknown `Type` must not crash. |
| `pfnCalcPrivateDeviceSize` | REQUIRED | Yes | Must return a valid non-zero size for your `D3D11DDI_HDEVICE` private storage (and, depending on interface version, may include immediate context storage). |
| `pfnCreateDevice` | REQUIRED | Yes | Must fill `D3D11DDI_DEVICEFUNCS` + `D3D11DDI_DEVICECONTEXTFUNCS` and return `S_OK`. |
| `pfnCloseAdapter` | REQUIRED | N/A | Free adapter-private state; never call back into the runtime after closing. |

---

## 3) `pfnGetCaps`: minimum `D3D11DDIARG_GETCAPS::Type` coverage for FL10_0

`pfnGetCaps` is where the D3D11 runtime learns what you support. Device creation is gated by the results.

### 3.1 “Unknown caps types” must be handled gracefully

This is a reliability requirement: **Win7 will probe more caps than you expect**, and the probe set differs by OS patch level.

Recommended robust behavior:

1. Treat `pData == NULL` as a “size query” when possible:
   * For fixed-size outputs, set `DataSize = sizeof(<expected struct>)` (if `DataSize` is in/out for your header version) and return `S_OK`.
   * For variable-size outputs, report the required size for the current adapter/device configuration and return `S_OK`.
2. For non-null `pData`, validate `DataSize` is at least what you need for that `Type` (fail with `E_INVALIDARG` rather than overrunning the buffer).
3. If `Type` is unknown:
   * **zero-fill** `pData` (up to `DataSize`) and return `S_OK`, **or**
   * return `E_INVALIDARG` (only if you’ve confirmed Win7 runtime tolerates failure for that `Type`).
4. Log unknown `Type` values (once) so you can expand coverage intentionally.

### 3.2 Minimum caps queries that typically gate device creation (FL10_0)

The exact set can vary, but in practice the Win7 D3D11 runtime usually needs at least:

| `D3D11DDIARG_GETCAPS::Type` | Required? | What to return (conservative baseline) |
|---|---:|---|
| `D3D11DDICAPS_TYPE_FEATURE_LEVELS` | Yes | Return a feature level list containing `D3D_FEATURE_LEVEL_10_0` (and *only* the levels you truly support). In practice, Win7-era runtimes/headers may interpret the buffer as either `{ UINT NumFeatureLevels; const D3D_FEATURE_LEVEL* pFeatureLevels; }` **or** `{ UINT NumFeatureLevels; D3D_FEATURE_LEVEL FeatureLevels[...]; }` (inline). When possible (x64), populate both layouts to avoid mismatched interpretation. On x86 the pointer field overlaps the first inline element, so you **cannot** satisfy both simultaneously; prefer the `{count, pointer}` layout because returning an inline `D3D_FEATURE_LEVEL` value where a pointer is expected can crash the runtime (attempting to dereference e.g. `0xA000`). |
| `D3D11DDICAPS_TYPE_THREADING` | Yes | Disable advanced threading unless implemented: `DriverConcurrentCreates = FALSE`, `DriverCommandLists = FALSE`. |
| `D3D11DDICAPS_TYPE_SHADER` | Yes | Claim only SM4.x for FL10_0: VS/GS/PS `*_4_0`-class support; no SM5-only stages. The output begins with per-stage shader-model “version tokens” (`UINT`) in DXBC encoding: `(program_type << 16) | (major << 4) | minor` (typically PS/VS/GS/HS/DS/CS order), with unimplemented stages left as 0. |
| `D3D11DDICAPS_TYPE_FORMAT` | Yes | Report support for the formats you need for DXGI swapchains + staging readback (see §3.3). |
| `D3D11DDICAPS_TYPE_D3D10_X_HARDWARE_OPTIONS` | Recommended | For FL10_0 bring-up: set `ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x = FALSE` unless you implement CS + raw/structured buffers. |
| `D3D11DDICAPS_TYPE_D3D11_OPTIONS` | Recommended | Return all options `FALSE` initially (no UAV-only features, no logic ops, etc). |
| `D3D11DDICAPS_TYPE_ARCHITECTURE_INFO` | Recommended | Conservative: `TileBasedDeferredRenderer = FALSE`, `UMA = FALSE`, `CacheCoherentUMA = FALSE`. |
| `D3D11DDICAPS_TYPE_DOUBLES` | Recommended | For FL10_0 bring-up: return `DoublePrecisionFloatShaderOps = FALSE`. |
| `D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS` | Recommended | If asked for `SampleCount==1`, return `NumQualityLevels >= 1` **only** for supported formats (Win7 apps often probe this early). Return `0` for unsupported formats so DXGI/D3D won’t pick an MSAA path for a format you can’t create. |

> Why “Recommended” is still important: many apps call `ID3D11Device::CheckFeatureSupport(...)`
> early. Even if the runtime can create a device without these, returning garbage here causes
> surprising app behavior.

### 3.3 Minimum *format* support required by the repo’s Win7 tests

For the current guest tests, the runtime needs at least:

| Format | Required usages (minimum) | Where it’s used |
|---|---|---|
| `DXGI_FORMAT_B8G8R8A8_UNORM` | `RENDER_TARGET`, `TEXTURE2D` | swapchain backbuffer in `d3d11_triangle` and render-target texture in `readback_sanity`. |
| `DXGI_FORMAT_R8G8B8A8_UNORM` | `RENDER_TARGET`, `TEXTURE2D` | common app fallback; good to support early even if tests use BGRA. |
| Depth formats (`DXGI_FORMAT_D24_UNORM_S8_UINT` or `DXGI_FORMAT_D32_FLOAT`) | `DEPTH_STENCIL`, `TEXTURE2D` | required by `d3d11_depth_test_sanity`; common for real apps. |
| `DXGI_FORMAT_R16_UINT` / `DXGI_FORMAT_R32_UINT` | `BUFFER`, `IA_INDEX_BUFFER` | required by `d3d11_caps_smoke` and common for indexed draws. |
| `DXGI_FORMAT_R32G32_FLOAT`, `DXGI_FORMAT_R32G32B32A32_FLOAT` | `BUFFER`, `IA_VERTEX_BUFFER` | required for the repo’s input layouts (multiple tests). |

BGRA device flag note:

* The Win7 tests create the device with `D3D11_CREATE_DEVICE_BGRA_SUPPORT`.
* In practice this means you must report BGRA support in `pfnGetCaps` (format caps) and successfully create BGRA render targets, or `D3D11CreateDevice*` may fail early.

Staging readback path requirements:

* `CopyResource` / `CopySubresourceRegion` must be able to copy from a DEFAULT render target into a STAGING texture.
* `Map(D3D11_MAP_READ)` on that staging texture must succeed.

If you are not ready to support a format for a given usage:

* make `D3D11DDICAPS_TYPE_FORMAT` report it as unsupported, **and**
* return a clean failure from the corresponding create call (`E_INVALIDARG` or `E_NOTIMPL`).

---

## 4) Device function table: `D3D11DDI_DEVICEFUNCS` checklist

This is the “device-level” function table you fill in `pfnCreateDevice`. It covers object lifetime and creation.

### 4.1 Minimum rule for crash-free bring-up

Populate **every** field in `D3D11DDI_DEVICEFUNCS` with a non-null function pointer. Even if you are not implementing a feature, provide a stub:

* `CalcPrivate*Size` returns a non-zero size (often `sizeof(YourDummyObject)`).
* `Create*` returns `E_NOTIMPL` / `E_INVALIDARG` if unsupported.
* `Destroy*` is a safe no-op if the object was never successfully created.

If a field exists in your `d3d11umddi.h` but is not explicitly mentioned in this doc, treat it as:

* **OPTIONAL** for FL10_0 bring-up, and
* still **non-null** (stubbed).

### 4.2 Function pointer checklist (grouped)

> Note: For any “Create*” below: if you don’t support the object yet, return `E_NOTIMPL` and do not touch the handle’s private memory.

#### 4.2.1 Device lifecycle

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnDestroyDevice` | REQUIRED | N/A (must work; freeing device is not optional). |

#### 4.2.2 Core resources (buffers + textures)

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateResourceSize` | REQUIRED | Return `sizeof(resource)` (even for unsupported resource kinds). |
| `pfnCreateResource` | REQUIRED | `HRESULT`: `E_NOTIMPL` for unsupported descs; `E_INVALIDARG` for invalid descs. |
| `pfnDestroyResource` | REQUIRED | `void`: must be safe on partially-initialized objects. |

Optional but common for real apps:

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnOpenResource` | REQUIRED-BUT-STUBBABLE | `HRESULT`: `E_NOTIMPL` is acceptable for early bring-up, but breaks DXGI/D3D11 shared-resource interop (`ID3D11Device::OpenSharedResource`). AeroGPU implements this in the Win7/WDDM 1.1 WDK UMD build (see `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/`). |

#### 4.2.3 Views (SRV / RTV / DSV / UAV)

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateShaderResourceViewSize` | REQUIRED | Return `sizeof(SRV)`. |
| `pfnCreateShaderResourceView` | REQUIRED | `HRESULT`: must succeed for Texture2D SRVs (used by `d3d11_texture_sampling_sanity`). |
| `pfnDestroyShaderResourceView` | REQUIRED | `void` no-op is OK. |
| `pfnCalcPrivateRenderTargetViewSize` | REQUIRED | Return `sizeof(RTV)`. |
| `pfnCreateRenderTargetView` | REQUIRED | `HRESULT`: must work for swapchain RTs and Texture2D RTs. |
| `pfnDestroyRenderTargetView` | REQUIRED | `void`. |
| `pfnCalcPrivateDepthStencilViewSize` | REQUIRED | Return `sizeof(DSV)`. |
| `pfnCreateDepthStencilView` | REQUIRED | `HRESULT`: must succeed for the depth formats you report (used by `d3d11_depth_test_sanity`). |
| `pfnDestroyDepthStencilView` | REQUIRED | `void`. |
| `pfnCalcPrivateUnorderedAccessViewSize` | OPTIONAL | Return `sizeof(UAV)`; `Create*` may return `E_NOTIMPL` for FL10_0. |
| `pfnCreateUnorderedAccessView` | OPTIONAL | `HRESULT`: `E_NOTIMPL` for FL10_0. |
| `pfnDestroyUnorderedAccessView` | OPTIONAL | `void`. |

#### 4.2.4 Shaders

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateVertexShaderSize` | REQUIRED | Return `sizeof(VS)`. |
| `pfnCreateVertexShader` | REQUIRED | `HRESULT`: must accept SM4.x DXBC. |
| `pfnDestroyVertexShader` | REQUIRED | `void`. |
| `pfnCalcPrivatePixelShaderSize` | REQUIRED | Return `sizeof(PS)`. |
| `pfnCreatePixelShader` | REQUIRED | `HRESULT`: must accept SM4.x DXBC. |
| `pfnDestroyPixelShader` | REQUIRED | `void`. |
| `pfnCalcPrivateGeometryShaderSize` | REQUIRED-BUT-STUBBABLE | Return `sizeof(GS)`. Required to pass `d3d11_geometry_shader_smoke`. |
| `pfnCreateGeometryShader` | REQUIRED-BUT-STUBBABLE | `HRESULT`: `E_NOTIMPL` allowed for bring-up, but breaks FL10_0 apps using GS and the `d3d11_geometry_shader_smoke` test. |
| `pfnDestroyGeometryShader` | REQUIRED-BUT-STUBBABLE | `void`. |
| `pfnCalcPrivateGeometryShaderWithStreamOutputSize` | OPTIONAL | Return `sizeof(GS+SO)`; `Create*` may return `E_NOTIMPL` until SO is implemented. |
| `pfnCreateGeometryShaderWithStreamOutput` | OPTIONAL | `HRESULT`: `E_NOTIMPL`. |

SM5/tessellation/compute (not required for FL10_0 bring-up):

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateHullShaderSize` / `pfnCreateHullShader` / `pfnDestroyHullShader` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateDomainShaderSize` / `pfnCreateDomainShader` / `pfnDestroyDomainShader` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateComputeShaderSize` / `pfnCreateComputeShader` / `pfnDestroyComputeShader` | OPTIONAL | `Create*` returns `E_NOTIMPL` unless you also report CS support in caps. |

#### 4.2.5 Fixed-function / pipeline state objects

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateElementLayoutSize` | REQUIRED | Return `sizeof(InputLayout)`; must support layouts used by tests. |
| `pfnCreateElementLayout` | REQUIRED | `HRESULT`: must work (D3D11 input layouts are required for most apps). |
| `pfnDestroyElementLayout` | REQUIRED | `void`. |
| `pfnCalcPrivateSamplerSize` / `pfnCreateSampler` / `pfnDestroySampler` | REQUIRED | Must succeed for point/clamp samplers (`d3d11_texture_sampling_sanity`). |
| `pfnCalcPrivateRasterizerStateSize` / `pfnCreateRasterizerState` / `pfnDestroyRasterizerState` | REQUIRED-BUT-STUBBABLE | Accept + store. To pass `d3d11_rs_om_state_sanity`, cull mode + front-face winding + scissor enable must work. |
| `pfnCalcPrivateBlendStateSize` / `pfnCreateBlendState` / `pfnDestroyBlendState` | REQUIRED-BUT-STUBBABLE | Accept + store. To pass `d3d11_rs_om_state_sanity`, basic alpha blending must work. |
| `pfnCalcPrivateDepthStencilStateSize` / `pfnCreateDepthStencilState` / `pfnDestroyDepthStencilState` | REQUIRED | Must succeed for depth test state (`d3d11_depth_test_sanity`). |

#### 4.2.6 Queries / predication / counters

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateQuerySize` / `pfnCreateQuery` / `pfnDestroyQuery` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivatePredicateSize` / `pfnCreatePredicate` / `pfnDestroyPredicate` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateCounterSize` / `pfnCreateCounter` / `pfnDestroyCounter` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |

#### 4.2.7 Deferred contexts / command lists / class linkage (advanced)

If you don’t implement deferred contexts yet, **still provide stubs** so an app calling `CreateDeferredContext` fails cleanly.

| Field | Status | Stub failure mode |
|---|---|---|
| `pfnCalcPrivateDeferredContextSize` / `pfnCreateDeferredContext` / `pfnDestroyDeferredContext` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateCommandListSize` / `pfnCreateCommandList` / `pfnDestroyCommandList` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateClassLinkageSize` / `pfnCreateClassLinkage` / `pfnDestroyClassLinkage` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |
| `pfnCalcPrivateClassInstanceSize` / `pfnCreateClassInstance` / `pfnDestroyClassInstance` | OPTIONAL | `Create*` returns `E_NOTIMPL`. |

#### 4.2.8 DXGI present integration (Win7 specific)

On Win7, DXGI uses `D3D10DDIARG_PRESENT` (DXGI 1.1) even for D3D11 devices.

Depending on `D3D11DDI_INTERFACE_VERSION`, the present/rotation entrypoints may be surfaced on either:

* `D3D11DDI_DEVICEFUNCS` (device table), or
* `D3D11DDI_DEVICECONTEXTFUNCS` (immediate context table; common for Win7 D3D11).

Treat `pfnPresent` and `pfnRotateResourceIdentities` as REQUIRED wherever they appear.
See §5.8 for the checklist entries and stub guidance.

---

## 5) Immediate context table: `D3D11DDI_DEVICECONTEXTFUNCS` checklist

This is the “immediate context” function table filled in `pfnCreateDevice`. It implements most of what `ID3D11DeviceContext` does.

### 5.1 Minimum rule for crash-free bring-up

Just like device funcs: populate **every field** with a non-null pointer and fail cleanly where unsupported. The runtime often calls many state setters during initialization (binding `NULL` to reset state); missing entrypoints here commonly crash on first device creation.

If a field exists in your `d3d11umddi.h` but is not explicitly mentioned in this doc, treat it as:

* **OPTIONAL** for FL10_0 bring-up, and
* still **non-null** (stubbed, usually via `SetErrorCb(E_NOTIMPL)` for `void` context DDIs).

### 5.2 Core pipeline binding (IA / VS / PS / GS)

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnIaSetInputLayout` | REQUIRED | Must accept valid layouts; accept NULL to unbind. |
| `pfnIaSetVertexBuffers` | REQUIRED | Must accept NULL buffers to unbind. |
| `pfnIaSetIndexBuffer` | REQUIRED | Used by `d3d11_texture_sampling_sanity`. |
| `pfnIaSetTopology` | REQUIRED | Required for `IASetPrimitiveTopology`. |
| `pfnVsSetShader` | REQUIRED | Must accept NULL to unbind. |
| `pfnPsSetShader` | REQUIRED | Must accept NULL to unbind. |
| `pfnGsSetShader` | REQUIRED-BUT-STUBBABLE | Required to pass `d3d11_geometry_shader_smoke`. For bring-up you can accept NULL only (and `SetErrorCb` if a non-NULL GS is bound). |

Resource/CB/sampler binding for FL10_0 pipeline:

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnPsSetConstantBuffers` | REQUIRED | Used by `d3d11_dynamic_constant_buffer_sanity`. Runtime may call with NULL to clear; handle that without error. |
| `pfnVsSetConstantBuffers` | REQUIRED | Used by `d3d11_dynamic_constant_buffer_sanity` (binds the CB to both VS and PS). Runtime may call with NULL to clear; handle that without error. |
| `pfnGsSetConstantBuffers` | REQUIRED-BUT-STUBBABLE | Keep non-null; implement as stages gain coverage. |
| `pfnPsSetShaderResources` | REQUIRED | Used by `d3d11_texture_sampling_sanity`; handle NULL to unbind. |
| `pfnVsSetShaderResources` | REQUIRED | Used by `d3d11_texture_sampling_sanity` (binds SRV to both VS and PS); handle NULL to unbind. |
| `pfnGsSetShaderResources` | REQUIRED-BUT-STUBBABLE | Keep non-null; implement as stages gain coverage. |
| `pfnPsSetSamplers` | REQUIRED | Used by `d3d11_texture_sampling_sanity`; handle NULL to unbind. |
| `pfnVsSetSamplers` | REQUIRED | Used by `d3d11_texture_sampling_sanity` (binds samplers to both VS and PS); handle NULL to unbind. |
| `pfnGsSetSamplers` | REQUIRED-BUT-STUBBABLE | Keep non-null; implement as stages gain coverage. |

#### 5.2.1 HS/DS/CS stages (optional for FL10_0 bring-up)

Even if you don’t support tessellation/compute yet, the context table will usually still contain entrypoints for:

* `pfnHsSet*` (hull shader stage)
* `pfnDsSet*` (domain shader stage)
* `pfnCsSet*` (compute shader stage)

Recommended stub behavior:

* If called with “unbind” semantics (NULL shader, NULL resources), treat it as a no-op success (don’t spam `SetErrorCb` during `ClearState`).
* If called with a non-NULL shader/resource that implies real execution, report `E_NOTIMPL` via `SetErrorCb`.

### 5.3 Rasterizer / output merger binding

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnSetViewports` | REQUIRED | `RSSetViewports`. |
| `pfnSetScissorRects` | REQUIRED-BUT-STUBBABLE | `d3d11_rs_om_state_sanity` requires scissor to clip rendering. If unimplemented, at minimum treat NULL/empty as disable (no error), but expect scissor-dependent tests to fail. |
| `pfnSetRasterizerState` | REQUIRED-BUT-STUBBABLE | `d3d11_rs_om_state_sanity` requires cull mode + front-face winding + scissor enable. For bring-up you can accept+store only, but expect state-dependent tests to fail. |
| `pfnSetBlendState` | REQUIRED-BUT-STUBBABLE | `d3d11_rs_om_state_sanity` requires basic alpha blending. For bring-up you can accept+store only, but expect blend-dependent tests to fail. |
| `pfnSetDepthStencilState` | REQUIRED | Must accept/store state for `d3d11_depth_test_sanity`. |
| `pfnSetRenderTargets` | REQUIRED | Must bind RTVs/DSV for draws and clears. |

### 5.3.1 State reset / convenience

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnClearState` | REQUIRED-BUT-STUBBABLE | Many apps (and sometimes runtimes) call `ID3D11DeviceContext::ClearState`. A safe bring-up implementation is “reset tracked bindings to defaults” (or even a no-op). Avoid calling `SetErrorCb` here: `ClearState` is commonly used as a non-failing reset path. |

### 5.4 Clears and draws

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnClearRenderTargetView` | REQUIRED | Must work for swapchain backbuffer RTV and texture RTVs. |
| `pfnClearDepthStencilView` | REQUIRED | Used by `d3d11_depth_test_sanity`. |
| `pfnDraw` | REQUIRED | Used by multiple Win7 tests. |
| `pfnDrawIndexed` | REQUIRED | Used by `d3d11_texture_sampling_sanity`; many samples use it. |
| `pfnDrawInstanced` / `pfnDrawIndexedInstanced` / `pfnDrawAuto` | OPTIONAL | Stub with `SetErrorCb(E_NOTIMPL)` if called with non-zero counts. |
| `pfnDrawInstancedIndirect` / `pfnDrawIndexedInstancedIndirect` | OPTIONAL | Stub. |

### 5.5 Resource update/copy/resolve

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnUpdateSubresourceUP` | REQUIRED | Must accept user-memory uploads. `d3d11_update_subresource_texture_sanity` specifically checks: padded `RowPitch` (not tightly packed) and non-NULL `D3D11_BOX` partial updates for both textures and buffers. |
| `pfnCopyResource` | REQUIRED | Used by multiple Win7 tests for staging readback. |
| `pfnCopySubresourceRegion` | REQUIRED-BUT-STUBBABLE | Many real apps use subresource copies; implement soon. |
| `pfnResolveSubresource` | OPTIONAL | Stub until MSAA is implemented. |
| `pfnGenerateMips` | OPTIONAL | Stub until autogen mips are implemented. |

### 5.5.1 Queries / predication (often unused for bring-up)

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnBegin` / `pfnEnd` | OPTIONAL | If unimplemented, use `SetErrorCb(E_NOTIMPL)` on non-null queries. |
| `pfnSetPredication` | OPTIONAL | Stub with `SetErrorCb(E_NOTIMPL)` until queries/predication are implemented. |

### 5.6 Map/Unmap (dynamic updates + staging readback)

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnMap` | REQUIRED | Must support at least: `D3D11_MAP_READ` on STAGING textures and `D3D11_MAP_WRITE_DISCARD` for dynamic buffer uploads. `d3d11_map_dynamic_buffer_sanity` also exercises `D3D11_MAP_WRITE_NO_OVERWRITE` and DISCARD renaming hazards (in-flight copies must see the pre-discard contents). |
| `pfnUnmap` | REQUIRED | Must commit writes / release mappings. |

Special note for Win7 bring-up:

* The tests call `Map(..., D3D11_MAP_READ, 0, ...)` (no `DO_NOT_WAIT`). It is acceptable for Map to block waiting for the copy to complete, but it must be bounded and backed by a real fence (avoid TDRs).
* If you implement a “submit-on-Flush” backend, make sure `CopyResource + Flush + Map(READ)` results in completed readback data.
* Some D3D11 DDI interface versions expose additional map-style entrypoints (for example, staging-specific map helpers). If your chosen `D3D11DDI_DEVICECONTEXTFUNCS` struct has them, wire them to the same underlying map/unmap implementation and keep them non-null.
* For the definitive Win7 Map/Unmap + `LockCb`/`UnlockCb` contract, see:
  * `docs/graphics/win7-d3d11-map-unmap.md`
* For the Win7/WDDM 1.1 callback-level contract (how a UMD blocks/polls on fences for `Map(READ)`), see:
  * `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

### 5.7 Flush / submission

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnFlush` | REQUIRED | Must actually submit queued work so fences advance and readbacks complete. `ID3D11DeviceContext::Flush` is `void` → failures must use `SetErrorCb`. |

### 5.8 Present callouts (Win7 DXGI)

Present/RotateResourceIdentities may be surfaced on either the device or immediate context table (interface-version dependent).
On Win7, DXGI uses `D3D10DDIARG_PRESENT` (DXGI 1.1) even for D3D11 devices:

| Field | Status | Notes / stub guidance |
|---|---|---|
| `pfnPresent` | REQUIRED | `HRESULT`: must succeed for windowed swapchains. Signature is table-dependent, but DXGI always passes a `D3D10DDIARG_PRESENT` (DXGI 1.1). Consider implicit flush/submit so rendering to the backbuffer is visible by the time present returns. |
| `pfnRotateResourceIdentities` | REQUIRED | `void`: must rotate the “identity” of backbuffer resources after present. `d3d11_swapchain_rotate_sanity` validates `BufferCount=2` rotation. |

Present also interacts with context submission:

* DXGI typically expects rendering to the backbuffer to be **submitted** before present returns.
* A common minimal policy is: `pfnPresent` performs an implicit `Flush` / submit of outstanding work.
* DXGI swapchains also use `pfnRotateResourceIdentities` to rotate backbuffer identities after present.

---

## 6) Mapping: Win7 guest tests → DDI entrypoints exercised

The repo’s Win7 D3D11 tests are good coverage targets because they exercise device creation + caps queries, basic rendering, staging readback, dynamic updates, and swapchain present/rotation.

Tests:

* `drivers/aerogpu/tests/win7/d3d11_caps_smoke`
* `drivers/aerogpu/tests/win7/d3d11_triangle`
* `drivers/aerogpu/tests/win7/readback_sanity`
* `drivers/aerogpu/tests/win7/d3d11_texture_sampling_sanity`
* `drivers/aerogpu/tests/win7/d3d11_dynamic_constant_buffer_sanity`
* `drivers/aerogpu/tests/win7/d3d11_depth_test_sanity`
* `drivers/aerogpu/tests/win7/d3d11_update_subresource_texture_sanity`
* `drivers/aerogpu/tests/win7/d3d11_map_dynamic_buffer_sanity`
* `drivers/aerogpu/tests/win7/d3d11_rs_om_state_sanity`
* `drivers/aerogpu/tests/win7/d3d11_swapchain_rotate_sanity`
* `drivers/aerogpu/tests/win7/d3d11_geometry_shader_smoke`

### 6.1 `d3d11_triangle` (D3D11CreateDeviceAndSwapChain + Present)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `D3D11CreateDeviceAndSwapChain` | `OpenAdapter11` → adapter `pfnGetCaps` (several types) → `pfnCalcPrivateDeviceSize` → `pfnCreateDevice` (fills `D3D11DDI_DEVICEFUNCS` + `D3D11DDI_DEVICECONTEXTFUNCS`). |
| `IDXGISwapChain::GetBuffer` | runtime bookkeeping; usually no direct DDI call, but the backbuffer is a DDI resource created during swapchain creation. |
| `ID3D11Device::CreateRenderTargetView` | `pfnCalcPrivateRenderTargetViewSize` → `pfnCreateRenderTargetView`. |
| `ID3D11DeviceContext::OMSetRenderTargets` | context `pfnSetRenderTargets`. |
| `ID3D11DeviceContext::RSSetViewports` | context `pfnSetViewports`. |
| `CreateVertexShader` / `CreatePixelShader` | `pfnCalcPrivate*ShaderSize` → `pfnCreate*Shader`. |
| `CreateInputLayout` | `pfnCalcPrivateElementLayoutSize` → `pfnCreateElementLayout`. |
| `CreateBuffer` (VB) | `pfnCalcPrivateResourceSize` → `pfnCreateResource` (must support initial data upload via `D3D11_SUBRESOURCE_DATA`). |
| `IASetInputLayout` / `IASetPrimitiveTopology` / `IASetVertexBuffers` | context `pfnIaSetInputLayout` / `pfnIaSetTopology` / `pfnIaSetVertexBuffers`. |
| `VSSetShader` / `PSSetShader` | context `pfnVsSetShader` / `pfnPsSetShader`. |
| `ClearRenderTargetView` | context `pfnClearRenderTargetView`. |
| `Draw` | context `pfnDraw`. |
| `CreateTexture2D` (staging) | `pfnCalcPrivateResourceSize` → `pfnCreateResource` (must support `D3D11_USAGE_STAGING` + `CPU_ACCESS_READ`). |
| `CopyResource` | context `pfnCopyResource`. |
| `Flush` | context `pfnFlush`. |
| `Map` / `Unmap` | context `pfnMap` / `pfnUnmap`. |
| `IDXGISwapChain::Present` | `pfnPresent` + `pfnRotateResourceIdentities` (table depends on interface version; DXGI uses `D3D10DDIARG_PRESENT`). |

### 6.2 `readback_sanity` (render-to-texture + staging readback; no Present)

Same as above except:

* No swapchain creation / `pfnPresent` / `pfnRotateResourceIdentities` required.
* Render target is a regular `Texture2D` created via `pfnCreateResource` + `pfnCreateRenderTargetView` (BGRA, `D3D11_BIND_RENDER_TARGET`).

### 6.3 `d3d11_texture_sampling_sanity` (SRV + sampler + indexed draw + readback)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateTexture2D` (src texture) | device `pfnCalcPrivateResourceSize` → `pfnCreateResource`. |
| `UpdateSubresource` | context `pfnUpdateSubresourceUP`. |
| `CreateShaderResourceView` | device `pfnCalcPrivateShaderResourceViewSize` → `pfnCreateShaderResourceView`. |
| `CreateSamplerState` | device `pfnCalcPrivateSamplerSize` → `pfnCreateSampler`. |
| `IASetIndexBuffer` | context `pfnIaSetIndexBuffer`. |
| `VSSetShaderResources` / `VSSetSamplers` | context `pfnVsSetShaderResources` / `pfnVsSetSamplers`. |
| `PSSetShaderResources` / `PSSetSamplers` | context `pfnPsSetShaderResources` / `pfnPsSetSamplers`. |
| `DrawIndexed` | context `pfnDrawIndexed`. |
| `CopyResource` + staging `Map(READ)` | context `pfnCopyResource` + `pfnMap` / `pfnUnmap`. |

### 6.4 `d3d11_dynamic_constant_buffer_sanity` (dynamic CB bind + draw + readback)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateBuffer` (dynamic constant buffer) | device `pfnCalcPrivateResourceSize` → `pfnCreateResource`. |
| `Map(WRITE_DISCARD)` | context `pfnMap` / `pfnUnmap` (the runtime may route dynamic CB discard through specialized DDIs depending on interface version). |
| `PSSetConstantBuffers` | context `pfnPsSetConstantBuffers`. |
| `Draw` | context `pfnDraw`. |

### 6.5 `d3d11_depth_test_sanity` (DSV + depth state + clear + draw + readback)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateTexture2D` (depth) | device `pfnCalcPrivateResourceSize` → `pfnCreateResource`. |
| `CreateDepthStencilView` | device `pfnCalcPrivateDepthStencilViewSize` → `pfnCreateDepthStencilView`. |
| `CreateDepthStencilState` | device `pfnCalcPrivateDepthStencilStateSize` → `pfnCreateDepthStencilState`. |
| `OMSetDepthStencilState` | context `pfnSetDepthStencilState`. |
| `ClearDepthStencilView` | context `pfnClearDepthStencilView`. |
| `Draw` | context `pfnDraw`. |

### 6.6 `d3d11_update_subresource_texture_sanity` (UpdateSubresource on textures + boxed updates)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `UpdateSubresource(tex, 0, NULL, pData, RowPitch, ...)` | context `pfnUpdateSubresourceUP` (must respect the caller-provided `RowPitch`, which may be larger than `Width*BytesPerPixel`). |
| `UpdateSubresource(tex, 0, &D3D11_BOX, ...)` | context `pfnUpdateSubresourceUP` (partial/boxed update). |
| `UpdateSubresource(buffer, 0, &D3D11_BOX, ...)` | context `pfnUpdateSubresourceUP` (for buffers, the box encodes byte offsets in `left/right`). |
| Readback verification | context `pfnCopyResource` + `pfnFlush` + `pfnMap`/`pfnUnmap`. |

### 6.7 `d3d11_map_dynamic_buffer_sanity` (Map DISCARD/NO_OVERWRITE correctness)

This test is intentionally “mean”: it checks that `Map(WRITE_DISCARD)` renames/allocates fresh storage so that
GPU work queued before the discard still sees the **old** contents.

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateBuffer(DYNAMIC, CPU_WRITE)` (VB/IB/CB) | device `pfnCalcPrivateResourceSize` → `pfnCreateResource`. |
| Bind dynamic buffers (`IASetVertexBuffers`, `IASetIndexBuffer`, `VSSetConstantBuffers`) | context `pfnIaSetVertexBuffers` / `pfnIaSetIndexBuffer` / `pfnVsSetConstantBuffers`. |
| `Map(WRITE_DISCARD)` / `Map(WRITE_NO_OVERWRITE)` | context `pfnMap` / `pfnUnmap` (some interface versions route these through specialized dynamic-map DDIs; if your struct has them, keep them non-null and implement equivalent semantics). |
| `CopyResource(staging, dynamic_buf)` | context `pfnCopyResource` (copies must read the correct “version” of a renamed/discarded buffer). |
| Readback verification | context `pfnFlush` + `pfnMap(READ)` on staging buffers. |

### 6.8 `d3d11_rs_om_state_sanity` (scissor + cull mode + blending)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateRasterizerState` | device `pfnCalcPrivateRasterizerStateSize` → `pfnCreateRasterizerState`. |
| `RSSetState` / `RSSetScissorRects` | context `pfnSetRasterizerState` / `pfnSetScissorRects`. |
| `CreateBlendState` | device `pfnCalcPrivateBlendStateSize` → `pfnCreateBlendState`. |
| `OMSetBlendState` | context `pfnSetBlendState`. |
| Draw + readback | context `pfnDraw` + `pfnCopyResource` + `pfnFlush` + `pfnMap`/`pfnUnmap`. |

### 6.9 `d3d11_swapchain_rotate_sanity` (RotateResourceIdentities, BufferCount=2)

This test validates that after `Present`, swapchain buffer identities rotate (buffer0 becomes buffer1, etc).

| API call in test | DDI entrypoints you should expect |
|---|---|
| `IDXGISwapChain::Present` | context `pfnPresent` + `pfnRotateResourceIdentities` (rotation is what the test is checking). |
| Readback of both swapchain buffers | context `pfnCopyResource` + `pfnFlush` + `pfnMap`/`pfnUnmap`. |

### 6.10 `d3d11_geometry_shader_smoke` (GS creation + bind + draw)

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CreateGeometryShader` | device `pfnCalcPrivateGeometryShaderSize` → `pfnCreateGeometryShader`. |
| `GSSetShader` | context `pfnGsSetShader`. |
| Draw + readback | context `pfnDraw` + `pfnCopyResource` + `pfnFlush` + `pfnMap`/`pfnUnmap`. |

### 6.11 `d3d11_caps_smoke` (caps queries: feature support, formats, MSAA)

This test is primarily about making `pfnGetCaps` return conservative-but-consistent answers:

| API call in test | DDI entrypoints you should expect |
|---|---|
| `CheckFeatureSupport(D3D11_FEATURE_THREADING/DOUBLES/D3D10_X_HARDWARE_OPTIONS/D3D11_OPTIONS)` | adapter `pfnGetCaps` with `D3D11DDICAPS_TYPE_*` matching the queried feature. |
| `CheckFormatSupport(...)` | adapter `pfnGetCaps(D3D11DDICAPS_TYPE_FORMAT)` for the probed `DXGI_FORMAT`s (BGRA/RGBA/depth/index). |
| `CheckMultisampleQualityLevels(format, 1, ...)` | adapter `pfnGetCaps(D3D11DDICAPS_TYPE_MULTISAMPLE_QUALITY_LEVELS)` should report at least 1 quality level for `SampleCount==1` on supported formats. |

---

## 7) Practical bring-up ordering (what to implement first)

If your goal is “device creates and the basic Win7 guest tests pass”:

1. **Adapter bring-up**
   * `OpenAdapter11` export
   * `D3D11DDI_ADAPTERFUNCS::{pfnGetCaps,pfnCalcPrivateDeviceSize,pfnCreateDevice,pfnCloseAdapter}`
2. **Device funcs (creation)**
   * Resource + RTV/SRV/DSV + sampler + depth-stencil state + shader + input layout creation (triads)
3. **Immediate context**
   * `pfnSetRenderTargets`, `pfnSetViewports`, IA bindings (incl. index buffer), VS/PS binds
   * PS resource/sampler/CB binding, depth-stencil state, clears, `pfnDraw`/`pfnDrawIndexed`
4. **Upload + readback path**
   * `pfnUpdateSubresourceUP`, `pfnCopyResource`, `pfnFlush`, `pfnMap`/`pfnUnmap` for STAGING read
5. **DXGI present** (swapchain tests)
   * context `pfnPresent` + `pfnRotateResourceIdentities`

Everything else can initially be “present-but-stubbed”, as long as it fails cleanly and never dereferences invalid handles.

---

## Appendix: common early crash sources (and what the checklist prevents)

If you’re bringing up a new UMD and seeing immediate access violations in `d3d11.dll` / `dxgi.dll`, the root cause is often one of:

* **NULL DDI function pointer** in `D3D11DDI_DEVICEFUNCS` / `D3D11DDI_DEVICECONTEXTFUNCS`.
  * Fix: stub-fill all fields (see §0 “Non-null discipline”).
* **Wrong calling convention / prototype mismatch** (stack imbalance).
  * Fix: make sure you compile with the exact `PFND3D11DDI_*` typedefs from `d3d11umddi.h` and use `__stdcall`/`APIENTRY`.
* **`pfnGetCaps` writing past `DataSize`**, or assuming `pData` is always non-null.
  * Fix: treat `pData == NULL` as a size query when applicable, validate `DataSize` before writing, and be conservative on unknown types.
* **Returning “supported” in caps but failing creation later** (leads to confusing app behavior and sometimes runtime asserts).
  * Fix: keep caps truthful; only advertise what you implement end-to-end.

This doc’s core recommendation (“fill everything with safe stubs first; then incrementally implement”) is specifically to avoid the first class of bring-up crashes.
