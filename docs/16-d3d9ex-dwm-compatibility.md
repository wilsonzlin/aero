# 16 - Direct3D 9Ex (D3D9Ex) & Windows 7 DWM Compatibility

## Overview

Windows 7’s Desktop Window Manager (DWM, `dwm.exe`) and many “Aero era” applications use **Direct3D 9Ex** rather than legacy D3D9. A D3D9-only user-mode driver (UMD) is typically insufficient to keep composition enabled: DWM expects the D3D9Ex API surface and relies on a few key Ex-specific semantics (present pacing, present statistics, and shared resources).

This document defines the minimum viable **end-to-end contract** needed to support D3D9Ex clients in Aero:

1. **Guest-side (Windows UMD):** `IDirect3D9Ex`, `IDirect3DDevice9Ex`, Ex present/reset/display-mode APIs, and shared resource behavior.
2. **Host-side protocol/runtime:** command ABI extensions for Ex present, present stats, and shared surface import/export.
3. **Synchronization model:** a simple, explicit fence mechanism that Ex clients can observe via present stats and `GetData` polling.
4. **Tests:** a Windows guest D3D9Ex test app + a host integration test flow.

---

## Why D3D9Ex Matters for Windows 7

D3D9Ex is not just “D3D9 with extra methods”. In practice, it is the path DWM takes for:

- More resilient device behavior (reduced “device lost” incidence)
- Improved frame pacing controls (max frame latency)
- Present statistics used for diagnostics and scheduling
- Shared resources used to pass surfaces between processes/components

If `Direct3DCreate9Ex` or `CreateDeviceEx` are missing, DWM usually falls back to Basic theme or fails to start composition.

---

## Guest-side (UMD) Requirements

### 1) COM interface surface

Implement the following interfaces in the guest UMD:

- `IDirect3D9Ex` (extends `IDirect3D9`)
- `IDirect3DDevice9Ex` (extends `IDirect3DDevice9`)
- Ex swap chain/present APIs (device-level in D3D9Ex)

**Implementation strategy (recommended):**

- Use a single concrete object to back both base and Ex interfaces.
- `QueryInterface` returns the same object with the appropriate vtable pointer.
- `AddRef`/`Release` shared refcount.

This minimizes duplicated state while satisfying interface identity rules that DWM and the D3D runtime assume.

### 2) Required entry points / methods

#### `Direct3DCreate9Ex`

Expose the D3D9Ex creation entry point:

```c++
HRESULT WINAPI Direct3DCreate9Ex(UINT sdkVersion, IDirect3D9Ex** out);
```

Return `D3D_OK` and a valid `IDirect3D9Ex` object.

#### `IDirect3D9Ex::CreateDeviceEx`

Implement `CreateDeviceEx`:

```c++
HRESULT CreateDeviceEx(
  UINT Adapter,
  D3DDEVTYPE DeviceType,
  HWND hFocusWindow,
  DWORD BehaviorFlags,
  D3DPRESENT_PARAMETERS* pPresentationParameters,
  D3DDISPLAYMODEEX* pFullscreenDisplayMode,
  IDirect3DDevice9Ex** ppReturnedDeviceInterface);
```

Notes:

- Treat windowed mode as the primary supported mode for DWM.
- `pFullscreenDisplayMode` may be `nullptr`; do not fail.
- Return an `IDirect3DDevice9Ex` that also answers `IDirect3DDevice9`.

#### `IDirect3DDevice9Ex::PresentEx`

Implement `PresentEx` (device-level present):

```c++
HRESULT PresentEx(
  const RECT* pSourceRect,
  const RECT* pDestRect,
  HWND hDestWindowOverride,
  const RGNDATA* pDirtyRegion,
  DWORD dwFlags);
```

Minimum viable behavior:

- Emit an `AEROGPU_CMD_PRESENT_EX` packet (see `drivers/aerogpu/protocol/aerogpu_cmd.h`) with:
  - `scanout_id` (0 for MVP),
  - `flags` (`AEROGPU_PRESENT_FLAG_VSYNC` if vsync paced), and
  - `d3d9_present_flags = dwFlags`.
  - Completion tracking is done via the submission fence (`aerogpu_submit_desc.signal_fence` in `drivers/aerogpu/protocol/aerogpu_ring.h`), not via a per-command fence payload.
  - **Fence ID source of truth (Win7/WDDM):** the UMD must use the exact `SubmissionFenceId` returned by the D3D9 runtime submission callbacks (`D3DDDICB_RENDER` / `D3DDDICB_PRESENT`) as the fence value for *that specific submission*.
    - Do **not** infer a per-submission “last submitted fence” via a global KMD escape query: under multi-process workloads (DWM + apps) that value can be dominated by another process’s submissions and will break EVENT query completion and PresentEx throttling.
    - Escape-based fence queries are still useful for polling **last completed** fence values, but should not be used to associate a fence with an individual submission except as a debug-only fallback.
- Return:
  - `S_OK` if accepted for execution
  - `D3DERR_WASSTILLDRAWING` if `D3DPRESENT_DONOTWAIT` is set and the queue is “full” (see **frame latency** below)
  - Optionally `S_PRESENT_OCCLUDED` if the window is minimized/occluded (can be approximated; returning `S_OK` is acceptable for initial bring-up if it keeps DWM stable)

**Important:** DWM often does *not* tolerate `D3DERR_DEVICELOST` style failures during composition. Prefer stable “always works” behavior over strict emulation of every failure mode.

#### Additional `IDirect3DDevice9Ex` methods that should not be stubs that fail

In practice, DWM and other Ex clients frequently touch a wider surface area than the “headline” methods. For initial compatibility, it is usually better to return **best-effort success** rather than `E_NOTIMPL`.

| Method | Minimal acceptable behavior |
|--------|----------------------------|
| `CheckDeviceState(HWND)` | Return `S_OK` for normal operation. If presentation is impossible (e.g. host surface unavailable), return `D3DERR_DEVICELOST` as a last resort. |
| `WaitForVBlank(UINT)` | Block until the next host “vsync tick” if available; otherwise sleep/yield and return `S_OK`. |
| `SetGPUThreadPriority(INT)` / `GetGPUThreadPriority(INT*)` | Store/return a clamped priority (e.g. `[-7, 7]`), no scheduling impact required initially. |
| `CheckResourceResidency(IDirect3DResource9** resources, UINT count)` | Return `S_OK` and report resources resident (the host-backed model usually makes “evicted” meaningless). |
| `ComposeRects(...)` | Can return `D3D_OK` if unused; if used, implement a simple CPU fallback or translate to a host blit operation. |
| `CreateRenderTargetEx` / `CreateOffscreenPlainSurfaceEx` / `CreateDepthStencilSurfaceEx` | Forward to the non-Ex creation path, honoring `pSharedHandle` and accepting additional flags/usage parameters. |

#### `ResetEx`, `GetDisplayModeEx`

Implement:

- `HRESULT ResetEx(D3DPRESENT_PARAMETERS* pPP, D3DDISPLAYMODEEX* pFSDM)`
- `HRESULT GetDisplayModeEx(UINT iSwapChain, D3DDISPLAYMODEEX* pMode, D3DDISPLAYROTATION* pRotation)`

For Windows 7 desktop composition:

- `ResetEx` should typically succeed and keep resources valid (see **D3DPOOL_DEFAULT semantics**).
- `GetDisplayModeEx` can return a single primary monitor mode and `D3DDISPLAYROTATION_IDENTITY`.

#### Present statistics

Implement:

- `HRESULT GetPresentStats(D3DPRESENTSTATS* pStats)`
- `HRESULT GetLastPresentCount(UINT* pLastPresentCount)`

If full-fidelity timing is not possible initially:

- Maintain a monotonic `present_count` incremented for each *accepted* `PresentEx`.
- Populate `D3DPRESENTSTATS` with “sane” monotonic values:
  - `PresentCount`: `present_count`
  - `PresentRefreshCount`: `present_count` (or a derived “vsync tick” if available)
  - `SyncRefreshCount`: same as above
  - `SyncQPCTime`/`PresentQPCTime`: best-effort (can be 0 if unavailable, but prefer using `QueryPerformanceCounter` in user mode)

DWM primarily cares that these calls **succeed** and that counts are **monotonic**.

#### Frame latency control

D3D9Ex adds:

- `SetMaximumFrameLatency(UINT MaxLatency)`
- `GetMaximumFrameLatency(UINT* pMaxLatency)`

Minimal viable implementation:

- Default `MaxLatency = 3` (Windows commonly uses 3).
- Count “in-flight” presents (submitted but fence not signaled yet).
- If `in_flight >= MaxLatency`:
  - If `D3DPRESENT_DONOTWAIT` is set: return `D3DERR_WASSTILLDRAWING`
  - Otherwise: block/poll until at least one present fence completes

This is the core frame-pacing behavior DWM relies on.

### 3) Resource behaviors Ex clients rely on

#### Shared resources / shared handles
 
DWM composition commonly depends on the ability to share render targets/textures across components. D3D9 already has `pSharedHandle` parameters, but D3D9Ex tends to rely on this behavior more heavily and in more cases.
 
Define a guest/host sharing model that does **not** attempt to expose host OS handles:

- The D3D9/D3D9Ex API surface uses a user-mode `HANDLE` (`pSharedHandle`) to represent “shared resources”.
  - This value is a normal Windows handle: **process-local**, not stable cross-process, and commonly different in the consumer after `DuplicateHandle`.
  - **AeroGPU does _not_ use the numeric `HANDLE` value as the protocol `share_token`.**
- In the AeroGPU protocol, `share_token` is defined as the **KMD-generated allocation `ShareToken`** returned to the UMD via allocation private driver data (see `drivers/aerogpu/protocol/aerogpu_alloc_privdata.h`).
  - **Do not** treat the raw Win32 `HANDLE` value itself as a stable cross-process token. The handle is still required for correctness (it is how another process asks Windows to open the shared resource), but it is not a good host-mapping key.

Expected sequence:

1. **Create shared resource → export (token)**
   - Producer process creates a shareable resource (`pSharedHandle != nullptr`).
   - The KMD generates/stores a `ShareToken` for the underlying allocation and returns it to the UMD (allocation private driver data).
   - The UMD submits `EXPORT_SHARED_SURFACE { resource_handle, share_token=ShareToken }` so the host can map `share_token → resource`.

2. **Open shared resource → import (token)**
   - Consumer process opens the resource via the OS shared handle mechanism (the handle must already be valid in the consumer process via `DuplicateHandle`/inheritance).
   - The KMD resolves the shared allocation and returns the same `ShareToken` (allocation private driver data).
   - The UMD submits `IMPORT_SHARED_SURFACE { share_token=ShareToken } -> resource_handle` to obtain a host resource alias.

**Key invariant:** `share_token` must be stable across processes inside the guest VM. The KMD `ShareToken` is stable; user-mode `HANDLE` numeric values are not.

See `docs/graphics/win7-shared-surfaces-share-token.md` for implementation details and the cross-process validation test.

Timing-wise: **export** the mapping from the creating process (the one that created the shared handle), and **import** from the opening process (the one that opens that handle) before the resource is used.

**Guest-side validation:** run `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_ipc` to exercise this exact cross-process “create shared → open shared” path.

##### MVP limitation: shared surfaces must be single-allocation

Many WDDM resources *can* be represented as multiple allocations (for example: per-mip allocations, texture arrays, or multi-plane formats). AeroGPU’s MVP shared-surface protocol (`EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`) currently associates a share token with a **single** backing resource/allocation.

To avoid creating share tokens that cannot be imported safely, the driver stack enforces an MVP restriction:

- **Only shared resources that map to exactly one WDDM allocation are supported.**
- The KMD validates `NumAllocations == 1` for shared allocation create/open and fails deterministically otherwise.
- The UMD should reject shared creations that would require multiple allocations (practically: keep shared surfaces to `mip_levels=1` (reject `MipLevels/Levels=0`, which requests a full mip chain) and `array_layers=1`, which matches typical DWM redirected surfaces).

##### Shared-surface lifetime / destruction (host + Win7 driver contract)

For correctness **and** to avoid leaking host-side GPU objects, the `share_token → resource` mapping must be removed once the **last** guest reference to that shared surface is closed.

**Host-side (✅ implemented; Task 639 closed):**

- `AEROGPU_CMD_EXPORT_SHARED_SURFACE` creates/updates the `share_token → resource` mapping (idempotent when re-exporting the same token/underlying surface).
- `AEROGPU_CMD_IMPORT_SHARED_SURFACE` increments the underlying surface refcount and returns a new alias handle referencing the same underlying resource.
- `AEROGPU_CMD_DESTROY_RESOURCE` decrements the refcount for any handle (original or alias) referencing a shared surface; when it hits 0, the host destroys the underlying resource and drops all `share_token` mappings to it.
- The host rejects `EXPORT_SHARED_SURFACE` collisions (same token mapped to a different underlying resource) and validates that alias handles resolve correctly.

**Win7 guest driver semantics (current):**

The Win7 D3D9 UMD now emits `AEROGPU_CMD_DESTROY_RESOURCE` for shared resources on per-process close (including alias handles). Because the host maintains a refcount across original + imported handles, this is safe and allows the host to drop `share_token` mappings when the last handle is destroyed.

**Hardening gap (optional):**

This still relies on user-mode teardown running. If we need host cleanup to track the WDDM kernel object lifetime even across abnormal termination (or future non-UMD sharing paths), add a KMD-driven global refcount keyed by `share_token` plus a host-visible “last close” signal.

##### Task status (shared-surface lifetime)

| Task | Status | Notes |
| ---- | ------ | ----- |
| 639 | ✅ Verified | Host-side shared-surface lifetime: `DESTROY_RESOURCE` + refcounting (original + aliases) + collision validation + multi-submission coverage (see `crates/aero-gpu/src/protocol.rs`, `crates/aero-gpu/src/command_processor.rs`, and `crates/aero-gpu/tests/aerogpu_d3d9_shared_surface.rs`). |
| 639-FU | 🔵 Blocked | (Hardening) Win7 KMD/UMD: define + implement a **cross-process shared-surface destruction contract** so host cleanup tracks WDDM kernel object lifetime even across abnormal termination (KMD-driven global refcount; host-visible “last close” signal keyed by `share_token`). Depends on Task 578 and Task 594. |

#### `D3DPOOL_DEFAULT` semantics for Ex

Ex clients expect `D3DPOOL_DEFAULT` resources to behave like true GPU resources:

- “Lost device” should be rare/nonexistent for typical windowed composition workloads.
- `ResetEx` must not force wholesale destruction of default-pool resources as D3D9 often does.

Recommended approach:

- Treat the device as “always operational” unless the host explicitly signals fatal device removal.
- Keep a resource table keyed by protocol `resource_handle` (or equivalent internal ID); `ResetEx` updates presentation parameters but does not invalidate existing resources unless format/size constraints require it.
- Ensure protocol handles (`resource_handle`, `shader_handle`, `input_layout_handle`, etc) are **globally unique across guest processes**. The MVP ring ABI currently sets `aerogpu_submit_desc.context_id = 0`, so the host interprets handles in a single global namespace.

---

## Host-side Protocol / Runtime Requirements

### 1) Command ABI extensions

Add Ex-specific operations to the GPU command ABI (guest → host):

#### `PRESENT_EX`

Payload includes:

- `scanout_id`
- `flags` (`AEROGPU_PRESENT_FLAG_*`)
- `d3d9_present_flags` (raw D3D9Ex `PresentEx` `dwFlags`)

Fence completion is tracked via the submission descriptor (`aerogpu_submit_desc.signal_fence`), not in the `PRESENT_EX` packet itself.

#### Shared surface export/import

Add commands (see `drivers/aerogpu/protocol/aerogpu_cmd.h`):

- `AEROGPU_CMD_EXPORT_SHARED_SURFACE { resource_handle, share_token }`
- `AEROGPU_CMD_IMPORT_SHARED_SURFACE { out_resource_handle, share_token }`

If the host is the sole renderer (WebGPU), “export” typically means “make this resource reachable by a token”; “import” returns an alias/resource view.

#### Flush/fence operations

The versioned AeroGPU ABI does not require a separate “insert fence” command:
each ring submission carries a `signal_fence` value.

The command stream does define an explicit flush point:

- `AEROGPU_CMD_FLUSH {}` (ensure all prior work is scheduled)

### 2) Fence/completion signaling (host → guest)

Fence completion is signaled via the ring/MMIO contract (`drivers/aerogpu/protocol/aerogpu_ring.h` and `aerogpu_pci.h`):

The guest uses fence completion to:

- unblock `PresentEx` when frame latency is exceeded
- implement `GetPresentStats` / `GetLastPresentCount` accurately
- support query `GetData` style polling

---

## Synchronization Model (Fence Contract)

Define a single fence namespace per device:

- Guest allocates monotonically increasing `fence_id` values (`u64` recommended).
- Every GPU submission may include a `fence_id`; `PresentEx` should always include one.
- Host promises:
  - the device-visible completed fence value monotonically advances to at least `fence_id` once all GPU work prior to and including that submission is complete (or at least “present-safe”).

Guest-side rules:

- Fence completion is tracked in a bitset/map.
- Blocking calls:
  - `PresentEx` without `DONOTWAIT` may wait until `in_flight < MaxLatency`
  - `GetData` should be **non-blocking**: return `S_FALSE` until the query fence is complete. If `D3DGETDATA_FLUSH`
    is specified, the UMD may attempt a best-effort flush/submission but must not wait (DWM can call `GetData` while
    holding global compositor locks).
  - `GetPresentStats` may report stats from the last completed present (or the last submitted present if initial bring-up prefers optimism)

This model is intentionally simple: it is enough for DWM frame pacing without requiring full D3D9 query semantics on day one.

---

## Suggested implementation layout

The supported, in-tree AeroGPU Windows 7 driver stack lives under `drivers/aerogpu/`:

- Guest Windows UMD (D3D9 + D3D9Ex): `drivers/aerogpu/umd/d3d9/` (or split Ex-specific code into a submodule)
- Guest Windows KMD (WDDM miniport): `drivers/aerogpu/kmd/` (WDDM kernel-mode display driver)
- Guest tests / probes (D3D9Ex + DWM): `drivers/aerogpu/tests/win7/` (see `d3d9ex_dwm_probe/` for a smoke test, `d3d9ex_event_query` for fence/query behavior including `--process-stress`, and `d3d9ex_query_latency` for max-frame-latency pacing)
- Guest↔host protocol headers (canonical ABI): `drivers/aerogpu/protocol/`
- Host protocol + command processor:
  - `crates/aero-gpu/src/protocol*.rs` (opcode + payload definitions; event types)
  - `crates/aero-gpu/src/command_processor*.rs` (implement `PRESENT_EX`, shared surface import/export, fence signaling)

Note: an older prototype Win7 driver stack existed during early bring-up (legacy PCI IDs / different ABI)
and was not WOW64-complete on Win7 x64. It is archived under `prototype/legacy-win7-aerogpu-1ae0/` to
avoid accidental installs.

---

## Tests

### Guest Windows test app: `d3d9ex_test`

Provide a Windows test program that:

1. Calls `Direct3DCreate9Ex`
2. Creates a device via `CreateDeviceEx`
3. Renders a moving pattern to a render target/backbuffer
4. Calls `PresentEx` in a loop
5. Calls `GetPresentStats` and `GetLastPresentCount` each frame and validates:
   - calls return `S_OK`
   - counts are monotonic

Example (sketch):

```c++
ComPtr<IDirect3D9Ex> d3d;
CHECK_HR(Direct3DCreate9Ex(D3D_SDK_VERSION, &d3d));

D3DPRESENT_PARAMETERS pp = {};
pp.Windowed = TRUE;
pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
pp.hDeviceWindow = hwnd;
pp.PresentationInterval = D3DPRESENT_INTERVAL_ONE;

ComPtr<IDirect3DDevice9Ex> dev;
CHECK_HR(d3d->CreateDeviceEx(
  D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd,
  D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, nullptr, &dev));

UINT last = 0;
for (;;) {
  render(dev.Get(), t++);
  CHECK_HR(dev->PresentEx(nullptr, nullptr, nullptr, nullptr, 0));

  D3DPRESENTSTATS st = {};
  CHECK_HR(dev->GetPresentStats(&st));
  CHECK_HR(dev->GetLastPresentCount(&last));
  assert(st.PresentCount >= last);
}
```

### Verification steps (Windows 7)

In a Windows 7 SP1 guest:

1. Verify `d3d9ex_test` runs without crashing, and it visibly animates.
2. Confirm `dwm.exe` starts and Aero composition remains enabled:
   - no immediate fallback to Basic theme
   - no repeated `Direct3DCreate9Ex` failures in logs
3. Optional: launch other D3D9Ex apps (media players, simple demos) to validate broader compatibility.

### Host integration test

At minimum, validate that:

- The host command processor accepts `PRESENT_EX` commands.
- Fence completion events are generated.
- Present stats requests do not fail (even if values are approximate).

---

## Known limitations / acceptable initial gaps

To keep the scope bounded for initial DWM bring-up, it is acceptable to defer:

- Full correctness of `D3DPRESENTSTATS` timing fields (counts must still be monotonic)
- Full cross-session “real NT handle” semantics (virtual share tokens are sufficient)
- Exclusive fullscreen / display mode switching
- Multi-adapter support

The key goal is: **DWM stays alive and keeps composition enabled**.
