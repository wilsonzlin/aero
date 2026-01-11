# Win7 (WDDM 1.1) D3D10/D3D11 UMD allocation contract (Win7-era WDK reference)

This document is the **single authoritative, implementation-oriented spec** for how a **Windows 7** (**WDDM 1.1**) **D3D10/D3D11 user-mode display driver (UMD)** allocates and frees memory through the Win7-era D3D UMD contracts.

> Terminology warning: Win7 D3D UMDs have *two* different “allocation” concepts that are easy to conflate:
>
> 1. **Resource backing allocations**: the WDDM allocations that back D3D buffers/textures (created during `CreateResource` via `D3DDDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO` and `pfnAllocateCb`).
> 2. **DMA buffer (command buffer) allocation**: acquiring and releasing the per-submit command buffer + allocation list + patch list (also via `pfnAllocateCb`/`pfnDeallocateCb`, but using a different subset of fields in `D3DDDICB_ALLOCATE` / `D3DDDICB_DEALLOCATE`).
>
> This doc focuses on (1) for `CreateResource`, but also lists (2) because the callback names (`AllocateCb`/`DeallocateCb`) are otherwise confusing.

It is written against Win7-era user-mode DDI headers:

* `d3d10umddi.h`
* `d3d11umddi.h`
* `d3dumddi.h`
* `dxgiddi.h`

The goal is that a developer with the Win7-era DDI headers can implement a correct `CreateResource` allocation flow without chasing definitions across multiple headers.

See also:

* `docs/graphics/win7-d3d11-map-unmap.md` — Win7 `Map`/`Unmap` (`pfnLockCb`/`pfnUnlockCb`), pitch rules, and staging readback synchronization.
* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md` — submission callback contracts (DMA buffer acquisition, render/present, fence waits).
* `docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md` — deprecated redirect to the focused docs above (kept for link compatibility).

## Related AeroGPU code/docs (cross-links)

* UMD D3D10/11 stubs: `drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_umd.cpp` (CreateResource/DestroyResource are currently “no-WDK” stubs).
* KMD allocation behavior: `drivers/aerogpu/kmd/src/aerogpu_kmd.c` (`AeroGpuDdiCreateAllocation` / `AeroGpuDdiDestroyAllocation`).
* WDDM memory model: `docs/graphics/win7-wddm11-aerogpu-driver.md` (§5 “Memory model (minimal)”).
* Header/layout probe tool: `drivers/aerogpu/tools/win7_wdk_probe` (prints `sizeof`/`offsetof` for `D3D*DDIARG_CREATERESOURCE`, `D3DDDI_ALLOCATIONINFO`, and the resource-allocation members of `D3DDDICB_ALLOCATE` / `D3DDDICB_DEALLOCATE`).

---

## 1) Win7 adapter-open + device-create wiring (where allocation callbacks come from)

### 1.1 `OpenAdapter10` / `OpenAdapter11` (Win7 quirk)

Exports (names matter; signatures from Win7-era headers):

* D3D10: `HRESULT APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData)`
* D3D11: `HRESULT APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData)`

**Win7 quirk (WDDM 1.1):** `OpenAdapter11` still uses the **D3D10** open container type `D3D10DDIARG_OPENADAPTER`. The D3D11-specific DDI begins later at `D3D11DDIARG_CREATEDEVICE` / `D3D11DDIARG_GETCAPS`.

`D3D10DDIARG_OPENADAPTER` is the handoff that provides:

* runtime→UMD adapter callback table (store it)
* UMD→runtime adapter function table output slot (fill it)
* interface version negotiation

In practice, the adapter open struct fields you care about are:

* `D3D10DDI_HRTADAPTER hRTAdapter`
  * Runtime-owned adapter handle (passed back to adapter callbacks if needed).
* `const D3D10DDI_ADAPTERCALLBACKS* pAdapterCallbacks`
  * Runtime callback table for adapter-level interactions (store it in your adapter object).
* `D3D10DDI_HADAPTER hAdapter`
  * **Out**: your adapter handle.
* `D3D10DDI_ADAPTERFUNCS* pAdapterFuncs`
  * **Out**: you fill this with your adapter entrypoints (including `pfnCreateDevice`).

### 1.2 `D3D10DDIARG_CREATEDEVICE` / `D3D11DDIARG_CREATEDEVICE` (store the device callbacks)

At adapter `pfnCreateDevice(...)` time, the runtime passes a `*_ARG_CREATEDEVICE` that contains (at minimum):

* an “RT device” handle (`D3D10DDI_HRTDEVICE` / `D3D11DDI_HRTDEVICE`) that must be passed back when invoking runtime callbacks
* pointers to runtime callback tables:
  * D3D10/11 “device callbacks” (for error reporting, and sometimes lock/unlock):
    * D3D10: `D3D10DDI_DEVICECALLBACKS`
    * D3D11: `D3D11DDI_DEVICECALLBACKS`
  * the shared `d3dumddi.h` callback table:
    * `D3DDDI_DEVICECALLBACKS`

**Win7-era header wiring (field names):**

* `pCallbacks` → `D3D10DDI_DEVICECALLBACKS` / `D3D11DDI_DEVICECALLBACKS`
* `pUMCallbacks` → `D3DDDI_DEVICECALLBACKS`

On Win7, `pUMCallbacks` is the table that contains the allocation callbacks used by `CreateResource`:

* `pfnAllocateCb`
* `pfnDeallocateCb`
* (for CPU staging/mapping) `pfnLockCb`, `pfnUnlockCb`

**Rule:** Store both callback table pointer(s) and the RT-device handle in your per-device object. Every `CreateResource`/`DestroyResource` uses them.

#### `*_ARG_CREATEDEVICE` fields that matter for allocations

Both D3D10 and D3D11 create-device structs contain:

* `hRTDevice`
  * Runtime device handle you must pass as the first argument to `pfnAllocateCb` / `pfnDeallocateCb` / `pfnLockCb` / `pfnUnlockCb`.
* `pUMCallbacks`
  * Pointer to the shared callback table (`D3DDDI_DEVICECALLBACKS`) that contains `pfnAllocateCb` / `pfnDeallocateCb` (and also `pfnLockCb` / `pfnUnlockCb`).
* `pCallbacks`
  * Pointer to the D3D10/11 “device callbacks” table (used for error reporting via `pfnSetErrorCb`, and in some headers also provides `pfnLockCb` / `pfnUnlockCb`).

D3D11 additionally wires both the device and immediate-context vtables during `CreateDevice`:

* `pDeviceFuncs` (`D3D11DDI_DEVICEFUNCS`)
* `pDeviceContextFuncs` (`D3D11DDI_DEVICECONTEXTFUNCS`)

---

## 2) The CreateResource allocation sequence (minimal, resource-backing allocations)

### 2.1 Sequence diagram (runtime ⇄ UMD ⇄ dxgkrnl ⇄ KMD)

```
App thread
  |
  |  (API call e.g. ID3D11Device::CreateTexture2D)
  v
D3D10/D3D11 runtime
  |
  | 1) pfnCalcPrivateResourceSize(hDevice, pCreateResource)
  |    -> runtime allocates hResource.pDrvPrivate
  |
  | 2) pfnCreateResource(hDevice, pCreateResource, hResource, hRTResource)
  v
UMD CreateResource
  |
  | 3) Decide allocation layout:
  |      - allocation count strategy
  |      - size/align per allocation
  |      - flags (Primary / RenderTarget / CpuVisible / etc)
  |
  | 4) Fill allocation info array (D3D11DDI_ALLOCATIONINFO / D3DDDI_ALLOCATIONINFO)
  |      - Size, Alignment, Flags, (optional) per-allocation private data
  |
  | 5) Call the runtime allocation callback to create WDDM allocations:
  |      D3DDDICB_ALLOCATE alloc = {...};
  |      alloc.hResource = hRTResource;
  |      alloc.NumAllocations = pCreateResource->NumAllocations;
  |      alloc.pAllocationInfo = pCreateResource->pAllocationInfo;
  |      hr = pUMCallbacks->pfnAllocateCb(hRTDevice, &alloc);
  |
  |    Runtime returns:
  |      - alloc.hKMResource
  |      - pAllocationInfo[i].hAllocation for each allocation
  |
  v
dxgkrnl / VidMm
  |
  | 6) Calls KMD allocation DDIs:
  |      DxgkDdiCreateAllocation / DxgkDdiDestroyAllocation
  |
  v
UMD CreateResource (continues)
  |
  | 7) Store those KM handles in your resource private object
  v
Return to runtime
```

### 2.2 The “one rule” about outputs

The only “real” outputs from `pfnAllocateCb` that the UMD must preserve are:

* `D3DDDICB_ALLOCATE::hKMResource` (resource-level kernel handle)
* `D3DDDI_ALLOCATIONINFO::hAllocation` for every allocation entry
* (if creating a shared resource) `D3DDDICB_ALLOCATE::hSection`

Everything else is driver-owned bookkeeping.

---

## 3) Runtime callback prototypes (Win7-era headers)

These callbacks are provided by the runtime (via the device callback table(s) handed to the UMD at `CreateDevice` time).

### 3.1 Resource backing allocation callbacks: `pfnAllocateCb` / `pfnDeallocateCb`

The D3D10/D3D11 runtime expects the UMD to create and destroy WDDM allocations for resources via the shared `D3DDDI_DEVICECALLBACKS` table (`*_ARG_CREATEDEVICE::pUMCallbacks`):

* `pfnAllocateCb` (create allocation(s) backing a resource)
* `pfnDeallocateCb` (destroy allocation(s) backing a resource)

These calls use:

* `D3DDDICB_ALLOCATE` with `NumAllocations` + `pAllocationInfo` (array of `D3DDDI_ALLOCATIONINFO`)
* `D3DDDICB_DEALLOCATE` with `NumAllocations` + `phAllocations` (array of `D3DKMT_HANDLE`)

### 3.2 DMA buffer (command buffer) allocation callbacks: `pfnAllocateCb` / `pfnDeallocateCb`

On Win7, the same callback names are also used to acquire and release the **DMA/command buffer** that a submission will be encoded into (plus its allocation list / patch list).

This is covered in detail in:

* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

### 3.3 Mapping callbacks (staging + dynamic updates): `pfnLockCb` / `pfnUnlockCb`

For CPU mapping (notably `D3D11_USAGE_STAGING` readback), the UMD uses:

* `pfnLockCb`
* `pfnUnlockCb`

The callback typedefs are declared in `d3dumddi.h`:

```c
typedef HRESULT (APIENTRY *PFND3DDDICB_ALLOCATE)(
    D3D10DDI_HRTDEVICE hDevice,
    D3DDDICB_ALLOCATE* pAllocateData
    );

typedef HRESULT (APIENTRY *PFND3DDDICB_DEALLOCATE)(
    D3D10DDI_HRTDEVICE hDevice,
    const D3DDDICB_DEALLOCATE* pDeallocateData
    );

// Used by Map/Unmap paths (notably D3D11_USAGE_STAGING)
typedef HRESULT (APIENTRY *PFND3DDDICB_LOCK)(D3D10DDI_HRTDEVICE hDevice, D3DDDICB_LOCK* pLockData);
typedef HRESULT (APIENTRY *PFND3DDDICB_UNLOCK)(D3D10DDI_HRTDEVICE hDevice, const D3DDDICB_UNLOCK* pUnlockData);
```

Notes:

* The first parameter is the runtime “RT device” handle passed at create-device time (commonly stored as `hRTDevice` in UMD code).
* `pfnAllocateCb` and `pfnLockCb` are in-out: they write handles/pointers back into the provided structs.

---

## 4) Allocation data structures (field lists)

> Naming: fields below are **verbatim Win7-era header identifiers**; descriptions are the meaning in the UMD allocation contract.

### 4.1 `D3DDDICB_ALLOCATE` (from `d3dumddi.h`)

`D3DDDICB_ALLOCATE` is an in/out struct used with `pfnAllocateCb`.

On Win7/WDDM 1.1 it is used in two distinct places:

1. **CreateResource resource backing allocations** (fill `NumAllocations`/`pAllocationInfo` and get back allocation handles).
2. **Submission DMA buffer allocation** (request command buffer capacity and get back pointers).

#### 4.1.1 Resource allocation fields (CreateResource path)

* `D3DDDI_HRESOURCE hResource`
  * Runtime resource handle being allocated for (the `hRTResource` passed to CreateResource).
* `D3DDDI_HKMRESOURCE hKMResource`
  * **Out**: kernel-mode resource handle returned by the runtime.
* `UINT NumAllocations`
  * Count of entries in `pAllocationInfo`.
* `D3DDDI_ALLOCATIONINFO* pAllocationInfo`
  * Array of per-allocation descriptors:
    * UMD fills `Size`/`Alignment`/`Flags`/`pPrivateDriverData`…
    * runtime returns `hAllocation` in each entry.
* `VOID* pPrivateDriverData`
  * Optional resource-level private data blob for KMD (rare in minimal designs; most metadata is per-allocation).
* `UINT PrivateDriverDataSize`
  * Size of `pPrivateDriverData` (bytes).
* `HANDLE hSection`
  * **Out (shared resources):** section handle for cross-process sharing (`IDXGIResource::GetSharedHandle`).
* `D3DDDICB_ALLOCATEFLAGS Flags`
  * Resource-level allocation flags (notably `Primary` for DXGI primaries/backbuffers).

#### `D3DDDICB_ALLOCATEFLAGS` (swapchain/backbuffer-relevant bits)

For Win7 swapchains, the critical bit is:

* `Primary`
  * Set for scanout-capable allocations (DXGI swapchain backbuffers / primaries).

#### 4.1.2 DMA buffer allocation fields (submission path)

* `D3DKMT_HANDLE hContext`
  * Kernel context handle that the DMA buffer will be associated with.
* `UINT DmaBufferSize` / `UINT CommandBufferSize`
  * Requested command buffer capacity (bytes). Header revisions may use either name.
* `VOID* pDmaBuffer` / `VOID* pCommandBuffer`
  * **Out**: pointer to the DMA/command buffer memory (owned by the runtime/OS).
* `D3DDDI_ALLOCATIONLIST* pAllocationList`
  * **Out**: pointer to the allocation list array for this submission.
* `UINT AllocationListSize`
  * Capacity of `pAllocationList` (in entries).
* `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList`
  * **Out**: pointer to the patch-location list array for this submission.
* `UINT PatchLocationListSize`
  * Capacity of `pPatchLocationList` (in entries).
* `VOID* pDmaBufferPrivateData`
  * **Out (optional)**: pointer to a fixed-size per-submission blob shared with the KMD (size set by `DXGK_DRIVERCAPS::DmaBufferPrivateDataSize`).
* `UINT DmaBufferPrivateDataSize`
  * Capacity of `pDmaBufferPrivateData` (bytes).

### 4.2 `D3DDDICB_DEALLOCATE` (from `d3dumddi.h`)

`D3DDDICB_DEALLOCATE` is used with `pfnDeallocateCb` and mirrors the same dual use:

#### 4.2.1 Resource deallocation fields (DestroyResource path)

* `D3DDDI_HRESOURCE hResource`
* `D3DDDI_HKMRESOURCE hKMResource`
* `UINT NumAllocations`
* `const D3DKMT_HANDLE* phAllocations`
  * Array of allocation handles (`hAllocation`) to free.

#### 4.2.2 DMA buffer release fields (submission path)

* `VOID* pDmaBuffer` / `VOID* pCommandBuffer`
  * The command buffer pointer previously returned by `D3DDDICB_ALLOCATE`.
* `D3DDDI_ALLOCATIONLIST* pAllocationList`
  * The allocation list pointer previously returned by `D3DDDICB_ALLOCATE`.
* `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList`
  * The patch list pointer previously returned by `D3DDDICB_ALLOCATE`.
* `VOID* pDmaBufferPrivateData`
  * The private-data pointer previously returned by `D3DDDICB_ALLOCATE` (if used).

### 4.3 `D3DDDI_ALLOCATIONINFO` (from `d3dumddi.h`)

This is the per-allocation descriptor used for **resource backing allocations**.

The D3D10/11 DDIs reuse this layout via the `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO` typedefs, and `D3DDDICB_ALLOCATE::pAllocationInfo` points at an array of this type.

Fields:

* `D3DKMT_HANDLE hAllocation`
  * **Out**: kernel allocation handle for this allocation entry.
* `UINT64 Size`
  * **In**: allocation size in bytes.
* `UINT64 Alignment`
  * **In**: required alignment (0 = default).
* `D3DDDI_ALLOCATIONINFOFLAGS Flags`
  * Per-allocation flags (CPU visibility, primary, etc).
* `VOID* pPrivateDriverData`
  * Optional per-allocation private data blob for KMD.
* `UINT PrivateDriverDataSize`
  * Size of `pPrivateDriverData` in bytes.

#### `D3DDDI_ALLOCATIONINFOFLAGS` (minimal set you will actually use)

The Win7 bring-up set of flags you should expect to set in practice:

* `Primary`
  * Must be set for scanout/backbuffer allocations.
* `CpuVisible`
  * Must be set for staging allocations that are CPU-mapped via `pfnLockCb`/`pfnUnlockCb`.
  * AeroGPU MVP often sets this for *all* allocations because it uses the single CPU-visible system segment.
* `RenderTarget`
  * Set for RTV-capable allocations (swapchain buffers, render targets).

> `D3DDDI_ALLOCATIONINFOFLAGS` contains more bits (overlay, shared, etc). Keep your initial implementation conservative: set only what you understand and what your KMD uses.

### 4.4 `D3D11DDI_ALLOCATIONINFO` vs `D3DDDI_ALLOCATIONINFO`

In the Win7-era D3D10/11 UMD DDI, the DDIs reuse the `d3dumddi.h` allocation info layout:

* `D3D10DDI_ALLOCATIONINFO`
* `D3D11DDI_ALLOCATIONINFO`

Conceptually, **they are the same structure as** `D3DDDI_ALLOCATIONINFO` (the D3D10/11 headers typedef/alias this for API namespacing).

Practical implication:

* The allocation info array you fill for `CreateResource` can be passed directly to `D3DDDICB_ALLOCATE::pAllocationInfo` when calling `pfnAllocateCb`, and the runtime returns `hAllocation` handles in the same array.

---

## 5) Resource descriptor fields that drive allocation (D3D11)

`D3D11DDIARG_CREATERESOURCE` (from `d3d11umddi.h`) is the UMD-visible description of the resource being created. For allocation, only a subset of fields matter:

### 5.0 Allocation plumbing fields (how CreateResource hands you the output arrays)

These fields are the “bridge” between `CreateResource` and the `pfnAllocateCb` resource-allocation call:

* `UINT NumAllocations`
  * Number of allocations the runtime expects you to allocate for this resource.
* `D3D11DDI_ALLOCATIONINFO* pAllocationInfo`
  * Output array to fill and pass as `D3DDDICB_ALLOCATE::pAllocationInfo`.

### 5.1 Common fields (all resource dimensions)

* `D3D10DDIRESOURCE_TYPE ResourceDimension`
  * Resource dimension discriminator.
  * **Win7 quirk:** D3D11 uses the **D3D10 DDI** enum values for this field:
    * `D3D10DDIRESOURCE_BUFFER`
    * `D3D10DDIRESOURCE_TEXTURE1D`
    * `D3D10DDIRESOURCE_TEXTURE2D`
    * `D3D10DDIRESOURCE_TEXTURE3D`
* `Usage`
  * Default/dynamic/staging semantics (values match `D3D11_USAGE_*`).
* `UINT BindFlags`
  * `D3D11_BIND_*` bits (render target, depth stencil, shader resource, etc).
* `UINT CPUAccessFlags`
  * `D3D11_CPU_ACCESS_READ` / `D3D11_CPU_ACCESS_WRITE` (staging and dynamic resources).
* `UINT MiscFlags`
  * `D3D11_RESOURCE_MISC_*` bits (shared resources, GDI compatibility, etc).

### 5.2 Dimension-specific fields (allocation sizing inputs)

#### Buffer (`ResourceDimension == D3D10DDIRESOURCE_BUFFER`)

* `UINT ByteWidth`
* `UINT StructureByteStride`

#### Texture2D (`ResourceDimension == D3D10DDIRESOURCE_TEXTURE2D`)

* `UINT Width`
* `UINT Height`
* `UINT MipLevels`
* `UINT ArraySize`
* `DXGI_FORMAT Format`
* `DXGI_SAMPLE_DESC SampleDesc`

### 5.3 Swapchain / backbuffer identification (DXGI primary)

When the resource is a DXGI swapchain backbuffer / primary, the DDI exposes this through a “primary descriptor” pointer (from `dxgiddi.h`):

* `const DXGI_DDI_PRIMARY_DESC* pPrimaryDesc`

**Rule of thumb:**

* `pPrimaryDesc != NULL` → treat this resource as a **primary/backbuffer** allocation.
* Allocate with both:
  * `D3DDDICB_ALLOCATEFLAGS.Primary = 1` (resource-level)
  * `D3DDDI_ALLOCATIONINFOFLAGS::Primary = 1` (per-allocation)

### 5.4 D3D10 parity (`D3D10DDIARG_CREATERESOURCE`)

The D3D10 DDI uses `D3D10DDIARG_CREATERESOURCE` (from `d3d10umddi.h`) and the same *WDDM resource allocation model* as D3D11 on Win7:

* the runtime asks the UMD to fill an allocation-info array (`D3D10DDI_ALLOCATIONINFO* pAllocationInfo`), and
* the UMD calls `pfnAllocateCb` with `D3DDDICB_ALLOCATE` to create the allocation(s) and receive `hAllocation` handles.

For allocation purposes, the D3D10 create-resource argument carries the same “shape” of data as the D3D11 one:

* allocation plumbing:
  * `UINT NumAllocations`
  * `D3D10DDI_ALLOCATIONINFO* pAllocationInfo`
* resource classification and access:
  * `ResourceDimension`
  * `Usage`
  * `UINT BindFlags`
  * `UINT CPUAccessFlags`
  * `UINT MiscFlags`
* dimension-specific sizing fields (buffer vs texture)
* swapchain/backbuffer identification:
  * `const DXGI_DDI_PRIMARY_DESC* pPrimaryDesc`

In practice for AeroGPU, you can share almost all of the resource-allocation logic between D3D10 and D3D11 because the per-allocation descriptor layout (`D3DDDI_ALLOCATIONINFO`) is reused by both APIs (via `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO`).

---

## 6) “Minimal correct” allocation strategies (Win7 bring-up)

The table below is a pragmatic “works first” allocation plan for AeroGPU’s MVP memory model (**single system-memory segment**, CPU-visible).

| Resource class | Allocation count strategy | Size computation | Flags you must set |
|---|---:|---|---|
| Buffer | 1 allocation per resource | `Size = ByteWidth` (optionally align up to 256) | `CpuVisible` if CPU reads/writes are expected (dynamic/staging or `CPUAccessFlags != 0`) |
| Texture2D (default) | 1 allocation per resource | `rowPitch = Align(Width * bytesPerPixel(Format), 256)`; `Size = rowPitch * Height` (no mips/arrays in MVP) | `RenderTarget` if `BindFlags & D3D11_BIND_RENDER_TARGET`; `CpuVisible` only if CPU access is requested |
| Swapchain backbuffer | 1 allocation per backbuffer | Same as Texture2D, but match the swapchain format exactly (commonly `DXGI_FORMAT_B8G8R8A8_UNORM`) | `D3DDDICB_ALLOCATEFLAGS.Primary`; allocation `Flags.Primary`; allocation `Flags.RenderTarget`; typically `CpuVisible` in AeroGPU MVP |
| Staging Texture2D | 1 allocation per resource | Same as Texture2D | Allocation `Flags.CpuVisible` (required); bind flags are typically 0; use `pfnLockCb`/`pfnUnlockCb` in `Map`/`Unmap` |

Notes / constraints for MVP:

* **Mipmaps and arrays:** simplest bring-up assumes `MipLevels == 1` and `ArraySize == 1`. If you see otherwise, either:
  * allocate one big linear blob and compute subresource offsets, or
  * return NOT_SUPPORTED / set error until implemented.
* **Depth/stencil:** treat as Texture2D sizing-wise; set `RenderTarget`-like flags only if your KMD/translator cares. The runtime’s bind flags still control whether DSV creation is legal.
* **All-system-memory (AeroGPU):** the KMD already advertises a single `CpuVisible` system segment (`DXGKQAITYPE_QUERYSEGMENT`). In that world, marking `CpuVisible` broadly is acceptable and keeps Map/Unmap simple.

---

## 7) Win7 + AeroGPU-specific quirks (allocation-relevant)

### 7.1 `OpenAdapter11` uses `D3D10DDIARG_OPENADAPTER`

Repeat because it’s easy to get wrong: on Win7/WDDM 1.1, the D3D11 runtime’s UMD entrypoint is:

* `OpenAdapter11(D3D10DDIARG_OPENADAPTER*)`

So your adapter-open code must be able to branch on the requested DDI interface/version and return the correct adapter function table for D3D10 vs D3D11.

### 7.2 Single system-memory segment model

AeroGPU’s MVP KMD exposes **one** segment:

* Segment 1: CPU-visible “system memory” (`DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL`, `Flags.CpuVisible = 1`, `Flags.Aperture = 1`)

See:

* `docs/graphics/win7-wddm11-aerogpu-driver.md` (§5)
* `drivers/aerogpu/kmd/src/aerogpu_kmd.c` (`DXGKQAITYPE_QUERYSEGMENT`)

**Implication for UMD allocation flags:**

* You do not need complex residency/eviction policy to get correctness.
* You *do* still need to set `Primary`/`RenderTarget`/`CpuVisible` correctly so dxgkrnl routes scanout and Map/Unmap expectations correctly.
