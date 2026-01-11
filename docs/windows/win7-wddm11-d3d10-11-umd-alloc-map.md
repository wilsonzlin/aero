# (Deprecated) Win7 (WDDM 1.1) D3D10/D3D11 UMD allocation + Map/Unmap reference (moved)

This file used to contain a combined “allocation + Map/Unmap” reference for the Win7 D3D10/D3D11 UMD DDIs.

It has been **replaced** by focused, authoritative docs under `docs/graphics/`:

* `docs/graphics/win7-d3d10-11-umd-allocations.md` — Win7/WDDM 1.1 resource allocation contract (`CreateResource` → `pfnAllocateCb` / `pfnDeallocateCb`, `D3DDDI_ALLOCATIONINFO`, segment-set fields, primary/backbuffer flags).
* `docs/graphics/win7-d3d11-map-unmap.md` — Win7 `Map`/`Unmap` semantics (`pfnLockCb`/`pfnUnlockCb`), staging readback synchronization, and DO_NOT_WAIT behavior.
* `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md` — submission callbacks (DMA buffer acquisition, render/present) and fence waits that interact with `Map(READ)`.
* `docs/graphics/win7-dxgi-swapchain-backbuffer.md` — trace guide + invariants for Win7 DXGI swapchain backbuffer `CreateResource` parameters and required allocation flags.
