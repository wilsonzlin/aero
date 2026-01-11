# Win7 (WDDM 1.1) D3D10/D3D11 UMD resource backing allocations + Map/Unmap (WDK 7.1 reference)

This document is a **symbol-accurate (WDK 7.1)** reference for the Win7/WDDM 1.1 D3D10 and D3D11 UMD contracts around:

* **resource backing allocation** (`*CreateResource` → `pfnAllocateCb` / `pfnDeallocateCb`), and
* **CPU mapping** (`Map`/`Unmap` → `pfnLockCb` / `pfnUnlockCb`).

It deliberately does **not** cover the full D3D10/11 DDI surface (pipeline state, shaders, etc). It exists so future bring-up/debugging work can be done without guesswork about which callback is responsible for what on Win7.

Header set assumed (Win7-era UMD DDIs):

* `d3d10umddi.h`
* `d3d11umddi.h`
* shared runtime callback layer: `d3dumddi.h`
* swapchain primary/backbuffer identification: `dxgiddi.h`

Related deeper dives (still useful, but this doc is the “one place” for alloc+map):

* `docs/graphics/win7-d3d10-11-umd-allocations.md` (more detailed CreateResource allocation notes)
* `docs/graphics/win7-d3d11-map-unmap.md` (more detailed D3D11 Map/Unmap behavior + validation rules)
* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md` (exact Win7 callback symbol names; useful context for submission + fence waits that interact with `Map(READ)` paths)

---

## 1) What is shared between D3D10 and D3D11 on Win7 (and what is not)

On Win7/WDDM 1.1, D3D10 and D3D11 UMDs both sit on top of the same WDDM allocation + mapping callbacks declared in `d3dumddi.h`.

### 1.1 Shared (D3D10 and D3D11)

**Runtime callback table (UMD calls runtime/OS):**

* `D3DDDI_DEVICECALLBACKS` (provided at device creation time, usually in `*_ARG_CREATEDEVICE::pUMCallbacks`)

**Resource allocation callbacks (UMD → runtime):**

* `D3DDDI_DEVICECALLBACKS::pfnAllocateCb` using `D3DDDICB_ALLOCATE`
* `D3DDDI_DEVICECALLBACKS::pfnDeallocateCb` using `D3DDDICB_DEALLOCATE`

**Mapping callbacks (UMD → runtime):**

* `D3DDDI_DEVICECALLBACKS::pfnLockCb` using `D3DDDICB_LOCK`
* `D3DDDI_DEVICECALLBACKS::pfnUnlockCb` using `D3DDDICB_UNLOCK`

**Per-allocation descriptor (array element) used by `pfnAllocateCb`:**

* `D3DDDI_ALLOCATIONINFO` (aliased by the D3D10/11 headers as `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO`)

> Note: many people casually refer to the array element as “allocation info for `D3DDDICB_ALLOCATE`”, but the concrete struct name in Win7-era headers is `D3DDDI_ALLOCATIONINFO` (not a `D3DDDICB_*` type).

### 1.2 D3D10-only vs D3D11-only

**Resource creation entrypoints (runtime calls UMD):**

* D3D10 resource creation is on the device vtable:
  * `D3D10DDI_DEVICEFUNCS::pfnCreateResource` (type: `PFND3D10DDI_CREATERESOURCE`)
* D3D11 resource creation is also on the device vtable:
  * `D3D11DDI_DEVICEFUNCS::pfnCreateResource` (type: `PFND3D11DDI_CREATERESOURCE`)

**Map/Unmap entrypoints (runtime calls UMD):**

* D3D10 has no “device context” split; Map/Unmap live on the device function table:
  * `D3D10DDI_DEVICEFUNCS::pfnMap` / `pfnUnmap` (types: `PFND3D10DDI_MAP` / `PFND3D10DDI_UNMAP`)
* D3D11 splits **device** vs **immediate context**:
  * `Map`/`Unmap` are called through the immediate context vtable:
    * `D3D11DDI_DEVICECONTEXTFUNCS::pfnMap` / `pfnUnmap` (types: `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP`)
  * Win7 D3D11 runtimes may also use specialized context entrypoints that must behave identically:
    * `D3D11DDI_DEVICECONTEXTFUNCS::pfnStagingResourceMap` / `pfnStagingResourceUnmap`
    * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferMapDiscard` / `pfnDynamicIABufferMapNoOverwrite` / `pfnDynamicIABufferUnmap`
    * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicConstantBufferMapDiscard` / `pfnDynamicConstantBufferUnmap`

### 1.3 D3D10/11 “wrapper” callback tables (runtime → UMD)

At device creation time, the runtime also provides D3D10/11-specific callback tables:

* D3D10: `D3D10DDI_DEVICECALLBACKS` (via `D3D10DDIARG_CREATEDEVICE::pCallbacks`)
* D3D11: `D3D11DDI_DEVICECALLBACKS` (via `D3D11DDIARG_CREATEDEVICE::{pCallbacks|pDeviceCallbacks}`)

These contain at least `pfnSetErrorCb` (for error reporting from `void` DDIs). Some header revisions also surface `pfnLockCb`/`pfnUnlockCb` here, but the **shared** `D3DDDI_DEVICECALLBACKS` (`pUMCallbacks`) is the canonical table for `pfnAllocateCb`/`pfnDeallocateCb`/`pfnLockCb`/`pfnUnlockCb`.

---

## 2) Where allocation + mapping callbacks come from (Win7 CreateDevice wiring)

Both D3D10 and D3D11 follow the same pattern:

1. The runtime calls your adapter’s `pfnCreateDevice(...)`.
2. The runtime passes `*_ARG_CREATEDEVICE` which contains:
   * the runtime “RT device” handle (`hRTDevice`), and
   * pointers to callback tables (including `pUMCallbacks`).
3. The UMD stores `hRTDevice` and callback table pointers in its device object.

The relevant `*_ARG_CREATEDEVICE` fields (WDK 7.1 names):

* D3D10: `D3D10DDIARG_CREATEDEVICE`
  * `D3D10DDI_HRTDEVICE hRTDevice`
  * `const D3D10DDI_DEVICECALLBACKS* pCallbacks`
  * `const D3DDDI_DEVICECALLBACKS* pUMCallbacks`
* D3D11: `D3D11DDIARG_CREATEDEVICE`
  * `D3D11DDI_HRTDEVICE hRTDevice`
  * `const D3D11DDI_DEVICECALLBACKS* pCallbacks` (some headers name this `pDeviceCallbacks`)
  * `const D3DDDI_DEVICECALLBACKS* pUMCallbacks`
  * output tables:
    * `D3D11DDI_DEVICEFUNCS* pDeviceFuncs`
    * `D3D11DDI_DEVICECONTEXTFUNCS* pDeviceContextFuncs`

**Rule:** all uses of `pfnAllocateCb`/`pfnDeallocateCb`/`pfnLockCb`/`pfnUnlockCb` take the runtime device handle first (the `hRTDevice` from CreateDevice), not your `D3D*DDI_HDEVICE`.

---

## 3) Resource backing allocations: `*CreateResource` → `pfnAllocateCb`

### 3.1 The entrypoints that trigger resource allocations

The only DDIs that create the **WDDM allocations backing a resource** are:

* D3D10: `PFND3D10DDI_CREATERESOURCE` (`D3D10DDI_DEVICEFUNCS::pfnCreateResource`)
* D3D11: `PFND3D11DDI_CREATERESOURCE` (`D3D11DDI_DEVICEFUNCS::pfnCreateResource`)

Both follow the same allocation model:

1. The runtime passes the UMD an output array:
   * `pCreateResource->NumAllocations`
   * `pCreateResource->pAllocationInfo` (array of `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO`, i.e. `D3DDDI_ALLOCATIONINFO`)
2. The UMD fills each element’s size/flags.
3. The UMD calls the runtime callback `pfnAllocateCb` with a `D3DDDICB_ALLOCATE` that points at that same array.
4. The runtime returns kernel allocation handles (`D3DKMT_HANDLE`) into each `D3DDDI_ALLOCATIONINFO::hAllocation`, plus `D3DDDICB_ALLOCATE::hKMResource`.

### 3.2 `pfnAllocateCb` and the kernel allocation path (what actually happens)

`pfnAllocateCb` is a runtime/OS callback. The UMD is not creating “malloc’d” memory; it is requesting WDDM allocations.

On success, the allocation creation goes roughly:

```
UMD CreateResource
  -> D3DDDI_DEVICECALLBACKS::pfnAllocateCb(hRTDevice, &D3DDDICB_ALLOCATE)
    -> dxgkrnl/VidMm
      -> KMD: DxgkDdiCreateAllocation (per allocation)
```

So “resource backing allocations” ultimately exist as kernel objects tracked by dxgkrnl, and the UMD’s job is to preserve the returned handles and use them for later operations (notably `pfnLockCb`).

### 3.3 `D3DDDICB_ALLOCATE` + `D3DDDI_ALLOCATIONINFO` (fields you must populate)

#### `D3DDDICB_ALLOCATE` (resource allocation use)

The same struct name is also used for DMA buffer allocation; the relevant fields for **resource creation** are:

* `D3DDDI_HRESOURCE hResource`
  * Set this to the runtime resource handle passed to `CreateResource` (commonly named `hRTResource` in the DDI signature).
* `UINT NumAllocations`
  * Copy from `pCreateResource->NumAllocations`.
* `D3DDDI_ALLOCATIONINFO* pAllocationInfo`
  * Point at `pCreateResource->pAllocationInfo`.
* `D3DDDICB_ALLOCATEFLAGS Flags`
  * Resource-level flags; the bring-up critical bit is `Flags.Primary` for DXGI primaries/backbuffers.

Outputs (store them):

* `D3DDDI_HKMRESOURCE hKMResource`
* For each allocation entry: `pAllocationInfo[i].hAllocation`
* (Shared resources only) `HANDLE hSection`

#### `D3DDDI_ALLOCATIONINFO` (per-allocation descriptor)

For each element in `pAllocationInfo[0..NumAllocations)` the UMD must fill:

* `UINT64 Size`
* `UINT64 Alignment` (0 is acceptable for MVP)
* `D3DDDI_ALLOCATIONINFOFLAGS Flags`
  * Minimal set commonly needed on Win7:
    * `Primary` (for swapchain buffers/primaries)
    * `RenderTarget` (for RTV-capable allocations)
    * `CpuVisible` (required for staging/dynamic CPU-mapped allocations)
* Optional KMD-private blob:
  * `VOID* pPrivateDriverData`
  * `UINT PrivateDriverDataSize`

Output per element:

* `D3DKMT_HANDLE hAllocation`

### 3.4 `pfnDeallocateCb` and `D3DDDICB_DEALLOCATE` (DestroyResource)

Destroying a resource requires explicitly freeing its WDDM allocations:

* callback: `D3DDDI_DEVICECALLBACKS::pfnDeallocateCb`
* struct: `D3DDDICB_DEALLOCATE`

Minimal fields:

* `D3DDDI_HRESOURCE hResource` (runtime resource handle)
* `D3DDDI_HKMRESOURCE hKMResource`
* `UINT NumAllocations`
* `const D3DKMT_HANDLE* phAllocations` (array of allocation handles to destroy)

---

## 4) Minimal allocation/flag rules for FL10_0 bring-up (buffers + Texture2D)

This is the pragmatic “don’t get stuck” rule set for bringing up a minimal FL10_0 D3D10/11 UMD on Win7.

### 4.1 Buffer allocations

| API usage | Typical CPU access | Allocation flags | Notes |
|---|---|---|---|
| DEFAULT (`D3D11_USAGE_DEFAULT` / `D3D10_USAGE_DEFAULT`) | none (`CPUAccessFlags == 0`) | usually **not** `CpuVisible` | Not mappable. Update via `UpdateSubresource*`/`Copy*`. |
| DYNAMIC (`D3D11_USAGE_DYNAMIC` / `D3D10_USAGE_DYNAMIC`) | write (`D3D11_CPU_ACCESS_WRITE` / `D3D10_CPU_ACCESS_WRITE`) | `CpuVisible=1` | Map is used for frequent uploads; `WRITE_DISCARD`/`NO_OVERWRITE` map types appear. |
| STAGING (`D3D11_USAGE_STAGING` / `D3D10_USAGE_STAGING`) | read and/or write (`D3D11_CPU_ACCESS_READ`/`WRITE`, `D3D10_CPU_ACCESS_READ`/`WRITE`) | `CpuVisible=1` (required) | Used for readback and upload staging. |

CPU visibility rule of thumb:

* If `pCreateResource->CPUAccessFlags` contains any CPU access bits (for example `D3D11_CPU_ACCESS_READ` / `D3D11_CPU_ACCESS_WRITE`), set `D3DDDI_ALLOCATIONINFOFLAGS::CpuVisible = 1` for the allocation(s). If you do not, `pfnLockCb` is expected to fail for Map/Unmap paths.

Size/align:

* `Size = ByteWidth`
* Aligning up (e.g. 256 bytes) is typical but not required for correctness if your KMD/translator can handle it.

### 4.2 Texture2D allocations (DEFAULT render target + STAGING readback)

For bring-up, assume:

* `MipLevels == 1`
* `ArraySize == 1`
* `SampleDesc.Count == 1` (no MSAA)

#### DEFAULT Texture2D used as render target

Allocation flags:

* `RenderTarget=1` if `BindFlags` contains `D3D11_BIND_RENDER_TARGET` / `D3D10_BIND_RENDER_TARGET`
* `CpuVisible=0` unless CPU access is explicitly requested (rare for DEFAULT)

Sizing:

* Choose a linear layout and an explicit row pitch:
  * `RowPitch = Align(Width * bytesPerPixel(Format), 256)` (256 is a common minimum; your KMD decides)
  * `Size = RowPitch * Height`

#### STAGING Texture2D used for readback

Allocation flags:

* `CpuVisible=1` (**required**, because `Map(READ)` uses `pfnLockCb`)
* Bind flags are typically 0 for staging.

Sizing:

* same as DEFAULT (linear, with a stable row pitch)

### 4.3 DXGI primaries/backbuffers (swapchain buffers)

Win7 DXGI identifies swapchain buffers/primaries via a “primary desc” pointer in the CreateResource argument:

* `const DXGI_DDI_PRIMARY_DESC* pPrimaryDesc` (from `dxgiddi.h`)

Rule:

* If `pPrimaryDesc != NULL`, treat the resource as a **primary/backbuffer** and set:
  * `D3DDDICB_ALLOCATEFLAGS::Primary = 1` (resource-level)
  * `D3DDDI_ALLOCATIONINFOFLAGS::Primary = 1` (per allocation)
  * usually also `D3DDDI_ALLOCATIONINFOFLAGS::RenderTarget = 1`

---

## 5) Map/Unmap: UMD entrypoints → `pfnLockCb` / `pfnUnlockCb`

### 5.1 The “one rule”: Map uses the runtime lock callbacks

On Win7/WDDM 1.1, UMD `Map`/`Unmap` is not “return a pointer to driver memory”.

The Win7 contract is:

* `Map` is implemented by calling the runtime callback `pfnLockCb` with a `D3DDDICB_LOCK`.
* `Unmap` is implemented by calling `pfnUnlockCb` with a `D3DDDICB_UNLOCK`.

The runtime (via dxgkrnl + KMD `DxgkDdiLock`/`DxgkDdiUnlock`) owns:

* waiting for GPU use to finish (unless DO_NOT_WAIT),
* creating/stabilizing a CPU virtual mapping, and
* returning the pointer + pitch metadata.

### 5.2 `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK` (required fields)

#### `D3DDDICB_LOCK`

Inputs:

* `D3DKMT_HANDLE hAllocation`
  * Must be the allocation handle returned in `D3DDDI_ALLOCATIONINFO::hAllocation` at CreateResource time.
* `UINT SubResourceIndex`
  * Subresource index (0 for buffers; `D3D11CalcSubresource` encoding for textures).
* `D3DDDICB_LOCKFLAGS Flags`
  * Read/write/discard/no-overwrite/do-not-wait.

Outputs:

* `VOID* pData` (CPU pointer)
* `UINT Pitch` (row pitch)
* `UINT SlicePitch` (depth/slice pitch)

#### `D3DDDICB_UNLOCK`

Inputs:

* `D3DKMT_HANDLE hAllocation`
* `UINT SubResourceIndex`

### 5.3 MapType/flags translation (D3D11 and D3D10)

#### 5.3.1 D3D11: `D3D11_MAP_*` → `D3DDDICB_LOCKFLAGS`

| `D3D11_MAP` | `D3DDDICB_LOCKFLAGS::ReadOnly` | `...::WriteOnly` | `...::Discard` | `...::NoOverwrite` |
|---|---:|---:|---:|---:|
| `D3D11_MAP_READ` | 1 | 0 | 0 | 0 |
| `D3D11_MAP_WRITE` | 0 | 1 | 0 | 0 |
| `D3D11_MAP_READ_WRITE` | 0 | 0 | 0 | 0 |
| `D3D11_MAP_WRITE_DISCARD` | 0 | 1 | 1 | 0 |
| `D3D11_MAP_WRITE_NO_OVERWRITE` | 0 | 1 | 0 | 1 |

Map flags:

* `D3D11_MAP_FLAG_DO_NOT_WAIT` → `D3DDDICB_LOCKFLAGS::DoNotWait = 1`

Required return behavior:

* If `DO_NOT_WAIT` is set and the allocation is still busy, return `DXGI_ERROR_WAS_STILL_DRAWING`.

#### 5.3.2 D3D10: `D3D10_DDI_MAP` / `D3D10_DDI_MAPFLAGS` → `D3DDDICB_LOCKFLAGS`

The D3D10 DDI uses the same map-type values (named `D3D10_DDI_MAP` in `d3d10umddi.h`) and maps to lock flags identically:

| `D3D10_DDI_MAP` | `D3DDDICB_LOCKFLAGS::ReadOnly` | `...::WriteOnly` | `...::Discard` | `...::NoOverwrite` |
|---|---:|---:|---:|---:|
| `D3D10_DDI_MAP_READ` | 1 | 0 | 0 | 0 |
| `D3D10_DDI_MAP_WRITE` | 0 | 1 | 0 | 0 |
| `D3D10_DDI_MAP_READ_WRITE` | 0 | 0 | 0 | 0 |
| `D3D10_DDI_MAP_WRITE_DISCARD` | 0 | 1 | 1 | 0 |
| `D3D10_DDI_MAP_WRITE_NO_OVERWRITE` | 0 | 1 | 0 | 1 |

Map flags:

* `D3D10_DDI_MAPFLAGS::DoNotWait` → `D3DDDICB_LOCKFLAGS::DoNotWait = 1`

### 5.4 Pitch rules (RowPitch / SlicePitch)

For textures, the runtime expects the UMD to return correct pitch values back to the API caller:

* D3D11: fill `D3D11DDI_MAPPED_SUBRESOURCE::{pData, RowPitch, DepthPitch}`
* D3D10: fill the D3D10 mapped-subresource output (same conceptual fields)

**Where pitch comes from on Win7:**

* The `pfnLockCb` call returns both the CPU pointer and the pitch values:
  * `D3DDDICB_LOCK::pData`
  * `D3DDDICB_LOCK::Pitch`
  * `D3DDDICB_LOCK::SlicePitch`
* The UMD should treat the lock callback output as authoritative and pass it through.

Practical implication for bring-up:

* If you choose a linear layout (recommended), make sure your KMD `DxgkDdiLock` implementation returns pitch values that match the layout you chose at CreateAllocation time (commonly by storing pitch in the allocation private driver data).

---

## 6) Staging readback synchronization (CopyResource + Flush + Map(READ))

The Win7 staging readback pattern used by many apps/tests is:

1. Render into a DEFAULT render target / backbuffer.
2. `CopyResource(staging, renderTarget)` where `staging` is `D3D11_USAGE_STAGING` + `D3D11_CPU_ACCESS_READ`.
3. `Flush()`.
4. `Map(staging, D3D11_MAP_READ, Flags=0)` and read bytes using the returned `RowPitch`.

Required behavior on Win7:

* `Map(READ)` **must block** until the GPU copy completes (unless DO_NOT_WAIT is set).
* When `Map(READ)` returns successfully, the bytes visible at `pData` must be the final results of the copy.

**Which flow provides the implicit sync on Win7:**

* The runtime’s `pfnLockCb` path is the “default” synchronization mechanism.
  * If `D3DDDICB_LOCKFLAGS::DoNotWait` is **not** set, the runtime will block inside `pfnLockCb` until it is safe to expose the allocation to CPU.
  * If `DoNotWait` **is** set and the allocation is still in use, the runtime returns `DXGI_ERROR_WAS_STILL_DRAWING`.

UMD guidance (to avoid deadlocks):

* Before calling a blocking lock for readback, ensure any pending GPU work that produces the readback bytes has actually been submitted (i.e. treat Map(READ) as an implicit flush boundary if needed).
