# Win7 (WDDM 1.1) D3D10/D3D11 UMD resource backing allocations + Map/Unmap (WDK 7.1 reference)

This document is a **Windows 7 / WDDM 1.1 (WDK 7.1)** reference for the parts of the D3D10/D3D11 user‑mode driver (UMD) DDI that are easy to get subtly wrong during bring‑up:

* **How `CreateResource` decides backing WDDM allocations** (buffers + Texture2D)
* **How `Map`/`Unmap` is implemented using `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK`**
* **Map flag translation** (D3D10 + D3D11) → `D3DDDICB_LOCKFLAGS`
* **Pitch** (`RowPitch` / `DepthPitch`) expectations for staging readback
* **Synchronization** expectations for `CopyResource` + `Flush` + `Map(READ)`

Scope is intentionally narrow: **resource backing allocations + Map/Unmap only** (no shader/pipeline breadth).

> Header references: symbol names in this doc match Win7-era WDK headers: `d3dumddi.h`, `d3d10umddi.h`, `d3d11umddi.h`, and (for KMD allocation creation) `dispmprt.h`.

---

## 0) Glossary: “resource”, “WDDM allocation”, and “DMA buffer allocation” (don’t mix these up)

WDDM uses the word “allocation” in *two* distinct ways that both show up in the Win7 D3D10/11 stack:

### 0.1 Resource backing allocations (the thing resources are backed by)

* A D3D “resource” (buffer/texture) is a **UMD object** (`D3D10DDI_HRESOURCE` / `D3D11DDI_HRESOURCE`).
* That resource is typically backed by one or more **WDDM allocations** (kernel objects tracked by VidMm).
* These backing allocations are represented to the UMD by per‑process allocation handles (`D3DKMT_HANDLE`, often named `hAllocation` in WDK structs).

This doc’s “resource allocation” topic is about these backing allocations.

### 0.2 DMA buffer allocation (the thing you write commands into)

The runtime callback table `D3DDDI_DEVICECALLBACKS` also contains:

* `pfnAllocateCb` / `pfnDeallocateCb` using:
  * `D3DDDICB_ALLOCATE`
  * `D3DDDICB_DEALLOCATE`

On Win7 these are used to **acquire and return DMA buffers** (command buffer backing store) for submission.
They are **not** how you create resource backing allocations.

For the DMA buffer “allocate/deallocate” contract, see:

* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

### 0.3 `D3DDDICB_ALLOCATE` / `D3DDDICB_DEALLOCATE` (DMA buffer) field cheat-sheet

If you are building a Win7-style submit path, the shared runtime callback table (`D3DDDI_DEVICECALLBACKS`) provides:

* `pfnAllocateCb` taking `D3DDDICB_ALLOCATE`
* `pfnDeallocateCb` taking `D3DDDICB_DEALLOCATE`

Important `D3DDDICB_ALLOCATE` fields (names vary slightly across Win7-capable header vintages; both spellings are common):

* Requested capacity (bytes):
  * `DmaBufferSize` **or** `CommandBufferSize`
* Returned pointers (owned by the runtime for this DMA buffer instance):
  * `pDmaBuffer` **or** `pCommandBuffer`
  * `pAllocationList` + `AllocationListSize` (entries)
  * `pPatchLocationList` + `PatchLocationListSize` (entries)
  * If present: `pDmaBufferPrivateData` + `DmaBufferPrivateDataSize` (bytes)
* If present: `hContext` (kernel context handle this DMA buffer is scoped to)

Important `D3DDDICB_DEALLOCATE` fields:

* Return the same pointers you received from `D3DDDICB_ALLOCATE`:
  * `pDmaBuffer`/`pCommandBuffer`
  * `pAllocationList`
  * `pPatchLocationList`
  * (and `pDmaBufferPrivateData` if present)

---

## 1) What is shared between D3D10 and D3D11 vs D3D11-only?

### Shared (D3D10 and D3D11 on Win7)

* Resource creation entrypoints:
  * `PFND3D10DDI_CREATERESOURCE`
  * `PFND3D11DDI_CREATERESOURCE`
* The WDDM allocation descriptor layout:
  * `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO` (aliases of `D3DDDI_ALLOCATIONINFO` from `d3dumddi.h` on Win7-capable header sets)
  * `D3DDDI_ALLOCATIONINFOFLAGS`
* CPU mapping callback structs (from `d3dumddi.h`):
  * `D3DDDICB_LOCK` + `D3DDDICB_LOCKFLAGS`
  * `D3DDDICB_UNLOCK`

### D3D11-only split: device funcs vs device-context funcs

* Allocation decisions happen during **resource creation**, which is a **device** entrypoint:
  * `PFND3D11DDI_CREATERESOURCE` (in `D3D11DDI_DEVICEFUNCS`)
* `Map`/`Unmap` are **immediate context** entrypoints in D3D11:
  * `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP` (in `D3D11DDI_DEVICECONTEXTFUNCS`)

In D3D10, `Map`/`Unmap` are device funcs (`D3D10DDI_DEVICEFUNCS`).

---

## 2) Which DDI entrypoints decide backing allocations for resources?

### 2.1 D3D10: `PFND3D10DDI_CREATERESOURCE`

On Win7 the D3D10 runtime calls the UMD’s `PFND3D10DDI_CREATERESOURCE`. This is where the UMD must:

1. Classify the resource (buffer vs texture, usage, bind flags, CPU access).
2. Decide how many backing allocations are needed (bring‑up usually uses **1 allocation per resource**).
3. Fill the per‑allocation descriptors (`D3D10DDI_ALLOCATIONINFO` / `D3DDDI_ALLOCATIONINFO`) including:
   * size
   * alignment
   * flags (`D3DDDI_ALLOCATIONINFOFLAGS`)
   * KMD private driver data blob

Ultimately dxgkrnl will call the KMD’s `DxgkDdiCreateAllocation` using the private driver data you supplied.

### 2.2 D3D11: `PFND3D11DDI_CREATERESOURCE`

Same conceptual responsibilities as D3D10, but the D3D11 runtime calls the device entrypoint `PFND3D11DDI_CREATERESOURCE`.

`Map`/`Unmap` are not device funcs in D3D11; see §5.

---

## 3) Backing allocation descriptor structures (what you must fill in `CreateResource`)

### 3.1 `D3DDDI_ALLOCATIONINFO` (alias: `D3D10DDI_ALLOCATIONINFO` / `D3D11DDI_ALLOCATIONINFO`)

On Win7, D3D10/11 `CreateResource` flows use the `d3dumddi.h` allocation-info layout:

* `D3DDDI_ALLOCATIONINFO`
  * commonly aliased as `D3D10DDI_ALLOCATIONINFO` and `D3D11DDI_ALLOCATIONINFO` in the D3D10/11 DDI headers.

The bring-up‑critical fields in each entry are:

* `UINT64 Size` (bytes) — **input** from UMD
* `UINT64 Alignment` — **input** from UMD (0 typically means “default”)
* `D3DDDI_ALLOCATIONINFOFLAGS Flags` — **input** from UMD
  * `Primary` (swapchain/backbuffer style allocations)
  * `RenderTarget` (RTV-capable allocations)
  * `CpuVisible` (required for staging readback and any CPU-mapped resource)
* `VOID* pPrivateDriverData` + `UINT PrivateDriverDataSize` — **input** from UMD
  * This blob is preserved and passed to the KMD (`DxgkDdiCreateAllocation`) so the KMD can learn:
    * format / dimensions
    * pitch / layout expectations
    * sharing IDs (if applicable)
* `D3DKMT_HANDLE hAllocation` — **output** (filled by the runtime/OS on success)

> Important: `hAllocation` is per‑process. Do not treat it as a stable cross‑process identity key.

### 3.2 Typical allocation recipes (minimal FL10_0 bring‑up)

These are conservative “works first” defaults.

#### Buffers

| API intent | Typical desc | `D3DDDI_ALLOCATIONINFOFLAGS` | Mappable? |
|---|---|---|---|
| DEFAULT GPU buffer | `D3D11_USAGE_DEFAULT`, `CPUAccessFlags = 0` | `CpuVisible = 0` | No |
| DYNAMIC upload buffer | `D3D11_USAGE_DYNAMIC`, `CPUAccessFlags = D3D11_CPU_ACCESS_WRITE` | `CpuVisible = 1` | Yes (WRITE_DISCARD / NO_OVERWRITE) |
| STAGING readback buffer | `D3D11_USAGE_STAGING`, `CPUAccessFlags = D3D11_CPU_ACCESS_READ` | `CpuVisible = 1` | Yes (READ) |

#### Texture2D

| API intent | Typical desc | `D3DDDI_ALLOCATIONINFOFLAGS` | Mappable? |
|---|---|---|---|
| DEFAULT render target | `D3D11_USAGE_DEFAULT`, `BindFlags` contains `D3D11_BIND_RENDER_TARGET`, `CPUAccessFlags = 0` | `RenderTarget = 1`, `CpuVisible = 0` | No |
| STAGING readback Texture2D | `D3D11_USAGE_STAGING`, `CPUAccessFlags = D3D11_CPU_ACCESS_READ`, `BindFlags = 0` | `CpuVisible = 1` | Yes (READ) |

Bring-up simplifications:

* Prefer **linear layouts** for anything you intend to `Map`.
* Start with **MipLevels = 1** and **ArraySize = 1** for staging readback paths.

---

## 4) Runtime callback tables involved (and what they’re used for here)

During device creation the runtime provides callback tables (names differ slightly by D3D10 vs D3D11):

* D3D10: `D3D10DDI_DEVICECALLBACKS`
* D3D11: `D3D11DDI_DEVICECALLBACKS`
* Shared WDDM callbacks (submission/sync/mapping): `D3DDDI_DEVICECALLBACKS` (from `d3dumddi.h`)

For allocation + Map/Unmap, the callbacks you care about are:

| Callback | Structs | Used for |
|---|---|---|
| `pfnLockCb` | `D3DDDICB_LOCK` / `D3DDDICB_LOCKFLAGS` | CPU mapping (`Map`) + implicit sync for staging readback. |
| `pfnUnlockCb` | `D3DDDICB_UNLOCK` | End CPU mapping (`Unmap`). |
| *(optional for explicit waits)* `pfnWaitForSynchronizationObjectCb` | `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT` | Poll/wait for a fence value before mapping readback resources. |

---

## 5) Map/Unmap: where it lives and how it uses `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK`

### 5.1 D3D10 Map/Unmap (device funcs)

* Entry points: `PFND3D10DDI_MAP` / `PFND3D10DDI_UNMAP` (`D3D10DDI_DEVICEFUNCS`)
* The UMD implements `pfnMap` by calling `pfnLockCb` with a `D3DDDICB_LOCK`:
  * `hAllocation = <backing allocation for the (sub)resource>`
  * `SubResourceIndex` / `SubresourceIndex = <subresource>`
  * `Flags = <translated lock flags>`
* On success, `pfnLockCb` fills:
  * `pData` (CPU pointer)
  * `Pitch` (row pitch)
  * `SlicePitch` (slice pitch)
* `pfnUnmap` calls `pfnUnlockCb` with `D3DDDICB_UNLOCK`.

### 5.2 D3D11 Map/Unmap (device-context funcs)

* Entry points: `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP` (`D3D11DDI_DEVICECONTEXTFUNCS`)
* Same `pfnLockCb`/`pfnUnlockCb` callback usage; only the DDI table/argument types differ.

> Win7 D3D11 quirk: the DDI `MapType` parameter is commonly typed as `D3D10_DDI_MAP` even in the D3D11 DDI. The numeric values match `D3D11_MAP`.

---

## 6) Map semantics translation tables (`D3D11_MAP_*` / `D3D10_DDI_MAP*` → `D3DDDICB_LOCKFLAGS`)

### 6.1 D3D11: `D3D11_MAP` + `D3D11_MAP_FLAG_DO_NOT_WAIT`

| API map mode | `D3DDDICB_LOCKFLAGS` bits |
|---|---|
| `D3D11_MAP_READ` | `ReadOnly = 1` |
| `D3D11_MAP_WRITE` | `WriteOnly = 1` |
| `D3D11_MAP_READ_WRITE` | read+write (`ReadOnly = 0`, `WriteOnly = 0`) |
| `D3D11_MAP_WRITE_DISCARD` | `WriteOnly = 1`, `Discard = 1` |
| `D3D11_MAP_WRITE_NO_OVERWRITE` | `WriteOnly = 1`, `NoOverwrite = 1` |

Additional flags:

* If the caller specifies `D3D11_MAP_FLAG_DO_NOT_WAIT`, set `D3DDDICB_LOCKFLAGS::DonotWait = 1`.

If `DonotWait = 1` and the allocation is still busy, `pfnLockCb` should fail in a “still drawing” way and the UMD’s `pfnMap` must return `DXGI_ERROR_WAS_STILL_DRAWING`.

### 6.2 D3D10: `D3D10_DDI_MAP` + `D3D10_DDI_MAPFLAGS`

| DDI map mode | `D3DDDICB_LOCKFLAGS` bits |
|---|---|
| `D3D10_DDI_MAP_READ` | `ReadOnly = 1` |
| `D3D10_DDI_MAP_WRITE` | `WriteOnly = 1` |
| `D3D10_DDI_MAP_READWRITE` | read+write (`ReadOnly = 0`, `WriteOnly = 0`) |
| `D3D10_DDI_MAP_WRITE_DISCARD` | `WriteOnly = 1`, `Discard = 1` |
| `D3D10_DDI_MAP_WRITE_NOOVERWRITE` | `WriteOnly = 1`, `NoOverwrite = 1` |

Additional flags:

* If `D3D10_DDI_MAPFLAGS` contains `D3D10_DDI_MAP_FLAG_DO_NOT_WAIT`, set `D3DDDICB_LOCKFLAGS::DonotWait = 1`.

> `Discard` and `NoOverwrite` are not “optional hints”; they materially change synchronization behavior for dynamic resources.

---

## 7) Row pitch / slice pitch: where they come from and what you must return

On Win7, the runtime does not “fix up” pitches:

* The values returned from the UMD DDI `Map` are what the API returns to the app (`D3D11_MAPPED_SUBRESOURCE`, etc).

For Win7 D3D10/11 UMDs, the pitches come from the runtime lock callback output:

* `D3DDDICB_LOCK::Pitch` → D3D `RowPitch`
* `D3DDDICB_LOCK::SlicePitch` → D3D `DepthPitch` (for 2D, typically `RowPitch * Height`)

Where `Pitch`/`SlicePitch` ultimately come from:

* Your **allocation layout decision** must be communicated to the KMD (usually via the per‑allocation `pPrivateDriverData` blob).
* The KMD’s `DxgkDdiLock` implementation returns a CPU pointer and pitch metadata consistent with that layout.

For staging readback, `RowPitch` correctness is critical: apps/tests index pixels using the returned pitch.

---

## 8) Staging readback synchronization (Copy → Flush → Map(READ))

For a staging readback pattern like:

1. `CopyResource` (DEFAULT render target → STAGING texture)
2. `Flush`
3. `Map(READ)` on the staging texture

…`Map(READ)` must **block until the GPU copy completes**, unless `DO_NOT_WAIT` is specified.

### 8.1 Where the “implicit sync” happens on Win7

On Win7/WDDM 1.1, the intended implicit sync point is:

* `pfnLockCb` with `D3DDDICB_LOCK`

Behavior:

* If you call `pfnLockCb` **without** `D3DDDICB_LOCKFLAGS::DonotWait`, the runtime/VidMm path will wait until it is safe to expose the allocation to CPU.
* If `DonotWait = 1` and the allocation is still in use, the lock should fail without blocking; the UMD returns `DXGI_ERROR_WAS_STILL_DRAWING` to the app.

### 8.2 “Flush before you wait” rule

If the UMD buffers GPU work in user‑mode, a `Map(READ)` that needs the result must ensure the producing work is submitted before waiting (otherwise you can deadlock waiting on work that has not been queued).

In practice:

* treat `Map(READ)` on staging as an implicit submit/flush boundary, then call `pfnLockCb`.
