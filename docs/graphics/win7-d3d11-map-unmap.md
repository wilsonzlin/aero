# Win7 (WDDM 1.1) D3D11 Map/Unmap semantics (UMD `pfnMap`/`pfnUnmap` + runtime `LockCb`/`UnlockCb`)

This document defines the **behavioral contract** AeroGPU must implement for **`Map`/`Unmap` on Windows 7** (WDDM 1.1) when implementing the **D3D11 user-mode display driver (UMD)**.

It exists to stop future implementers from reverse‑engineering Win7 runtime behavior repeatedly, and to ensure the AeroGPU stack satisfies the staging-readback patterns used by:

* `drivers/aerogpu/tests/win7/d3d11_triangle/`
* `drivers/aerogpu/tests/win7/readback_sanity/`
* `drivers/aerogpu/tests/win7/d3d11_map_roundtrip/`
* `drivers/aerogpu/tests/win7/d3d11_map_do_not_wait/`

> Header references: symbol/type names in this doc are from Win7-era user-mode DDI headers:
> `d3d11umddi.h` (D3D11 UMD DDI), `d3d10umddi.h` (some D3D10-era shared DDI enums/types still used by D3D11 on Win7), and `d3dumddi.h` (common runtime callback types like `D3DDDICB_LOCK`).
>
> For overall D3D10/11 bring-up context, see [`win7-d3d10-11-umd-minimal.md`](./win7-d3d10-11-umd-minimal.md).

---

## 0) What “Map/Unmap” means on Win7 WDDM (high-level)

On Windows 7, `ID3D11DeviceContext::Map` **does not** mean “driver returns a pointer to some driver-owned allocation”. In the WDDM model the **runtime + VidMm own allocation residency and CPU mappings**.

The concrete contract is:

1. The D3D11 runtime calls the UMD DDI entrypoint `D3D11DDI_DEVICECONTEXTFUNCS::pfnMap` / `pfnUnmap`.
2. The UMD implements `pfnMap` by calling back into the runtime via `pfnLockCb` (and `pfnUnmap` via `pfnUnlockCb`) using the `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK` structures.
3. The runtime uses the KMD’s WDDM callbacks (notably `DxgkDdiLock` / `DxgkDdiUnlock`) plus scheduler/fence state to:
   * block until it is safe to expose the memory to CPU (unless DO_NOT_WAIT), and
   * return a CPU pointer + pitch metadata.

**AeroGPU implication:** for deterministic staging readback and correct DO_NOT_WAIT behavior, the stack must make the runtime’s LockCb path “real”:

* fences must advance correctly, and
* KMD lock/unlock must behave consistently (details in §6).

### Bring-up fallback: host-owned Map/Unmap without LockCb

During early bring-up it is possible for the AeroGPU Win7 KMD to *not* expose `DxgkDdiLock` / `DxgkDdiUnlock` yet. In that case the runtime’s `pfnLockCb` / `pfnUnlockCb` path may be absent or fail, which would normally make `Map` unusable for dynamic updates.

AeroGPU supports a **host-owned** update path for resources where the command stream sets:

* `backing_alloc_id = 0` (host-allocated resource; no guest allocation table indirection)

For these resources, the UMD may implement write-style maps (`WRITE`, `WRITE_DISCARD`, `WRITE_NO_OVERWRITE`) by returning a pointer into an in-UMD shadow buffer (`Resource::storage`) and then emitting `AEROGPU_CMD_UPLOAD_RESOURCE` on `Unmap` to push the updated bytes to the host.

Implications:

* This fallback is intentionally **not** used for staging readback (`Map(READ)` / `READ_WRITE` on `D3D11_USAGE_STAGING`) because returning stale shadow bytes would violate readback correctness.
* For host-owned write maps, the UMD can succeed without waiting even if the runtime lock path reports “still drawing”, because the CPU pointer is not a direct mapping of the WDDM allocation.

---

## 1) DDI entrypoints involved (UMD entrypoints + runtime callbacks)

### 1.1 UMD DDI entrypoints (called by D3D11 runtime)

The D3D11 runtime calls these UMD entrypoints through the immediate-context function table:

* `D3D11DDI_DEVICECONTEXTFUNCS::pfnMap`
* `D3D11DDI_DEVICECONTEXTFUNCS::pfnUnmap`

Depending on the negotiated `D3D11DDI_INTERFACE_VERSION`, the Win7 runtime may also route specific Map patterns through additional entrypoints that should forward to the same underlying implementation/semantics:

* Staging helpers:
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnStagingResourceMap`
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnStagingResourceUnmap`
* Dynamic buffer helpers:
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferMapDiscard`
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferMapNoOverwrite`
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicIABufferUnmap`
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicConstantBufferMapDiscard`
  * `D3D11DDI_DEVICECONTEXTFUNCS::pfnDynamicConstantBufferUnmap`

The Win7-era `d3d11umddi.h` prototypes are conceptually:

```c
HRESULT APIENTRY pfnMap(
    D3D11DDI_HDEVICECONTEXT hContext,
    D3D11DDI_HRESOURCE hResource,
    UINT Subresource,
    /* Win7 WDK uses D3D10-era DDI types here; values match D3D11_MAP. */ D3D10_DDI_MAP MapType,
    UINT MapFlags,
    D3D11DDI_MAPPED_SUBRESOURCE* pMapped);

void APIENTRY pfnUnmap(
    D3D11DDI_HDEVICECONTEXT hContext,
    D3D11DDI_HRESOURCE hResource,
    UINT Subresource);
```

Notes:

* Depending on the negotiated D3D11 DDI interface version, `pfnMap` may return
  `HRESULT` or be `void`.
  * If it returns `HRESULT`, it must return `DXGI_ERROR_WAS_STILL_DRAWING` when
    `D3D11_MAP_FLAG_DO_NOT_WAIT` is set and the map would block.
  * If it is `void`, failures (including `DXGI_ERROR_WAS_STILL_DRAWING`) must be
    reported via `pfnSetErrorCb`.
* `pfnUnmap` is typically `void` (errors must be reported via `pfnSetErrorCb`).

### 1.2 Runtime callback table entries used by Map/Unmap

The runtime exposes callback tables to the UMD during device creation (`D3D11DDIARG_CREATEDEVICE`):

* `D3D11DDIARG_CREATEDEVICE::pCallbacks` / `pDeviceCallbacks` → `D3D11DDI_DEVICECALLBACKS` (D3D11 wrapper callbacks)
* Some header revisions also expose `D3D11DDIARG_CREATEDEVICE::pUMCallbacks` → `D3DDDI_DEVICECALLBACKS` (shared WDDM submission callbacks from `d3dumddi.h`)

For exact field names across Win7 WDK revisions (and a probe tool you can build against your installed headers), see:

* [`win7-d3d10-11-umd-callbacks-and-fences.md`](./win7-d3d10-11-umd-callbacks-and-fences.md)
* [`drivers/aerogpu/tools/win7_wdk_probe`](../../drivers/aerogpu/tools/win7_wdk_probe/README.md) (prints `sizeof`/`offsetof` for `D3DDDICB_LOCK` / `D3DDDICB_UNLOCK` and the CreateResource allocation structs)

Map/Unmap uses at least:

* `pfnLockCb` with `D3DDDICB_LOCK`
* `pfnUnlockCb` with `D3DDDICB_UNLOCK`
* `pfnSetErrorCb` (required for `pfnUnmap` error reporting and other void DDIs)

In Win7-era header sets, these callbacks are typically declared as `HRESULT`-returning functions that take the runtime device handle first:

```c
HRESULT APIENTRY pfnLockCb(D3D10DDI_HRTDEVICE hRTDevice, D3DDDICB_LOCK* pLock);
HRESULT APIENTRY pfnUnlockCb(D3D10DDI_HRTDEVICE hRTDevice, D3DDDICB_UNLOCK* pUnlock);
```

Important details:

* `D3DDDICB_LOCK::hAllocation` / `D3DDDICB_UNLOCK::hAllocation` is a `D3DKMT_HANDLE` (a 32-bit integer handle even on x64), **not** a pointer.
* Field spellings in `D3DDDICB_LOCK` / `D3DDDICB_LOCKFLAGS` vary across header revisions; build against your chosen WDK and use the exact member names it defines (see `win7_wdk_probe` link above).

For synchronization/fence-based implementations, the shared callback table (or an embedded equivalent) also provides (names vary slightly by interface version, but the Win7-era concept is consistent):

* `pfnWaitForSynchronizationObjectCb` / `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT`
* `pfnSignalSynchronizationObjectCb` / `D3DDDICB_SIGNALSYNCHRONIZATIONOBJECT`
* (optionally) CPU-specific wait/signal variants such as `...FROMCPU`

**Important:** even if AeroGPU chooses to wait via explicit fence callbacks, `pfnLockCb`/`pfnUnlockCb` must still be correct because the runtime uses them for:

* returning the actual CPU pointer and pitch, and
* (when not using explicit waits) the default “stall until safe” behavior.

---

## 2) `D3D11DDIARG_MAP` / `D3D11DDIARG_UNMAP` field breakdown (Win7-era headers)

Some Win7 D3D11 documentation refers to the Map/Unmap argument bundle as `D3D11DDIARG_MAP` / `D3D11DDIARG_UNMAP`.
In practice, the Win7-era D3D11 UMD DDI passes Map/Unmap as **flat arguments** (rather than a single `*ARG_*` struct), but the logical field breakdown is the same.

### 2.1 `pfnMap` arguments

`pfnMap` describes *which* subresource to map and *how* the caller wants to access it.

* `hResource` (`D3D11DDI_HRESOURCE`)
  * The runtime-provided handle for the resource being mapped.
  * The UMD must translate this to its private resource object and (ultimately) to the underlying WDDM allocation(s) that `pfnLockCb` understands.
* `Subresource` (`UINT`)
  * `D3D11CalcSubresource(MipSlice, ArraySlice, MipLevels)` encoding.
  * For buffers, this is typically `0`.
* `MapType` (DDI enum; typically `D3D10_DDI_MAP` on Win7, values mirroring `D3D11_MAP`)
  * One of:
    * `D3D11_MAP_READ`
    * `D3D11_MAP_WRITE`
    * `D3D11_MAP_READ_WRITE`
    * `D3D11_MAP_WRITE_DISCARD`
    * `D3D11_MAP_WRITE_NO_OVERWRITE`
* `MapFlags` (`UINT`)
  * D3D11 only defines one public map flag on Win7: `D3D11_MAP_FLAG_DO_NOT_WAIT`.
  * The DDI receives the same semantic flag (see §3.2).

### 2.2 `pfnUnmap` arguments

`pfnUnmap` identifies the mapping to end:

* `hResource` (`D3D11DDI_HRESOURCE`)
* `Subresource` (`UINT`)

The UMD must treat `(hResource, Subresource)` as the key and ensure it matches the last successful map.

### 2.3 `D3D11DDI_MAPPED_SUBRESOURCE` (output of `pfnMap`)

`pfnMap` must fill:

* `pData` (`void*`)
* `RowPitch` (`UINT`) — bytes per row for textures (for buffers can be set to `ByteWidth` or `0`; prefer deterministic values)
* `DepthPitch` (`UINT`) — bytes per 2D slice for 3D textures (for 2D textures can be `RowPitch * Height`)

For the `d3d11_triangle` / `readback_sanity` staging readback path, **`RowPitch` must be correct** for `DXGI_FORMAT_B8G8R8A8_UNORM` staging textures because the tests index pixels using the returned pitch.

---

## 3) Mapping: `D3D11_MAP_*` → `D3DDDICB_LOCK` flags

### 3.1 The principle

On Win7, a D3D11 `Map` call is implemented by translating the map request to a runtime `pfnLockCb` request:

* `D3D11_MAP_*` → `D3DDDICB_LOCKFLAGS` (`ReadOnly`, `Discard`, `NoOverwrite`, …)
* `D3D11_MAP_FLAG_DO_NOT_WAIT` → `D3DDDICB_LOCKFLAGS::{DoNotWait, DonotWait}` (header spelling varies)

### 3.2 Map-type table

The table below is the required translation for correctness and to match runtime expectations.

| API MapType (`D3D11_MAP`) | `D3DDDICB_LOCKFLAGS::ReadOnly` | `...::Write` (WriteOnly/Write flag) | `...::Discard` | `...::NoOverwrite` |
|---|---:|---:|---:|---:|
| `D3D11_MAP_READ` | 1 | 0 | 0 | 0 |
| `D3D11_MAP_WRITE` | 0 | 1 | 0 | 0 |
| `D3D11_MAP_READ_WRITE` | 0 | 0 (read+write) | 0 | 0 |
| `D3D11_MAP_WRITE_DISCARD` | 0 | 1 | 1 | 0 |
| `D3D11_MAP_WRITE_NO_OVERWRITE` | 0 | 1 | 0 | 1 |

Notes:

* The exact “write” bit name in `D3DDDICB_LOCKFLAGS` is header-version dependent (commonly `WriteOnly`). The semantic requirement is the same: the lock must be treated as CPU-write.
* `WRITE_DISCARD` and `WRITE_NO_OVERWRITE` are meaningful for dynamic resources (see §5). For other usages, treat them as invalid.

### 3.3 Map-flag table (DO_NOT_WAIT)

| API flag | `D3DDDICB_LOCKFLAGS` bit | Required return on contention |
|---|---|---|
| `D3D11_MAP_FLAG_DO_NOT_WAIT` | `DoNotWait/DonotWait = 1` | `DXGI_ERROR_WAS_STILL_DRAWING` |

Required behavior:

* If DO_NOT_WAIT is set, the UMD **must not block** inside `pfnMap`.
* The UMD must attempt `pfnLockCb` with `DoNotWait/DonotWait = 1`.
* If the runtime reports the allocation is still in use (i.e. the lock would block), `pfnMap` must return `DXGI_ERROR_WAS_STILL_DRAWING` (not `S_FALSE`, not `E_FAIL`).
  * Note: the runtime/KMD may report “would block” using different HRESULTs on Win7/WDDM 1.1 (observed values include `DXGI_ERROR_WAS_STILL_DRAWING`, `HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)`, `E_PENDING`, and various timeout HRESULTs like `HRESULT_FROM_WIN32(WAIT_TIMEOUT)` / `HRESULT_FROM_NT(STATUS_TIMEOUT)`). When DO_NOT_WAIT is requested, normalize these “busy” results to `DXGI_ERROR_WAS_STILL_DRAWING` so the D3D11 API sees the required error code.

Practical Win7 note: different WDK/runtime combinations do not always return `DXGI_ERROR_WAS_STILL_DRAWING` directly from `pfnLockCb`/fence waits when `DO_NOT_WAIT` is set. Treat the common "would block" variants as equivalent to still-drawing and return `DXGI_ERROR_WAS_STILL_DRAWING` to the API:

* `HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)` (`0xD01E0102`)
* `HRESULT_FROM_WIN32(WAIT_TIMEOUT)`
* `HRESULT_FROM_WIN32(ERROR_TIMEOUT)`
* `HRESULT_FROM_NT(STATUS_TIMEOUT)` (`0x10000102`; note that this is `SUCCEEDED()` and must be checked explicitly)
* `E_PENDING` (`0x8000000A`) (observed in some poll-style wait paths; typically for DO_NOT_WAIT / `Timeout = 0`)

---

## 4) Synchronization rules (the part that makes staging readback work)

### 4.1 Staging readback: must wait (unless DO_NOT_WAIT)

The staging readback used by `d3d11_triangle` and `readback_sanity` is:

1. Draw into a DEFAULT render target (or swapchain backbuffer).
2. Create a `D3D11_USAGE_STAGING` texture with `CPU_ACCESS_READ`.
3. `CopyResource(staging, renderTarget)`.
4. `Flush()`.
5. `Map(staging, 0, D3D11_MAP_READ, 0, &mapped)` and read pixels.

The Win7 correctness requirement is:

* `Map(READ)` on `D3D11_USAGE_STAGING + CPU_ACCESS_READ` **must not return until the GPU has completed all prior work that writes the staging resource**, including the `CopyResource`.
* The returned `pData` must contain the **final bytes** produced by the GPU copy.

Practical AeroGPU guidance:

* Avoid waiting on (or polling) the device’s “latest submitted fence” for staging readback.
  Instead, track a **per-resource fence** (`last_gpu_write_fence`) that is updated only when a command that *writes that resource* is recorded/submitted (e.g. `CopyResource(staging, …)` / `CopySubresourceRegion`).
  * This prevents `Map(DO_NOT_WAIT)` from spuriously returning `DXGI_ERROR_WAS_STILL_DRAWING` due to unrelated in-flight work.
  * It also reduces unnecessary stalls when a different command stream is still executing but the staging destination is already complete.

Unless:

* `D3D11_MAP_FLAG_DO_NOT_WAIT` is set, in which case:
  * if the copy hasn’t completed yet, return `DXGI_ERROR_WAS_STILL_DRAWING` and do not block.

### 4.2 “Force submit” rule before waiting

In a command-buffering UMD (including AeroGPU), it is possible for the UMD to have pending GPU work *in user-mode* that the kernel scheduler has not seen yet.

Therefore, when `pfnMap` needs the GPU to be finished (typically READ/READ_WRITE staging maps), the UMD should:

1. **Flush/submit** pending work that could affect the mapped resource (or its source) *before* calling a blocking lock/wait path.
2. Then wait (via blocking `pfnLockCb` or explicit fence waits).

Why this matters:

* Waiting without submitting first can deadlock (you wait for work that hasn’t been queued).
* It also increases latency (runtime can’t start executing the copy until submission happens).

Practical guidance:

* If the app already called `ID3D11DeviceContext::Flush`, the runtime will typically call your `pfnFlush` before `pfnMap` anyway, but the UMD must not rely on this ordering.
* Treat “Map needing synchronization” as an implicit flush point.

### 4.3 Which maps require GPU synchronization?

For Win7 correctness, at minimum:

* `D3D11_MAP_READ` on a staging resource requires synchronization if the resource might have been written by GPU.
* `D3D11_MAP_READ_WRITE` on staging similarly requires synchronization.

For write maps:

* `WRITE_DISCARD` should avoid synchronization by discarding/renaming (runtime may help if `Discard` is set in lock flags).
* `WRITE_NO_OVERWRITE` should avoid synchronization but requires the app to honor the no-overwrite contract; the driver must not internally “rename” in a way that breaks the API guarantee.

---

## 5) Resource-usage validation rules (legal MapTypes by usage)

These are the D3D11 API-level rules the Win7 runtime enforces; the UMD must match them.

### 5.1 Usage table

| `D3D11_USAGE` | Required `CPUAccessFlags` | Legal MapTypes | Notes |
|---|---|---|---|
| `D3D11_USAGE_DEFAULT` | `0` | *(none)* | CPU mapping is not allowed. Use `UpdateSubresource` / `Copy*` instead. |
| `D3D11_USAGE_IMMUTABLE` | `0` | *(none)* | Never mappable; contents fixed at creation. |
| `D3D11_USAGE_DYNAMIC` | `D3D11_CPU_ACCESS_WRITE` | `WRITE_DISCARD`, `WRITE_NO_OVERWRITE` | The “dynamic upload” path for VB/IB/CB updates. |
| `D3D11_USAGE_STAGING` | `D3D11_CPU_ACCESS_READ` | `READ` | Readback staging. Must synchronize (see §4). |
| `D3D11_USAGE_STAGING` | `D3D11_CPU_ACCESS_WRITE` | `WRITE` | CPU-only upload staging; later copied to DEFAULT resource. |
| `D3D11_USAGE_STAGING` | `D3D11_CPU_ACCESS_READ \| D3D11_CPU_ACCESS_WRITE` | `READ_WRITE` | Less common; treat as valid. |

Other validation:

* `D3D11_MAP_FLAG_DO_NOT_WAIT` is only legal if it’s the only bit set in `Flags`. Unknown bits must fail.
* `Subresource` must be within the resource’s subresource count.
* `pfnMap` must fail if the same subresource is already mapped.

### 5.2 Error reporting for invalid Map/Unmap

* `pfnMap` must return an `HRESULT`:
  * `E_INVALIDARG` for illegal MapType/Flags/usage/subresource combinations.
  * `DXGI_ERROR_WAS_STILL_DRAWING` for DO_NOT_WAIT contention.
* `pfnUnmap` is `void`:
  * if the Unmap arguments are invalid (unknown resource, bad subresource, Unmap without a prior successful Map), report via the runtime callback `pfnSetErrorCb(<device-handle>, E_INVALIDARG)` (using whatever handle type your headers declare: `HRTDEVICE` vs `HDEVICE`) and return.

Do **not** silently ignore invalid Unmap in AeroGPU; hiding these errors makes runtime/device-state bugs extremely difficult to diagnose.

---

## 6) KMD-side notes (why `DxgkDdiLock/Unlock` matter and what “coherent” means)

### 6.1 Why `DxgkDdiLock` / `DxgkDdiUnlock` are usually required

On Win7 WDDM, the runtime’s `pfnLockCb` typically relies on the KMD’s `DxgkDdiLock` / `DxgkDdiUnlock` implementation to:

* validate that the allocation is CPU-mappable,
* return a stable CPU virtual address for the allocation/subresource,
* enforce synchronization against in-flight GPU usage (or return “still drawing” for DO_NOT_WAIT), and
* apply cache policy / flushing rules.

If AeroGPU’s KMD does not implement Lock/Unlock correctly, common failure modes include:

* `Map(READ)` returning stale data (copy completed on “GPU”, but CPU reads old bytes)
* DO_NOT_WAIT never returning `DXGI_ERROR_WAS_STILL_DRAWING` (leading to app hangs or unexpected stalls)
* runtime returning a pointer that becomes invalid or aliases unrelated memory

The minimal AeroGPU KMD architecture doc already calls out `DxgkDdiLock/Unlock` as required plumbing for CPU access (see §4.3 in [`win7-wddm11-aerogpu-driver.md`](./win7-wddm11-aerogpu-driver.md)).

### 6.2 Cache coherency expectations for staging readback

For staging readback correctness, the following must be true:

* When the UMD returns from `pfnMap(READ)` successfully, the bytes visible at `pMapped->pData` must reflect the **completed GPU write**.
* If AeroGPU’s emulator/host writes into guest allocations (system memory), the fence completion signal must not occur until those writes are globally visible to the CPU thread that will read them.

In other words: **“Fence complete” implies “data visible to CPU”** for readback destinations.

For a virtual GPU this often means:

* perform the host-side copy into the guest allocation memory *before* raising the completion interrupt / advancing the completed fence, and
* use appropriate host memory ordering primitives so the guest CPU thread cannot observe completion without observing the writes.

---

## 7) Pseudocode: recommended `pfnMap` / `pfnUnmap` control flow

This pseudocode is intentionally explicit about where flushing, locking, and error handling occur.

### 7.1 `pfnMap`

```c
HRESULT APIENTRY Map(hContext, hResource, Subresource, MapType, MapFlags, pOut) {
  if (!pOut) return E_INVALIDARG;

  Resource* res = lookup_resource(hResource);
  if (!res) return E_INVALIDARG;

  if (!validate_subresource(res, Subresource)) return E_INVALIDARG;
  if (!validate_usage_and_map_type(res, MapType, MapFlags)) return E_INVALIDARG;

  bool do_not_wait = (MapFlags & D3D11_MAP_FLAG_DO_NOT_WAIT) != 0;
  bool needs_gpu_sync = map_requires_sync(res, MapType);

  if (needs_gpu_sync) {
    // Important: submit any pending GPU work that may produce the readback bytes.
    flush_pending_work(hContext);
  }

  // Translate to runtime lock.
  D3DDDICB_LOCK lock = {};
  lock.hAllocation = res->allocation_handle;
  // Field spelling varies by WDK revision (`SubResourceIndex` / `SubresourceIndex`);
  // use the exact name exposed by the header you build against.
  lock.SubresourceIndex = Subresource;
  lock.Flags = translate_map_to_lockflags(MapType);
  // Field spelling varies by WDK revision (`DoNotWait` / `DonotWait`); use the
  // exact name exposed by the header you build against.
  lock.Flags.DoNotWait = do_not_wait ? 1 : 0;

  HRESULT hr = callbacks->pfnLockCb(hRTDevice, &lock);
  if (do_not_wait && (hr == DXGI_ERROR_WAS_STILL_DRAWING ||
                      hr == HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY) ||
                      hr == E_PENDING ||
                      is_timeout_hr(hr))) {
    // The D3D11 API contract requires DXGI_ERROR_WAS_STILL_DRAWING for DO_NOT_WAIT.
    // Some Win7/WDDM 1.1 paths report “busy” using other HRESULTs; normalize them.
    return DXGI_ERROR_WAS_STILL_DRAWING;
  }
  if (FAILED(hr)) return hr;

  // The runtime filled lock.pData + pitch metadata.
  pOut->pData = lock.pData;
  pOut->RowPitch = lock.Pitch;
  pOut->DepthPitch = lock.SlicePitch;

  mark_mapped(res, Subresource, MapType);
  return S_OK;
}
```

### 7.2 `pfnUnmap`

```c
void APIENTRY Unmap(hContext, hResource, Subresource) {
  Resource* res = lookup_resource(hResource);
  if (!res || !is_mapped(res, Subresource)) {
    callbacks->pfnSetErrorCb(<device-handle>, E_INVALIDARG);
    return;
  }

  // If this was a write map, ensure subsequent GPU use sees the data.
  // (In AeroGPU, this may mean emitting an upload command or marking the
  // allocation as dirty so the host reads from guest memory on next use.)
  if (last_map_was_write(res, Subresource)) {
    commit_cpu_writes(res, Subresource);
  }

  D3DDDICB_UNLOCK unlock = {};
  unlock.hAllocation = res->allocation_handle;
  // Field spelling varies by WDK revision (`SubResourceIndex` / `SubresourceIndex`);
  // use the exact name exposed by the header you build against.
  unlock.SubresourceIndex = Subresource;

  HRESULT hr = callbacks->pfnUnlockCb(hRTDevice, &unlock);
  if (FAILED(hr)) {
    callbacks->pfnSetErrorCb(<device-handle>, hr);
  }

  clear_mapped(res, Subresource);
}
```

---

## 8) “Definition of done” for AeroGPU Map/Unmap on Win7

An implementation matches Win7 expectations when:

* `drivers/aerogpu/tests/win7/d3d11_triangle` reliably reads the expected center/corner pixels via staging `Map(READ)`.
* `drivers/aerogpu/tests/win7/readback_sanity` reliably reads expected pixels via staging `Map(READ)`.
* `drivers/aerogpu/tests/win7/d3d11_map_roundtrip` reliably round-trips a staging texture via `Map(WRITE)` + `Unmap` + `Map(READ)`.
* `Map(DO_NOT_WAIT)` returns `DXGI_ERROR_WAS_STILL_DRAWING` when the staging destination is still busy (validated by `drivers/aerogpu/tests/win7/d3d11_map_do_not_wait`).
* Invalid Map usage returns `E_INVALIDARG` and invalid Unmap reports `E_INVALIDARG` via `pfnSetErrorCb` (no silent success).
