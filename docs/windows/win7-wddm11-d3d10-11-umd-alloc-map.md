# Win7 (WDDM 1.1) D3D10/D3D11 UMD resource allocation + Map/Unmap callback usage (WDK 7.1 reference)

This document is a **Win7 / WDDM 1.1 (WDK 7.1)** reference for the parts of the D3D10/D3D11 user-mode driver (UMD) DDI that are easy to get subtly wrong during bring-up:

* **Where resource backing allocations are created** (and freed)
* **Which runtime callback tables/structs are involved**
* **How `Map`/`Unmap` is implemented using `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK`**
* **Pitch (row/slice) expectations**
* **Staging readback synchronization behavior**

Scope is intentionally narrow: **allocation + Map/Unmap only**. Shader/pipeline/DDI breadth is covered elsewhere.

> Header references: symbol names in this doc match Windows 7-era WDK headers: `d3dumddi.h`, `d3d10umddi.h`, `d3d11umddi.h`.

---

## 0) “Resource” vs “allocation” (the mental model Win7 expects)

On WDDM 1.1, a D3D “resource” (buffer/texture) is a **driver object** that typically owns **one or more WDDM allocations**:

* The **resource handle** the runtime passes to the UMD is `D3D10DDI_HRESOURCE` (D3D10) or `D3D11DDI_HRESOURCE` (D3D11).
* The **backing memory** that VidMm manages is represented to the UMD by allocation handles returned from the runtime callback **`pfnAllocateCb`** (see `D3DDDICB_ALLOCATE` / `D3DDDICB_ALLOCATIONINFO`).

For bring-up, it helps to think in layers:

1. **DDI object**: what the D3D10/11 runtime calls your UMD with (e.g. `PFND3D11DDI_CREATERESOURCE`).
2. **WDDM allocation(s)**: what you ask the runtime/VidMm to create via callbacks (`D3DDDICB_ALLOCATE`).
3. **CPU mapping**: what you obtain by locking an allocation (`D3DDDICB_LOCK`) and return through the DDI `Map` output structure.

---

## 1) What is shared between D3D10 and D3D11 vs D3D11-only?

### Shared (D3D10 and D3D11 on Win7)

These names/structures are used by **both** D3D10 and D3D11 UMDs on Win7:

* Runtime callback struct namespace: `D3DDDICB_*` (from `d3dumddi.h`)
  * `D3DDDICB_ALLOCATE` + `D3DDDICB_ALLOCATIONINFO`
  * `D3DDDICB_DEALLOCATE`
  * `D3DDDICB_LOCK` + `D3DDDICB_LOCKFLAGS`
  * `D3DDDICB_UNLOCK`
* The core runtime callback table concept: `D3DDDI_DEVICECALLBACKS` (and the D3D10/11-specific wrapper tables that contain the same callback entrypoints).

### D3D11-only split: device funcs vs device-context funcs

* Allocation happens during **resource creation**, which is a **device** entrypoint: `PFND3D11DDI_CREATERESOURCE` (in `D3D11DDI_DEVICEFUNCS`).
* `Map`/`Unmap` are **immediate context** entrypoints in D3D11: `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP` (in `D3D11DDI_DEVICECONTEXTFUNCS`).

In D3D10, `Map`/`Unmap` live on the device function table (see §3).

---

## 2) Runtime callback tables involved (and the structs they take)

During device creation the runtime hands the UMD a callbacks table:

* D3D10: `D3D10DDI_DEVICECALLBACKS`
* D3D11: `D3D11DDI_DEVICECALLBACKS`

These tables expose the “VidMm interaction” callbacks the UMD must use for allocations and CPU mappings:

| Callback table member | Callback prototype uses | What it does |
|---|---|---|
| `pfnAllocateCb` | `D3DDDICB_ALLOCATE`, `D3DDDICB_ALLOCATIONINFO` | Create WDDM allocation(s) to back a resource. |
| `pfnDeallocateCb` | `D3DDDICB_DEALLOCATE` | Free allocation(s) created by `pfnAllocateCb`. |
| `pfnLockCb` | `D3DDDICB_LOCK`, `D3DDDICB_LOCKFLAGS` | Map an allocation into CPU VA space (and implicitly synchronize if needed). |
| `pfnUnlockCb` | `D3DDDICB_UNLOCK` | Release a CPU mapping acquired via `pfnLockCb`. |

> Practical rule: **UMD resource allocation is not “malloc”.** On Win7 you are expected to request allocations through the runtime callback table so VidMm can track residency, paging, and GPU synchronization.

---

## 3) Which DDI entrypoints allocate backing allocations for resources?

### D3D10: `PFND3D10DDI_CREATERESOURCE`

On Win7 the D3D10 runtime expects the UMD to create backing allocations inside:

* `PFND3D10DDI_CREATERESOURCE` (device function table: `D3D10DDI_DEVICEFUNCS`)

**Flow (minimal, one allocation per resource):**

1. Runtime calls `PFND3D10DDI_CALCPRIVATERESOURCESIZE` → allocates `hResource.pDrvPrivate`.
2. Runtime calls `PFND3D10DDI_CREATERESOURCE(hDevice, pCreate, hResource, hRTResource)`.
3. UMD decides allocation layout (size/pitch/flags).
4. UMD calls runtime callback `pfnAllocateCb` with a filled `D3DDDICB_ALLOCATE` containing:
   * `NumAllocations = 1`
   * `pAllocationInfo = &allocationInfo` (an array of `D3DDDICB_ALLOCATIONINFO`)
5. Runtime fills out the allocation handle(s) (in `D3DDDICB_ALLOCATIONINFO`) and returns.
6. UMD stores allocation handle(s) in its private resource object for later `Map`/`Unmap` and destruction.

On `PFND3D10DDI_DESTROYRESOURCE`, free those allocations via `pfnDeallocateCb` (`D3DDDICB_DEALLOCATE`).

### D3D11: `PFND3D11DDI_CREATERESOURCE`

D3D11 uses the same basic allocation model but splits entrypoints across tables:

* Allocation happens inside `PFND3D11DDI_CREATERESOURCE` (device table: `D3D11DDI_DEVICEFUNCS`)
* CPU mapping happens inside `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP` (immediate context table: `D3D11DDI_DEVICECONTEXTFUNCS`)

The `PFND3D11DDI_CREATERESOURCE` allocation flow is the same as D3D10: call `pfnAllocateCb` with `D3DDDICB_ALLOCATE` and persist the returned allocation handle(s).

---

## 4) `D3DDDICB_ALLOCATE` / `D3DDDICB_ALLOCATIONINFO`: what you must fill (bring-up essentials)

The runtime callback `pfnAllocateCb` takes a `D3DDDICB_ALLOCATE` containing an array of `D3DDDICB_ALLOCATIONINFO`.

For a minimal FL10_0 bring-up, the “must decide and persist” pieces are:

### 4.1 The key inputs to allocation layout decisions

From the D3D runtime you primarily look at the resource description passed to:

* `PFND3D10DDI_CREATERESOURCE` via `D3D10DDIARG_CREATERESOURCE`
* `PFND3D11DDI_CREATERESOURCE` via `D3D11DDIARG_CREATERESOURCE`

The important API-visible concepts they encode:

* **Usage** (`D3D10_USAGE_*` / `D3D11_USAGE_*`): DEFAULT vs DYNAMIC vs STAGING
* **BindFlags** (DEFAULT resources only): e.g. `D3D11_BIND_RENDER_TARGET`, `D3D11_BIND_VERTEX_BUFFER`
* **CPUAccessFlags** (DYNAMIC/STAGING): e.g. `D3D11_CPU_ACCESS_READ`, `D3D11_CPU_ACCESS_WRITE`

These fields drive whether the allocation must be:

* GPU-only (DEFAULT)
* CPU-writable (DYNAMIC)
* CPU-readable (STAGING readback)

### 4.2 The “typical” `D3DDDICB_ALLOCATIONINFO` fields you must set correctly

For each element in the `D3DDDICB_ALLOCATIONINFO` array, the UMD must provide (at minimum):

* `Size` (total bytes)
* `Alignment` (0 is commonly used to mean “default alignment”; use explicit alignment if your KMD requires it)
* `pPrivateDriverData` + `PrivateDriverDataSize`
  * This is the blob the runtime passes down to the KMD for `DxgkDdiCreateAllocation` (how your KMD learns the size/format/pitch/usage of the allocation).

And the UMD must consume:

* `hAllocation` (filled in by the runtime on success) — store this per allocation/subresource.

> `D3DDDICB_ALLOCATIONINFO::Flags` exists and is important, but the exact bits you need depend on your KMD’s segment/memory model. For bring-up, treat flags as a way to express “CPU visible vs GPU-only” (see §5) and encode the rest in `pPrivateDriverData` so the KMD can make the real placement decision.

### 4.3 Minimal allocation recipes (buffers + Texture2D)

The tables below describe conservative, “works first” layouts.

#### Buffers

| API intent | Typical D3D desc | Allocation placement | Map allowed? | Notes |
|---|---|---|---|---|
| DEFAULT GPU buffer | `D3D11_USAGE_DEFAULT`, `CPUAccessFlags = 0` | GPU memory (or “GPU-only” segment) | No | Upload via `UpdateSubresource(UP)` or staging copy. |
| DYNAMIC upload buffer | `D3D11_USAGE_DYNAMIC`, `CPUAccessFlags = D3D11_CPU_ACCESS_WRITE` | CPU-visible, write-optimized | Yes (WRITE_DISCARD/NO_OVERWRITE) | `Map(WRITE_DISCARD)` should not stall on GPU reads; use DISCARD semantics (§6). |
| STAGING readback buffer | `D3D11_USAGE_STAGING`, `CPUAccessFlags = D3D11_CPU_ACCESS_READ` | CPU-visible, read-optimized | Yes (READ) | Map must block until copy completes (§8). |

#### Texture2D (common Win7 bring-up cases)

| API intent | Typical D3D desc | Allocation placement | Map allowed? | Pitch requirements |
|---|---|---|---|---|
| DEFAULT render target | `D3D11_USAGE_DEFAULT`, `BindFlags` contains `D3D11_BIND_RENDER_TARGET`, `CPUAccessFlags = 0` | GPU-only | No | Pitch is internal; if you ever allow CPU mapping of DEFAULT, you must handle tiling/resolve yourself. |
| STAGING Texture2D readback | `D3D11_USAGE_STAGING`, `CPUAccessFlags = D3D11_CPU_ACCESS_READ`, `BindFlags = 0` | CPU-visible (linear) | Yes (READ) | Must return correct `RowPitch`/`DepthPitch` to the app (§7). |

Bring-up simplifications that are consistent with Win7 expectations:

* Start with **MipLevels = 1** and **ArraySize = 1** (one subresource) for staging readback and tests.
* Use a **linear layout** for any resource you intend to `Map`.

---

## 5) CPU access flags → allocation characteristics (what “must be true”)

Even if your exact KMD segment flags differ, the Win7 runtime contract boils down to:

* If the resource is intended to be CPU-readable/writable, **`pfnLockCb` must return a valid CPU pointer** for the allocation (and `pfnUnlockCb` must succeed).
* If the resource is GPU-only (DEFAULT), either:
  * the runtime will never call `Map`, or
  * your UMD must fail `Map` cleanly (set error / return failure) according to the DDI contract.

Concrete guidance for minimal FL10_0:

* **DYNAMIC** (CPU write):
  * Choose an allocation kind that is CPU-visible and optimized for write-combined behavior.
  * Always implement `WRITE_DISCARD` and `WRITE_NO_OVERWRITE` mapping flags (even if conservatively).
* **STAGING readback** (CPU read):
  * Choose an allocation kind that is CPU-visible and suitable for cached reads.
  * Ensure `CopyResource/CopySubresourceRegion` into this allocation is visible after `Map(READ)` returns (the implicit synchronization is described in §8).

---

## 6) Map/Unmap: where it lives and how it uses `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK`

### D3D10 Map/Unmap (device funcs)

* Entry points: `PFND3D10DDI_MAP` / `PFND3D10DDI_UNMAP`
* Table: `D3D10DDI_DEVICEFUNCS`
* Map arg struct: `D3D10DDIARG_MAP`

Typical implementation skeleton:

1. Determine which allocation/subresource is being mapped (often 1:1 in bring-up).
2. Translate `D3D10_DDI_MAP` + `D3D10_DDI_MAPFLAGS` to `D3DDDICB_LOCKFLAGS` (§6.3).
3. Call runtime callback `pfnLockCb` with `D3DDDICB_LOCK`.
4. Return the CPU pointer + pitches to the runtime via `D3D10DDI_MAPPED_SUBRESOURCE`.
5. On `PFND3D10DDI_UNMAP`, call `pfnUnlockCb` with `D3DDDICB_UNLOCK`.

### D3D11 Map/Unmap (device-context funcs)

* Entry points: `PFND3D11DDI_MAP` / `PFND3D11DDI_UNMAP`
* Table: `D3D11DDI_DEVICECONTEXTFUNCS`
* Map arg struct: `D3D11DDIARG_MAP`

The `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK` callback usage is identical to D3D10; only the DDI table/argument types differ.

### 6.3 Map semantics translation tables

These tables are the intended translation of API-level map modes to `D3DDDICB_LOCKFLAGS` bits.

#### D3D11: `D3D11_MAP` + `D3D11_MAP_FLAG_DO_NOT_WAIT` → `D3DDDICB_LOCKFLAGS`

| API map mode | `D3DDDICB_LOCKFLAGS` bits |
|---|---|
| `D3D11_MAP_READ` | `ReadOnly = 1` |
| `D3D11_MAP_WRITE` | `WriteOnly = 1` |
| `D3D11_MAP_READ_WRITE` | `ReadOnly = 0`, `WriteOnly = 0` (read/write) |
| `D3D11_MAP_WRITE_DISCARD` | `WriteOnly = 1`, `Discard = 1` |
| `D3D11_MAP_WRITE_NO_OVERWRITE` | `WriteOnly = 1`, `NoOverwrite = 1` |

Additional flags:

* If the caller specifies `D3D11_MAP_FLAG_DO_NOT_WAIT`, set `D3DDDICB_LOCKFLAGS::DoNotWait = 1`.

#### D3D10: `D3D10_DDI_MAP` + `D3D10_DDI_MAPFLAGS` → `D3DDDICB_LOCKFLAGS`

| DDI map mode | `D3DDDICB_LOCKFLAGS` bits |
|---|---|
| `D3D10_DDI_MAP_READ` | `ReadOnly = 1` |
| `D3D10_DDI_MAP_WRITE` | `WriteOnly = 1` |
| `D3D10_DDI_MAP_READWRITE` | `ReadOnly = 0`, `WriteOnly = 0` (read/write) |
| `D3D10_DDI_MAP_WRITE_DISCARD` | `WriteOnly = 1`, `Discard = 1` |
| `D3D10_DDI_MAP_WRITE_NOOVERWRITE` | `WriteOnly = 1`, `NoOverwrite = 1` |

Additional flags:

* If `D3D10_DDI_MAPFLAGS` contains `D3D10_DDI_MAP_FLAG_DO_NOT_WAIT`, set `D3DDDICB_LOCKFLAGS::DoNotWait = 1`.

> For both D3D10 and D3D11: `Discard` and `NoOverwrite` are *not* “UMD-only hints”; they change the expected synchronization behavior. Do not ignore them for dynamic buffers.

---

## 7) Row pitch / slice pitch: what the runtime expects the UMD to return

### 7.1 Where the app-visible pitch values come from

On Win7, the runtime does not “fix up” pitches for you:

* Whatever pitches you return from the UMD’s DDI `Map` are what the runtime returns to the app in:
  * `D3D10_MAPPED_TEXTURE2D` / `D3D10_MAPPED_TEXTURE3D` (D3D10 API), or
  * `D3D11_MAPPED_SUBRESOURCE` (D3D11 API).

At the DDI level, you typically fill:

* D3D10: `D3D10DDI_MAPPED_SUBRESOURCE::{pData, RowPitch, DepthPitch}`
* D3D11: `D3D11DDI_MAPPED_SUBRESOURCE::{pData, RowPitch, DepthPitch}`

### 7.2 Where to source pitch values (Win7 bring-up guidance)

The runtime lock callback (`pfnLockCb` with `D3DDDICB_LOCK`) gives you a CPU pointer to the allocation. The pitch values are expected to come from your **resource layout decision**:

* Either compute and store pitches in your private resource object at `CreateResource` time, or
* Treat pitches as derived from the same layout fields you pass down to the KMD via `D3DDDICB_ALLOCATIONINFO::pPrivateDriverData`.

Practical, correct defaults for linear staging resources:

* `RowPitch = Align(width_in_bytes, 256)` (or whatever alignment your backend requires)
* `DepthPitch = RowPitch * height` (for Texture2D, “slice pitch”)

### 7.3 Buffers vs textures

* For **buffers**, the API does not meaningfully use row/slice pitch; apps treat `pData` as a flat byte array.
* For **Texture2D**, apps rely on `RowPitch` and will interpret the data incorrectly if it is wrong (common readback corruption cause).

---

## 8) Staging readback synchronization (Copy → Flush → Map(READ))

### The expectation (what tests will assume)

For a staging readback path like:

1. `CopyResource` (DEFAULT render target → STAGING texture)
2. `Flush`
3. `Map(D3D11_MAP_READ)` on the staging texture

…the `Map(READ)` call must **block until the GPU copy completes**, unless the caller asked for “don’t wait”.

### Where the implicit sync happens on Win7

On Windows 7 / WDDM 1.1, the intended implicit sync point is the runtime callback:

* `pfnLockCb` with `D3DDDICB_LOCK`

Behavior:

* If you call `pfnLockCb` **without** `D3DDDICB_LOCKFLAGS::DoNotWait`, the runtime/VidMm path will wait until the allocation is safe to map for CPU access (i.e. until prior GPU work that writes that allocation completes).
* If you set `D3DDDICB_LOCKFLAGS::DoNotWait` and the allocation is still busy, `pfnLockCb` may fail with `D3DDDIERR_WASSTILLDRAWING` (which the runtime surfaces back to the app as `DXGI_ERROR_WAS_STILL_DRAWING` for `Map`).

UMD guidance:

* Do not implement your own busy-wait loop for staging readback; rely on the `pfnLockCb` synchronization behavior.
* Ensure your `pfnFlush` actually submits GPU work so the copy can complete; otherwise `pfnLockCb` can block indefinitely and trigger TDR.

---

## 9) Minimal end-to-end example (conceptual)

This is the “shape” of a correct bring-up implementation (pseudo-flow, not code):

1. `PFND3D11DDI_CREATERESOURCE` (DEFAULT render target):
   * allocate GPU-only allocation via `pfnAllocateCb(D3DDDICB_ALLOCATE)`
2. `PFND3D11DDI_CREATERESOURCE` (STAGING readback texture):
   * allocate CPU-visible linear allocation via `pfnAllocateCb(D3DDDICB_ALLOCATE)`
3. `PFND3D11DDI_COPYRESOURCE`:
   * enqueue a GPU copy from DEFAULT → STAGING
4. `PFND3D11DDI_FLUSH`:
   * submit work
5. `PFND3D11DDI_MAP` on STAGING with `D3D11_MAP_READ`:
   * call `pfnLockCb(D3DDDICB_LOCK)` (no `DoNotWait`) → blocks until copy done
   * return `{pData, RowPitch, DepthPitch}` to runtime
6. `PFND3D11DDI_UNMAP`:
   * call `pfnUnlockCb(D3DDDICB_UNLOCK)`

If any step returns “still drawing” while `DO_NOT_WAIT` is set, propagate that failure cleanly.

