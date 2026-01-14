# Win7 shared surfaces: ShareToken vs user-mode HANDLE (AeroGPU)

This note documents the **Win7 shared-surface strategy** used by AeroGPU (D3D9Ex *and* DXGI/D3D10/D3D11) so future work does not accidentally rely on **user-mode `HANDLE` numeric values** (which are not stable cross-process).

## Problem: D3D “shared handles” are not stable cross-process

On Windows 7, D3D9/D3D9Ex exposes resource sharing via a user-mode `HANDLE` (e.g. `pSharedHandle` in `CreateTexture`, `CreateRenderTarget`, etc).

DXGI/D3D10/D3D11 exposes the same concept via `IDXGIResource::GetSharedHandle()` and the various `OpenSharedResource(...)` APIs.

In the Win7 desktop composition scenario, **DWM (D3D9Ex) consumes DXGI shared handles**
produced by D3D10/D3D11 apps; AeroGPU therefore treats “shared surfaces” as a cross-API mechanism
(not just D3D9 ↔ D3D9 or D3D11 ↔ D3D11).

That `HANDLE` is typically an **NT handle** (notably for DXGI shared handles), which means:

- It is only meaningful in the process that owns it (each process has its own handle table).
- When transferred to another process it must be **duplicated** (`DuplicateHandle`) or inherited (for real NT handles).
- The numeric `HANDLE` value is **not stable** cross-process; the consumer’s handle value commonly differs from the producer’s.

Therefore: **AeroGPU must not use the numeric D3D shared `HANDLE` value as a protocol share identifier.**

Note: some D3D9Ex implementations use “token-style” shared handles that are not real NT handles and cannot be duplicated
with `DuplicateHandle`. Even in that case, the numeric value is not a robust protocol key: the stable cross-process
identifier is still `share_token` preserved in WDDM allocation private data.

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

### Related metadata: preserving row pitch for cross-API consumers (DWM)

In addition to `share_token`, AeroGPU also persists enough allocation metadata for safe cross-process **and cross-API**
interop:

- `alloc_id` (stable allocation ID used by the per-submit allocation table)
- `size_bytes`
- and (for surface allocations) an optional **row pitch** encoding

The pitch metadata matters because some Win7 `OpenResource` DDI variants (notably D3D9Ex consumers such as `dwm.exe`)
do not reliably provide a full resource description (including pitch). AeroGPU therefore propagates row pitch through the
preserved private-data blob:

- for `aerogpu_wddm_alloc_priv_v2`, via `row_pitch_bytes`
- and for legacy consumers, via the low 32 bits of `reserved0` when the D3D9 surface descriptor marker bit is not set

Additionally, when a **D3D9Ex producer** shares a surface with a **DXGI/D3D10/D3D11 consumer**, the opening D3D10/11 UMD may need
to recover the resource description from the legacy v1 private-data blob. AeroGPU encodes a minimal D3D9 surface descriptor
into `reserved0` (marker bit set; `format/width/height`), and the D3D10/11 UMD maps that descriptor back to a suitable
`DXGI_FORMAT` when opening such resources.

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

1. If the shared handle is a real NT handle, the OS duplicates/inherits the shared `HANDLE` into the consumer process.
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
- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface/main.cpp`
- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_stress/main.cpp` (hardening; repeated create → open → destroy)
- `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/main.cpp`
- `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc/main.cpp`
- `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc/main.cpp`

It should:

1. Create a shareable render target/texture in a parent process.
2. If the shared handle is a real NT handle, duplicate it into a child process (`DuplicateHandle`); otherwise pass the raw numeric value (token-style handles).
3. Child opens the resource and validates content via readback.

This test catches the common bug where `share_token` is (incorrectly) derived from the user-mode shared `HANDLE` numeric value: for real NT handles, producer and consumer numeric values commonly differ (for example after `DuplicateHandle`), so `IMPORT_SHARED_SURFACE` would fail to resolve the previously-exported surface.

### Validation: cross-bitness shared surfaces (Win7 x64 / WOW64)

On Win7 x64, DWM is 64-bit but many applications are 32-bit (WOW64). Validate this scenario with:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_wow64/` (x86 producer spawns an x64 consumer)

Optional debug-only validation (when supported by the KMD):

- Use `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` (via `D3DKMTEscape`) to map a process-local shared `HANDLE` to a stable 32-bit **debug token**.
- The producer and consumer should observe the same debug token even when their numeric `HANDLE` values differ.
- This debug token is **not** the protocol `u64 share_token` used by `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`; it exists only to help bring-up tooling prove that handle duplication/inheritance is working correctly.
- Stress test: `drivers/aerogpu/tests/win7/map_shared_handle_stress/main.cpp` exercises this escape in a tight loop and under many unique section handles (skips when unsupported or gated off).

## Validation: shared resources must be single-allocation (MVP policy)

Shared resources should be restricted to a single WDDM allocation (`NumAllocations == 1`) so `EXPORT_SHARED_SURFACE` /
`IMPORT_SHARED_SURFACE` can safely associate one `share_token` with one underlying allocation. This implies rejecting shared
textures that request a full mip chain (`Levels=0`) and other descriptors that would imply multiple allocations.

Use:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_allocations/main.cpp`

## Validation: alloc_id uniqueness under DWM-like batching

Use the multi-producer/persistence D3D9Ex test apps:

- `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_many_producers/main.cpp`
- `drivers/aerogpu/tests/win7/d3d9ex_alloc_id_persistence/main.cpp`

`d3d9ex_shared_surface_many_producers` spawns multiple producer processes and opens their shared surfaces in a compositor
process, then references all of them in a single `Flush` (one submission). This stresses the `alloc_id` uniqueness
requirement across processes for batched submissions (DWM composition case).

`d3d9ex_alloc_id_persistence` is a longer-running two-process ping-pong test that repeatedly references allocations created
in different processes in a single submission (`StretchRect` uses both src+dst allocations).
