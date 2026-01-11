# Win7 (WDDM 1.1) D3D10/D3D11 UMD callbacks, submission, and fences (Win7 WDK reference)

This document pins down the **exact Windows 7 (WDDM 1.1) symbol names** (types, struct fields, and callback entrypoints) that matter for a D3D10/D3D11 **user-mode display driver (UMD)** implementing:

- DMA buffer allocation (command buffer acquisition)
- command submission (**render** and **present**)
- error reporting from `void` DDIs
- fence wait/poll for `Map(READ)` (staging readback)
- WOW64 (32-bit UMD on x64) ABI gotchas

It is intended to be used *together with* the Win7-era D3D UMD headers shipped with a Windows SDK/WDK install (WDK10+ supported), including:

- `d3d10umddi.h`, `d3d10_1umddi.h`
- `d3d11umddi.h`
- shared: `d3dumddi.h`, `d3dkmthk.h`

Clean-room note: this document **does not** include sample-driver code. It references only the public WDK DDI contracts and describes call flow and field usage.

Related docs:

- High-level bring-up checklist: `docs/graphics/win7-d3d10-11-umd-minimal.md`
- D3D11 function-table checklist (REQUIRED vs stubbable DDIs): `docs/graphics/win7-d3d11ddi-function-tables.md`
- KMD submission/fence architecture: `docs/graphics/win7-wddm11-aerogpu-driver.md`

---

## 0) Naming conventions / what “callbacks” means here

Windows D3D10/11 UMDs interact with three “layers” of function tables:

1. **UMD exports** (OS loads your DLL and calls these): `OpenAdapter10`, `OpenAdapter10_2`, `OpenAdapter11`.
2. **UMD-provided function tables** (runtime calls into these): `D3D10DDI_ADAPTERFUNCS`/`D3D11DDI_ADAPTERFUNCS`, `D3D10DDI_DEVICEFUNCS`/`D3D11DDI_DEVICEFUNCS`, and `D3D11DDI_DEVICECONTEXTFUNCS`.
3. **Runtime-provided callback tables** (UMD calls into these): `D3D10DDI_ADAPTERCALLBACKS`/`D3D11DDI_ADAPTERCALLBACKS` and `D3D10DDI_DEVICECALLBACKS`/`D3D11DDI_DEVICECALLBACKS` (plus the shared `D3DDDI_DEVICECALLBACKS`-style “CB” entrypoints in `d3dumddi.h`).

This doc focuses on (3): the callbacks the UMD uses for **submission and synchronization**, and where you receive them.

---

## 1) Callback tables provided to the UMD (OpenAdapter + CreateDevice)

### 1.1 OpenAdapter time (adapter callbacks)

**Exports (Win7):**

- D3D10: `HRESULT APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData)`
- D3D10.1: `HRESULT APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData)`
- D3D11 (Win7): `HRESULT APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData)`
  - On Windows 7, `OpenAdapter11` still receives a `D3D10DDIARG_OPENADAPTER` container; the D3D11-specific DDIs begin at device creation/caps.

**The OpenAdapter container: `D3D10DDIARG_OPENADAPTER`**

Fields that matter for submission work later:

- `D3D10DDI_HRTADAPTER hRTAdapter` — runtime-owned adapter handle (opaque to the driver).
- `D3D10DDI_HADAPTER hAdapter` — driver-owned adapter handle (`.pDrvPrivate` points at your adapter object).
- `const D3D10DDI_ADAPTERCALLBACKS* pAdapterCallbacks` — runtime callback table you must store.
- `D3D10DDI_ADAPTERFUNCS* pAdapterFuncs` — output table you fill (at minimum: `pfnCreateDevice`, `pfnCloseAdapter`, `pfnGetCaps`, `pfnCalcPrivateDeviceSize`).
- `UINT Interface` / `UINT Version` — interface/version negotiation.

> The exact adapter callback table type you receive depends on which OpenAdapter export is used:
>
> - D3D10/10.1: `D3D10DDI_ADAPTERCALLBACKS`
> - D3D11: `D3D11DDI_ADAPTERCALLBACKS` (still delivered through `D3D10DDIARG_OPENADAPTER` on Win7).

**Callbacks worth knowing about (for later device bring-up):**

- `pfnQueryAdapterInfoCb`-style callbacks (adapter info queries)
- allocations and residency are typically device-scoped (covered below), not adapter-scoped

For *submission* specifically, you mostly care about storing `hRTAdapter` and getting to `CreateDevice`, where the device callbacks are provided.

### 1.2 CreateDevice time (device callbacks)

#### D3D10: `D3D10DDIARG_CREATEDEVICE`

The runtime calls your `D3D10DDI_ADAPTERFUNCS::pfnCreateDevice(...)`, passing a `D3D10DDIARG_CREATEDEVICE`.

Fields that matter for submission/sync:

- `D3D10DDI_HDEVICE hDevice` — driver device handle (where your device object lives).
- `D3D10DDI_HRTDEVICE hRTDevice` — runtime device handle (store it; needed for `pfnSetErrorCb`).
- `const D3D10DDI_DEVICECALLBACKS* pCallbacks` — runtime callback table for:
  - reporting errors from `void` DDIs (`pfnSetErrorCb`)
  - creating contexts, acquiring DMA buffers, submitting render/present, and waiting fences (via `d3dumddi.h` “CB” entrypoints)
- `D3D10DDI_DEVICEFUNCS* pDeviceFuncs` — output function table you fill.

#### D3D11: `D3D11DDIARG_CREATEDEVICE`

The runtime calls your `D3D11DDI_ADAPTERFUNCS::pfnCreateDevice(...)`, passing a `D3D11DDIARG_CREATEDEVICE`.

Fields that matter for submission/sync:

- `D3D11DDI_HDEVICE hDevice`
- `D3D11DDI_HRTDEVICE hRTDevice` (still the handle passed to `pfnSetErrorCb`)
- `D3D11DDI_HDEVICECONTEXT hImmediateContext` (Win7 immediate context handle you own)
- `const D3D11DDI_DEVICECALLBACKS* pCallbacks`
- output tables:
  - `D3D11DDI_DEVICEFUNCS* pDeviceFuncs`
  - `D3D11DDI_DEVICECONTEXTFUNCS* pDeviceContextFuncs`

> **Why this matters:** the D3D11 runtime will call `pfnMap`/`pfnFlush` on the device-context table, so your fence tracking must be reachable from the context object too.

---

## 2) Error reporting from `void` DDIs (Win7 D3D10/D3D11)

Many D3D10/D3D11 DDIs are declared `void APIENTRY ...(...)` and **cannot return** an `HRESULT`.

### 2.1 The callback: `pfnSetErrorCb`

On WDDM 1.1 / Win7, the D3D10/D3D11 runtimes provide an error callback named:

- `pfnSetErrorCb`

It is reachable from the **device callbacks** you receive during `CreateDevice`:

- D3D10: `D3D10DDIARG_CREATEDEVICE::pCallbacks->pfnSetErrorCb`
- D3D11: `D3D11DDIARG_CREATEDEVICE::pCallbacks->pfnSetErrorCb`

**Signature (conceptually):**

- input: `D3D10DDI_HRTDEVICE` / `D3D11DDI_HRTDEVICE` plus an `HRESULT` describing the failure.

### 2.2 Rule: “set error then return”

When a `void` DDI encounters an error:

1. Call `pfnSetErrorCb(hRTDevice, hr)`.
2. Return immediately (do not continue executing the DDI).

The runtime associates the error with the originating API call.

### 2.3 Acceptable `HRESULT` values (practical Win7 set)

Use **specific** errors. The common “safe set” for Win7 bring-up:

- `E_OUTOFMEMORY` — allocation failure (including inability to get a DMA buffer).
- `E_INVALIDARG` — runtime provided invalid arguments (should be rare; runtime usually validates).
- `E_NOTIMPL` — feature not implemented but the call was reached anyway.

Device-removal style errors are also valid but should be used only for genuine “device is broken” scenarios:

- `DXGI_ERROR_DEVICE_REMOVED`
- `DXGI_ERROR_DEVICE_HUNG`
- `DXGI_ERROR_DEVICE_RESET`

Avoid `E_FAIL` for predictable conditions; it makes debugging harder and can push the runtime into harsh recovery paths.

---

## 3) Win7 submission model: acquire DMA buffer → fill → submit

On Win7/WDDM 1.1, a D3D10/11 UMD submits work by building:

- a **DMA buffer** (your command stream)
- an **allocation list** describing referenced allocations
- an optional **patch-location list** (relocations)
- optional **DMA-buffer private data** (per-submission sideband blob)

Then it submits via the runtime callbacks which route into:

- KMD `DxgkDdiRender` for “render” submissions, or
- KMD `DxgkDdiPresent` for “present” submissions,

followed by dxgkrnl scheduling and eventual KMD `DxgkDdiSubmitCommand`.

### 3.1 Create the kernel device + context (get `hContext`, `hSyncObject`, and initial DMA buffer pointers)

The submission callbacks in `d3dumddi.h` are **context-scoped**: before you can submit anything, you need:

- a kernel device handle (`D3DKMT_HANDLE hDevice`),
- a kernel context handle (`D3DKMT_HANDLE hContext`), and
- a synchronization object (`D3DKMT_HANDLE hSyncObject`) you can wait on with a target fence value.

On Win7, these are created via callbacks in the shared device callback table:

- `D3DDDI_DEVICECALLBACKS::pfnCreateDeviceCb`
- `D3DDDI_DEVICECALLBACKS::pfnCreateContextCb2` (preferred on WDDM 1.1) or `pfnCreateContextCb`

#### Create the kernel device: `pfnCreateDeviceCb` + `D3DDDICB_CREATEDEVICE`

Struct:

- `D3DDDICB_CREATEDEVICE`

Important fields:

- `HANDLE hAdapter` (input) — the adapter handle you returned from `OpenAdapter*` (for D3D10/11 this is typically the `.pDrvPrivate` pointer behind `D3D10DDI_HADAPTER` / `D3D11DDI_HADAPTER`).
- `D3DKMT_HANDLE hDevice` (output) — kernel device handle; store it.

#### Create the kernel context: `pfnCreateContextCb2`/`pfnCreateContextCb` + `D3DDDICB_CREATECONTEXT`

Struct:

- `D3DDDICB_CREATECONTEXT`

Important inputs:

- `D3DKMT_HANDLE hDevice` — the kernel device handle from `pfnCreateDeviceCb`.
- `UINT NodeOrdinal` — set to `0` for a single-node MVP.
- `UINT EngineAffinity` — set to `0` for a single-engine MVP.
- `Flags` — **zero-initialize** for bring-up unless you know you need a bit.
- `VOID* pPrivateDriverData` / `UINT PrivateDriverDataSize` — bring-up can pass `NULL`/`0` unless your KMD needs context-private data.

Important outputs:

- `D3DKMT_HANDLE hContext` — kernel context handle; pass this to render/present/wait CBs.
- `D3DKMT_HANDLE hSyncObject` — monitored-fence synchronization object; pass this in wait calls.
- Initial submission buffers (owned by the runtime; treat as the “current DMA buffer”):
  - `VOID* pCommandBuffer` + `UINT CommandBufferSize` (**bytes**)
  - `D3DDDI_ALLOCATIONLIST* pAllocationList` + `UINT AllocationListSize` (**entries**)
  - `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList` + `UINT PatchLocationListSize` (**entries**)
  - If your header exposes it: `VOID* pDmaBufferPrivateData` + `UINT DmaBufferPrivateDataSize` (**bytes**)

> **Key Win7 rule:** the runtime is allowed to **rotate** DMA buffers and lists over time. After each submission, update your stored pointers/sizes from whatever “out” fields your header exposes (see render/present notes below).

#### Lifetime / cleanup callbacks

At shutdown, these additional `D3DDDI_DEVICECALLBACKS` entries may exist (check for presence in your headers):

- `pfnDestroySynchronizationObjectCb` (takes a struct with `hSyncObject`)
- `pfnDestroyContextCb` (takes a struct with `hContext`)
- `pfnDestroyDeviceCb` (takes a struct with `hDevice`)

### 3.2 The core submission structs (d3dumddi.h)

The *shared* WDDM 1.x CB structs used by D3D10/11 are declared in `d3dumddi.h`:

The corresponding **function pointers** are in the shared runtime callback table:

- `D3DDDI_DEVICECALLBACKS`
  - `pfnCreateDeviceCb`
  - `pfnCreateContextCb2` / `pfnCreateContextCb`
  - `pfnGetCommandBufferCb`
  - `pfnRenderCb`
  - `pfnPresentCb`
  - `pfnWaitForSynchronizationObjectCb`

#### Acquire / (re)acquire a command buffer

Callback:

- `pfnGetCommandBufferCb`

Struct:

- `D3DDDICB_GETCOMMANDINFO`

CreateContext already provides the **initial** `pCommandBuffer` / lists. `pfnGetCommandBufferCb` is the runtime entrypoint used to acquire a *fresh* DMA buffer instance (and is the standard place where the UMD receives a pointer to `pDmaBufferPrivateData`).

Important fields (header names):

- `D3DKMT_HANDLE hContext` — kernel context handle to build commands for.
- output pointers (memory owned by runtime/OS for this DMA buffer instance):
  - `VOID* pCommandBuffer`
  - `D3DDDI_ALLOCATIONLIST* pAllocationList`
  - `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList`
  - `VOID* pDmaBufferPrivateData`
- output capacities (max sizes you are allowed to write):
  - `UINT CommandBufferSize` (bytes)
  - `UINT AllocationListSize` (count of `D3DDDI_ALLOCATIONLIST` entries)
  - `UINT PatchLocationListSize` (count of `D3DDDI_PATCHLOCATIONLIST` entries)
  - `UINT DmaBufferPrivateDataSize` (bytes)

> The capacity fields are critical: **do not write past them**. If you need more space, end the current buffer and submit, then acquire a new one.

#### Submit a render DMA buffer

Callback:

- `pfnRenderCb`

Struct:

- `D3DDDICB_RENDER`

Important fields:

- `D3DKMT_HANDLE hContext`
- `UINT CommandLength` (bytes written to `pCommandBuffer`)
- `VOID* pCommandBuffer`
- `UINT CommandBufferSize` (bytes; some WDKs include this as an in/out field)
- `UINT AllocationListSize` (count) + `D3DDDI_ALLOCATIONLIST* pAllocationList`
- `UINT PatchLocationListSize` (count) + `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList`
- `VOID* pDmaBufferPrivateData`

Fence output (Win7 pattern):

- `UINT64 NewFenceValue` (written by the callback on success; use as the target value when waiting for completion via `WaitForSynchronizationObject`)

> **Buffer rotation:** in some Win7-era header revisions, `D3DDDICB_RENDER` treats the buffer/list pointer+size fields as **IN/OUT** (you pass the current buffers, and on return the runtime may overwrite them with the next buffers/capacities). If your header has this behavior, update your stored `pCommandBuffer` / `pAllocationList` / `pPatchLocationList` and their capacities after each successful submit.

#### Submit a present DMA buffer

Callback:

- `pfnPresentCb`

Struct:

- `D3DDDICB_PRESENT`

Important common submission fields (present has additional present-specific fields; see the header):

- `D3DKMT_HANDLE hContext`
- `UINT CommandLength` (bytes)
- `VOID* pCommandBuffer`
- `UINT CommandBufferSize` (bytes; some WDKs include this as an in/out field)
- `UINT AllocationListSize` (count) + `D3DDDI_ALLOCATIONLIST* pAllocationList`
- `UINT PatchLocationListSize` (count) + `D3DDDI_PATCHLOCATIONLIST* pPatchLocationList`
- `VOID* pDmaBufferPrivateData`

Fence output (Win7 pattern):

- `UINT64 NewFenceValue` (written by the callback on success)

### 3.3 Minimal call sequence (render submission)

At a “flush boundary” (e.g. `D3D10DDI_DEVICEFUNCS::pfnFlush` or `D3D11DDI_DEVICECONTEXTFUNCS::pfnFlush`):

1. **Ensure you have a context** (once per device, at bring-up):
   - `pfnCreateDeviceCb` → kernel `hDevice`
   - `pfnCreateContextCb2`/`pfnCreateContextCb` → `hContext`, `hSyncObject`, and an initial `pCommandBuffer` + list pointers/capacities.
2. **Acquire** a command buffer (per submission, if you use `pfnGetCommandBufferCb` in your design):
   - Fill a `D3DDDICB_GETCOMMANDINFO` with `hContext`.
   - Call `pfnGetCommandBufferCb(&get)`.
   - Otherwise, use the “current” `pCommandBuffer`/lists you last received from `CreateContext` or from the previous submit callback.
3. **Fill**:
   - Write your DMA stream to `pCommandBuffer`.
   - Write allocation references into `pAllocationList[0..N)`.
   - Write patch entries into `pPatchLocationList[0..M)` (for AeroGPU, typically `M=0`).
   - If `pDmaBufferPrivateData != NULL`, write per-submit metadata into it (fixed-size).
4. **Submit**:
    - Fill `D3DDDICB_RENDER`:
      - `hContext = ...`
      - `CommandLength = <bytes actually written>`
      - `pCommandBuffer = pCommandBuffer`
      - `AllocationListSize = N`, `pAllocationList = pAllocationList`
      - `PatchLocationListSize = M`, `pPatchLocationList = pPatchLocationList`
      - `pDmaBufferPrivateData = pDmaBufferPrivateData` (or `NULL` if not used / size is 0)
    - Call `pfnRenderCb(&render)`.
    - On success:
      - read back `render.NewFenceValue` and treat it as the fence value for this submission (store it as “last submitted”, and use it to update per-resource “last write fence” tracking)
      - if your header treats buffer/list fields as in/out, update your stored pointers/capacities from `render` for the next submission.

### 3.4 Minimal call sequence (present submission)

In `D3D10DDI_DEVICEFUNCS::pfnPresent` (called by DXGI on Win7 for both D3D10 and D3D11 devices):

1. Flush/submit any outstanding render work that must precede present.
2. Acquire a command buffer (either via `pfnGetCommandBufferCb` or by using the current runtime-provided buffer pointers).
3. Encode your present command(s) into the DMA buffer (e.g. an `AEROGPU_CMD_PRESENT` packet referencing the backbuffer allocation index).
4. Submit via `pfnPresentCb(&present)`.
5. On success, read back `present.NewFenceValue` and treat it as the fence value for the present submission (useful for throttling and for “present implies completion” queries).

### 3.5 Patch lists: “empty is valid” if you design for it

If your DMA stream never embeds GPU virtual addresses (AeroGPU uses allocation indices), you can submit with:

- `PatchLocationListSize = 0`
- `pPatchLocationList = NULL` (or a valid pointer with 0 size)

**Do not** put uninitialized junk in the patch list. If `PatchLocationListSize != 0`, dxgkrnl and the KMD may attempt to interpret it.

### 3.6 DMA buffer private data (`pDmaBufferPrivateData`)

The private-data blob is sized by the KMD via `DXGK_DRIVERCAPS::DmaBufferPrivateDataSize`.

Where you receive the pointer depends on the exact Win7-era header/interface revision:

- Common path: `D3DDDICB_GETCOMMANDINFO::pDmaBufferPrivateData` with capacity `DmaBufferPrivateDataSize`
- Some headers also surface it alongside the initial DMA buffer in `D3DDDICB_CREATECONTEXT` and/or treat it as an in/out field on submit structs. In that case, treat it as part of your “current DMA buffer state” just like `pCommandBuffer`.

Rules:

- Treat it as **opaque fixed-size bytes** shared with the KMD.
- Use a fixed-width, pointer-free layout (see WOW64 notes).
- Typical uses:
  - classify submissions (render vs present vs paging)
  - include a tiny “submission header” version for debugging

---

## 4) Fence wait/poll for `Map(READ)` on Win7

### 4.1 What `Map(READ)` needs (D3D11 staging readback)

For `D3D11_MAP_READ` on a staging resource, the UMD must ensure:

- the GPU copy into the staging resource has completed, and
- the CPU mapping observes the completed data.

In the repo’s Win7 tests (`drivers/aerogpu/tests/win7/readback_sanity`), the pattern is:

1. `CopyResource(staging, renderTarget)`
2. `Flush()`
3. `Map(staging, D3D11_MAP_READ, Flags=0)`

So the UMD’s `Map` must block (or poll+block) on a fence.

### 4.2 The Win7 wait callback (preferred): `pfnWaitForSynchronizationObjectCb`

The shared wait CB entrypoint lives in `d3dumddi.h`:

- Callback: `pfnWaitForSynchronizationObjectCb`
- Struct: `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT`

Important fields:

- `D3DKMT_HANDLE hContext` — context whose sync objects/fences are relevant.
- `UINT ObjectCount`
- `const D3DKMT_HANDLE* ObjectHandleArray` (one per sync object)
- `const UINT64* FenceValueArray` (target values; one per sync object)
- `UINT64 Timeout` (milliseconds; `0` is a poll, `~0ULL` is effectively “infinite wait”)

**Which sync object handle to wait on:**

- Use the `hSyncObject` returned by `pfnCreateContextCb2`/`pfnCreateContextCb` (see §3.1). This is the monitored-fence object whose value advances with your submissions.

**How to pick the target fence value:**

- Track a monotonically increasing fence/timeline value per submission.
  - On Win7, this is typically the `NewFenceValue` returned by the last `pfnRenderCb` / `pfnPresentCb` submission that produced the data you need.
- Store “last write fence” on resources that are written by the GPU.
- When mapping for read, wait for `completed >= last_write_fence`.

**Polling (for DO_NOT_WAIT paths):**

- Call the wait callback with `Timeout = 0`.
- If it indicates not-ready (timeout), return `DXGI_ERROR_WAS_STILL_DRAWING` from the `Map` DDI (D3D11) / `D3D10DDIERR_WASSTILLDRAWING`-style equivalent as applicable.

### 4.3 Direct thunk alternative: `D3DKMTWaitForSynchronizationObject`

If you are not using the runtime’s wait callback (e.g., in standalone tooling), you can call the kernel thunk directly:

- Function: `NTSTATUS APIENTRY D3DKMTWaitForSynchronizationObject(D3DKMT_WAITFORSYNCHRONIZATIONOBJECT* pData)`
- Struct: `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT` (in `d3dkmthk.h`)

Important fields (header names):

- `D3DKMT_HANDLE hAdapter`
- `UINT ObjectCount`
- `const D3DKMT_HANDLE* ObjectHandleArray`
- `const UINT64* FenceValueArray`
- `UINT64 Timeout` (**milliseconds**; `0` is a poll, `~0ULL` is effectively “infinite wait”)

The “target fence value” is specified via `FenceValueArray[i]` for each sync object handle in `ObjectHandleArray[i]`.

**Recommendation:** in a real UMD, prefer the runtime callback if available; it keeps the driver insulated from some OS-version quirks and ensures WOW64 thunking is correct.

---

## 5) WOW64 notes (32-bit UMD on x64)

Windows 7 x64 will load **both**:

- a 64-bit UMD for 64-bit processes, and
- a 32-bit UMD under WOW64 for 32-bit processes.

Both UMDs talk to the same 64-bit kernel (dxgkrnl + your x64 KMD). The biggest pitfalls are ABI and “binary blob” layouts.

### 5.1 Handle sizes: pointer-sized vs 32-bit

**Pointer-sized (differs between x86 and x64 UMD builds):**

- D3D10/11 DDI handles like `D3D10DDI_HRESOURCE`, `D3D10DDI_HDEVICE`, `D3D11DDI_HDEVICECONTEXT`, etc:
  - these are wrapper structs containing `.pDrvPrivate` pointers.

**Always 32-bit (even on x64):**

- `D3DKMT_HANDLE` (declared as a `UINT`)
  - used for kernel objects: adapter/device/context/allocation/synchronization object handles.

Rule: never assume `sizeof(D3DKMT_HANDLE) == sizeof(void*)`.

### 5.2 Packing pitfalls

- Do not use `#pragma pack(1)` globally in a UMD; it will break the WDK struct ABI.
- Ensure all compilation units that include `d3d10umddi.h` / `d3d11umddi.h` see the default packing expected by the headers.

### 5.3 The critical cross-arch blob: `pDmaBufferPrivateData`

`pDmaBufferPrivateData` is a **binary packet shared between UMD and KMD**.

On x64:

- the **KMD** is always 64-bit, and it defines the size via `DXGK_DRIVERCAPS::DmaBufferPrivateDataSize`.
- the **32-bit UMD** still receives a buffer of that x64-defined size.

Therefore:

- The private-data layout must be **explicitly architecture-independent**.
- **Do not embed pointers** (user-mode or kernel-mode) in this blob.
- Use fixed-width integers (`uint32_t`, `uint64_t`) and explicit padding if needed.

### 5.4 Calling D3DKMT directly under WOW64

If you call `D3DKMT*` thunks directly:

- call the documented user-mode exports (typically from `gdi32.dll`), not private syscalls
- do not hand-roll struct layouts; include the SDK/WDK `d3dkmthk.h`

This ensures the WOW64 layer performs the correct pointer-size translation for thunk parameter structs.

### 5.5 x86 stdcall export decoration (common loader gotcha)

On x86 (including WOW64), the UMD exports are `__stdcall`, so the *raw* symbol names are decorated with a stack size suffix:

- `_OpenAdapter10@4`
- `_OpenAdapter10_2@4`
- `_OpenAdapter11@4`

However, the runtime uses undecorated names (`"OpenAdapter10"`, etc) with `GetProcAddress`, so your DLL must also export:

- `OpenAdapter10`, `OpenAdapter10_2`, `OpenAdapter11`

In AeroGPU this is handled by `.def` files:

- `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_x86.def` (x86, maps undecorated → decorated)
- `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_x64.def` (x64, no `@N` decoration)

---

## 6) Optional: Win7 WDK layout probe tool (sizeof/offsetof)

To catch header/version mismatches early (especially when switching between SDKs/WDKs or x86/x64),
the repo includes a small Windows-only probe you can build with any toolchain that provides the
Win7-era D3D10/11 UMD DDI headers:

- `drivers/aerogpu/tools/win7_wdk_probe/`

It includes the Win7 D3D10/11 UMD DDI headers and prints `sizeof`/`offsetof` for:

- Context bring-up:
  - `D3DDDICB_CREATEDEVICE`
  - `D3DDDICB_CREATECONTEXT`
- `D3DDDICB_GETCOMMANDINFO`
- `D3DDDICB_RENDER`
- `D3DDDICB_PRESENT`
- `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT`
- `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT`

This is not part of runtime/CI; it is a developer-side sanity check.

---

## Appendix A) “What you actually need to implement” checklist (submission/fences only)

To implement correct Win7 submission + `Map(READ)` synchronization in a D3D10/11 UMD, you will need:

1. Store runtime callbacks from:
   - `D3D10DDIARG_OPENADAPTER::pAdapterCallbacks`
   - `D3D10DDIARG_CREATEDEVICE::pCallbacks` / `D3D11DDIARG_CREATEDEVICE::pCallbacks`
2. Implement `pfnSetErrorCb` usage for all failing `void` DDIs.
3. Create and store kernel submission state via `d3dumddi.h` callbacks:
   - `pfnCreateDeviceCb` + `D3DDDICB_CREATEDEVICE` → `hDevice`
   - `pfnCreateContextCb2`/`pfnCreateContextCb` + `D3DDDICB_CREATECONTEXT` → `hContext`, `hSyncObject`, and initial DMA buffer pointers/capacities
4. Implement “fill → submit” (and optionally “acquire →” if using `pfnGetCommandBufferCb`):
   - optional acquire: `pfnGetCommandBufferCb` + `D3DDDICB_GETCOMMANDINFO`
   - submit render: `pfnRenderCb` + `D3DDDICB_RENDER`
   - submit present: `pfnPresentCb` + `D3DDDICB_PRESENT`
   - update stored DMA buffer pointers if the submit structs are in/out in your header revision.
5. Track per-resource “last GPU write fence” and implement `Map(READ)` wait:
   - `pfnWaitForSynchronizationObjectCb` + `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT` with `ObjectHandleArray[0] = hSyncObject`
