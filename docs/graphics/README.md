# Graphics docs index

This directory contains “graphics stack” implementation notes and bring-up checklists for the AeroGPU project, mostly focused on Windows 7 (WDDM 1.1) and DirectX translation.

## Current status (single source of truth)

- [`status.md`](./status.md) — implemented vs missing checklist for the Windows 7 user experience

## Windows 7 / AeroGPU driver stack

- `aerogpu-protocols.md` — overview of the different “AeroGPU” ABIs in-tree and which one is the Win7/WDDM target.
- `aerogpu-executor-modes.md` — how the canonical machine (`aero_machine`) drives AeroGPU submissions/fences under different host integration styles (no-op bring-up vs submission bridge vs in-process backend).
- `aerogpu-backing-alloc-id.md` — stable Win7/WDDM 1.1 `backing_alloc_id` semantics for guest-backed resources (not “slot in the current submit’s alloc table”).
- `win7-wddm11-aerogpu-driver.md` — WDDM 1.1 KMD+UMD architecture (adapter bring-up, memory model, submission rings, fences/interrupts, scanout/vblank).
- `win7-aerogpu-validation.md` — Win7 driver stability checklist (TDR avoidance, vblank pacing expectations, debug playbook).
- `win7-vblank-present-requirements.md` — minimal vblank/present timing contract needed to keep DWM (Aero) stable.
- `win7-shared-surfaces-share-token.md` — D3D9Ex/DWM shared surface share_token strategy (stable cross-process token vs user-mode `HANDLE` numeric values, which are not stable cross-process).

## D3D shader translation notes

- `d3d9-sm2-sm3-shader-translation.md` — task-level status/limitations for the D3D9 SM2/SM3 bytecode → IR → WGSL pipeline (kept as a “don’t duplicate work” scratchpad).
- `geometry-shader-emulation.md` — D3D10/11 geometry shader (GS) emulation via compute expansion.
- `tessellation-emulation.md` — D3D11 tessellation (HS/DS) emulation via compute expansion.

## User-mode driver (UMD) API surfaces

- `win7-d3d9ex-umd-minimal.md` — minimal D3D9Ex UMD/DDI surface for DWM + basic D3D9 apps.
- `win7-d3d9-umd-tracing.md` — lightweight D3D9 UMD DDI call tracing (which entrypoints DWM/apps invoke).
- `win7-d3d9-fixedfunc-wvp.md` — fixed-function WVP implementation notes (`D3DFVF_XYZ*` draw-time WVP paths + minimal `NORMAL` lighting subset + `ProcessVertices` CPU path).
- `win7-d3d10-11-umd-minimal.md` — minimal D3D10 + D3D11 UMD/DDI surface (SM4/SM5) plus DXGI swapchain expectations (targeting FL10_0 bring-up, roadmap to FL11_0).
- `win7-d3d10-11-umd-allocations.md` — CreateResource-side Win7/WDDM 1.1 allocation contract (`pfnAllocateCb`/`pfnDeallocateCb`, `D3DDDI_ALLOCATIONINFO`, `DXGI_DDI_PRIMARY_DESC` primary/backbuffer identification).
- `win7-d3d11-map-unmap.md` — Win7 D3D11 Map/Unmap + runtime `LockCb`/`UnlockCb` semantics (dynamic uploads + staging readback).
- `win7-d3d10-11-umd-callbacks-and-fences.md` — Win7 WDK header reference for the **exact** D3D10/11 UMD callback structs used for DMA buffer allocation, submission (render/present), error reporting from `void` DDIs, fence waits for `Map(READ)`, and WOW64 gotchas.
- `win7-d3d11ddi-function-tables.md` — D3D11 `d3d11umddi.h` function-table checklist for Win7 (which table entries must be non-null vs safely stubbed for FL10_0 bring-up).
- `win7-d3d10-caps-tracing.md` — enabling `OutputDebugString` tracing for D3D10DDI `GetCaps` + unexpected runtime entrypoints during Win7 bring-up.
- `win7-dxgi-swapchain-backbuffer.md` — trace guide + invariants for Win7 DXGI swapchain backbuffer `CreateResource` parameters and allocation flags.

Combined reference:

- `../windows/win7-wddm11-d3d10-11-umd-alloc-map.md` — deprecated redirect (kept for link compatibility; points at the focused docs above).

## Strategy / prototypes

- `guest-gpu-driver-strategy.md` — options for Windows guest GPU drivers (virtio reuse vs custom WDDM).
- `virtio-gpu-proto-proof.md` — early prototype notes for virtio-style GPU paths.

## Internal agent coordination / task audits

- [`task-489-sm3-dxbc-sharedsurface-audit.md`](./task-489-sm3-dxbc-sharedsurface-audit.md) — scratchpad task audit (SM3/DXBC/shared-surface); includes code+test pointers to avoid duplicate work.
