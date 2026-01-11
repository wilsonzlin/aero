# Win7 shared surfaces: ShareToken vs user-mode HANDLE (AeroGPU)

This note documents the **Win7 D3D9Ex shared-surface strategy** used by AeroGPU so future work does not accidentally rely on **process-local handle numeric values**.

## Problem: D3D “shared handles” are process-local

On Windows 7, D3D9/D3D9Ex exposes resource sharing via a user-mode `HANDLE` (e.g. `pSharedHandle` in `CreateTexture`, `CreateRenderTarget`, etc).

That `HANDLE` is an **NT handle**, which means:

- It is only meaningful in the process that owns it (each process has its own handle table).
- When transferred to another process it must be **duplicated** (`DuplicateHandle`) or inherited.
- The numeric `HANDLE` value is **not stable** cross-process; the consumer’s handle value commonly differs from the producer’s.

Therefore: **AeroGPU must not use the numeric D3D shared `HANDLE` value as a protocol share identifier.**

## AeroGPU contract: `share_token` is UMD-owned and preserved by dxgkrnl

In the AeroGPU guest↔host command stream, shared surfaces are keyed by a stable `u64 share_token`:

- `struct aerogpu_cmd_export_shared_surface` (`drivers/aerogpu/protocol/aerogpu_cmd.h`)
- `struct aerogpu_cmd_import_shared_surface` (`drivers/aerogpu/protocol/aerogpu_cmd.h`)

On Windows 7 (WDDM 1.1), `share_token` is stored in WDDM allocation private driver data:

- Header: `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`
- Struct/field: `aerogpu_wddm_alloc_priv.share_token`

The **UMD** chooses a non-zero, collision-resistant token at creation time and writes it into the allocation private-data blob. dxgkrnl preserves that blob for shared allocations and returns it verbatim to other processes when they open the shared resource, making the token stable cross-process.

Note: On Win7/WDDM 1.1, treat allocation private driver data as **UMD → KMD input**.
Do not rely on any KMD→UMD writeback semantics (see the header comment in
`drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
## Expected flow (UMD ↔ KMD ↔ host)

### 1) Create shared resource → export (token)

1. Producer creates a shareable resource (`pSharedHandle != NULL` in the D3D API/DDI).
2. The UMD generates a stable `share_token` and stores it in allocation private driver data (`aerogpu_wddm_alloc_priv.share_token`) for the backing allocation(s).
3. The UMD sends `AEROGPU_CMD_EXPORT_SHARED_SURFACE` with `share_token`.

### 2) Open shared resource → import (token)

1. The OS duplicates/inherits the shared `HANDLE` into the consumer process.
2. Consumer opens the resource; dxgkrnl returns the same allocation private-data blob (including `share_token`) to the UMD.
3. The UMD sends `AEROGPU_CMD_IMPORT_SHARED_SURFACE` with `share_token`.

At no point should the AeroGPU protocol key off the user-mode `HANDLE` numeric value.

## Validation: cross-process Win7 test

Use the cross-process shared-surface test app *(Task 613)*:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_ipc/main.cpp`

It should:

1. Create a shareable D3D9Ex render target in a parent process.
2. Duplicate the D3D shared `HANDLE` into a child process.
3. Child opens the resource and validates content via readback.

This test catches the common bug where `share_token` is (incorrectly) derived from the process-local shared `HANDLE` value: producer and consumer handles differ, so `IMPORT_SHARED_SURFACE` would fail to resolve the previously-exported surface.
