# Win7 shared surfaces: ShareToken vs user-mode HANDLE (AeroGPU)

This note documents the **Win7 shared-surface strategy** used by AeroGPU (D3D9Ex *and* DXGI/D3D10/D3D11) so future work does not accidentally rely on **process-local handle numeric values**.

## Problem: D3D “shared handles” are process-local

On Windows 7, D3D9/D3D9Ex exposes resource sharing via a user-mode `HANDLE` (e.g. `pSharedHandle` in `CreateTexture`, `CreateRenderTarget`, etc).

DXGI/D3D10/D3D11 exposes the same concept via `IDXGIResource::GetSharedHandle()` and the various `OpenSharedResource(...)` APIs.

In the Win7 desktop composition scenario, **DWM (D3D9Ex) consumes DXGI shared handles**
produced by D3D10/D3D11 apps; AeroGPU therefore treats “shared surfaces” as a cross-API mechanism
(not just D3D9 ↔ D3D9 or D3D11 ↔ D3D11).

That `HANDLE` is an **NT handle**, which means:

- It is only meaningful in the process that owns it (each process has its own handle table).
- When transferred to another process it must be **duplicated** (`DuplicateHandle`) or inherited.
- The numeric `HANDLE` value is **not stable** cross-process; the consumer’s handle value commonly differs from the producer’s.

Therefore: **AeroGPU must not use the numeric D3D shared `HANDLE` value as a protocol share identifier.**

## AeroGPU contract: `share_token` is a stable token persisted in WDDM allocation private data

In the AeroGPU guest↔host command stream, shared surfaces are keyed by a stable `u64 share_token`:

- `struct aerogpu_cmd_export_shared_surface` (`drivers/aerogpu/protocol/aerogpu_cmd.h`)
- `struct aerogpu_cmd_import_shared_surface` (`drivers/aerogpu/protocol/aerogpu_cmd.h`)

On Win7/WDDM 1.1, the Win7 KMD generates a stable non-zero `share_token` for each shared
allocation and persists it in the preserved WDDM allocation private driver data blob
(`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
For shared allocations, dxgkrnl preserves these bytes and returns them verbatim when
another process opens the shared resource, so the opening UMD instance observes the
same `share_token`.

### Collision policy

`share_token` must be treated as a **globally unique** identifier:

- `share_token == 0` is reserved/invalid.
- If the host observes an `EXPORT_SHARED_SURFACE` attempting to re-bind an
  already-exported token to a *different* resource, it must fail deterministically
  (never silently retarget the token).
- Once a token is released (`RELEASE_SHARED_SURFACE`) it must be treated as **retired**
  and must never be re-exported for a new resource (misbehaving guests must get a
  deterministic error rather than silently re-arming a stale token).
- If the host observes an `IMPORT_SHARED_SURFACE` for an unknown/released token,
  it must fail deterministically.
- Host failures must **not** block fence completion: the submission must still
  complete and advance the fence with an error indication (so the guest cannot
  deadlock waiting on a fence that will never signal).

## Expected flow (UMD ↔ KMD ↔ host)

### 1) Create shared resource → export (token)

1. Producer creates a shareable resource (`pSharedHandle != NULL` in the D3D API/DDI).
2. The UMD provides a WDDM allocation private-data buffer (`aerogpu_wddm_alloc_priv`) describing the allocation and marking it as shared (`flags |= AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED`). The UMD sets `share_token = 0` (placeholder).
3. The KMD creates the underlying allocation, generates a stable 64-bit `share_token`, and writes it into `aerogpu_wddm_alloc_priv.share_token`.
4. The UMD sends `AEROGPU_CMD_EXPORT_SHARED_SURFACE` with `share_token`.

### 2) Open shared resource → import (token)

1. The OS duplicates/inherits the shared `HANDLE` into the consumer process.
2. Consumer opens the resource; dxgkrnl returns the preserved allocation private driver data so the UMD can recover the same `share_token`.
3. The UMD sends `AEROGPU_CMD_IMPORT_SHARED_SURFACE` with `share_token`.

At no point should the AeroGPU protocol key off the user-mode `HANDLE` numeric value.

## Lifetime: invalidating `share_token` on final close

For correctness and to avoid leaking host-side shared-surface mappings, the host must eventually drop the `share_token → resource` entry used by `IMPORT_SHARED_SURFACE`.

On Win7/WDDM 1.1, the KMD tracks the allocation wrapper lifetime across processes and emits:

- `AEROGPU_CMD_RELEASE_SHARED_SURFACE { share_token }`

when the **final** wrapper for a shared surface is released (tolerant of Win7’s varying `CloseAllocation`/`DestroyAllocation` call patterns). After this, new imports by that token must fail; existing alias handles remain valid until they are destroyed.

## Validation: cross-process Win7 test

Use one of the cross-process shared-surface IPC tests:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_ipc/main.cpp`
- `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/main.cpp`
- `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc/main.cpp`
- `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc/main.cpp`

It should:

1. Create a shareable render target/texture in a parent process.
2. Duplicate the D3D shared `HANDLE` into a child process.
3. Child opens the resource and validates content via readback.

This test catches the common bug where `share_token` is (incorrectly) derived from the process-local shared `HANDLE` value: producer and consumer handles differ, so `IMPORT_SHARED_SURFACE` would fail to resolve the previously-exported surface.

Optional debug-only validation (when supported by the KMD):

- Use `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` (via `D3DKMTEscape`) to map a process-local shared `HANDLE` to a stable 32-bit **debug token**.
- The producer and consumer should observe the same debug token even when their numeric `HANDLE` values differ.
- This debug token is **not** the protocol `u64 share_token` used by `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`; it exists only to help bring-up tooling prove that handle duplication/inheritance is working correctly.

## Validation: alloc_id uniqueness under DWM-like batching

Use the multi-producer D3D9Ex test app:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_many_producers/main.cpp`

It spawns multiple producer processes and opens their shared surfaces in a
compositor process, then references all of them in a single `Flush` (one
submission). This stresses the `alloc_id` uniqueness requirement across processes
for batched submissions (DWM composition case).
