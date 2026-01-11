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

- Submit a host “present” command with:
  - Swapchain ID / backbuffer resource ID
  - `dwFlags`
  - (Optional) a fence ID for completion tracking
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

Define a guest/host handle model that does **not** attempt to expose host OS handles:

- Guest UMD treats `HANDLE` as an opaque **share token**.
- On “export” (resource creation with `pSharedHandle != nullptr`):
  1. Guest derives a stable `share_token` and writes it to `*pSharedHandle`.
  2. Guest informs the host: `(share_token → host_resource_id)` mapping is created.
- On “import” (open from a shared handle token):
  1. Guest passes the `share_token` to the host.
  2. Host returns an existing host-side resource ID or errors if unknown.

**Key invariant:** the token must be stable across processes inside the guest VM. In real Windows this stability is provided via NT handles + `DuplicateHandle`; in Aero, stability is provided by the virtualization driver + host mapping table.

**Implementation note (AeroGPU/WDDM):**
prefer computing `share_token` from the KMD-provided stable allocation ID (`alloc_id`, returned via allocation private driver data on Create/Open),
instead of using raw Win32/D3DKMT handle values (which are process-local).
The recommended scheme is:

- `share_token = (uint64_t)alloc_id`

See `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h` for the concrete private-data structure used to persist `alloc_id`/`share_token` across `CreateAllocation`/`OpenAllocation`.

Timing-wise: **export** the mapping from the creating process (the one that created the shared handle), and **import** from the opening process (the one that opens that handle) before the resource is used.

#### `D3DPOOL_DEFAULT` semantics for Ex

Ex clients expect `D3DPOOL_DEFAULT` resources to behave like true GPU resources:

- “Lost device” should be rare/nonexistent for typical windowed composition workloads.
- `ResetEx` must not force wholesale destruction of default-pool resources as D3D9 often does.

Recommended approach:

- Treat the device as “always operational” unless the host explicitly signals fatal device removal.
- Keep a resource table keyed by resource ID; `ResetEx` updates presentation parameters but does not invalidate existing resources unless format/size constraints require it.

---

## Host-side Protocol / Runtime Requirements

### 1) Command ABI extensions

Add Ex-specific operations to the GPU command ABI (guest → host):

#### `PRESENT_EX`

Payload includes:

- `swapchain_id`
- `dw_flags` (D3D9Ex `PresentEx` flags)
- optional `source_rect`, `dest_rect` (can be ignored initially)
- **fence_id** (for completion tracking)

#### Shared surface export/import

Add commands (see `drivers/aerogpu/protocol/aerogpu_cmd.h`):

- `AEROGPU_CMD_EXPORT_SHARED_SURFACE { resource_handle, share_token }`
- `AEROGPU_CMD_IMPORT_SHARED_SURFACE { out_resource_handle, share_token }`

If the host is the sole renderer (WebGPU), “export” typically means “make this resource reachable by a token”; “import” returns an alias/resource view.

#### Flush/fence operations

Add commands:

- `INSERT_FENCE { fence_id }` (optional if every submit carries a fence)
- `FLUSH {}` (ensure all prior work is scheduled)

### 2) Event ring / IRQ messages (host → guest)

Host signals completion via an event ring buffer:

- `FENCE_SIGNALED { fence_id }`
- `PRESENT_COMPLETED { swapchain_id, present_count, qpc_time }` (or folded into fence completion)

The guest uses these events to:

- unblock `PresentEx` when frame latency is exceeded
- implement `GetPresentStats` / `GetLastPresentCount` accurately
- support query `GetData` style polling

---

## Synchronization Model (Fence Contract)

Define a single fence namespace per device:

- Guest allocates monotonically increasing `fence_id` values (`u64` recommended).
- Every GPU submission may include a `fence_id`; `PresentEx` should always include one.
- Host promises:
  - `FENCE_SIGNALED(fence_id)` is emitted after all GPU work prior to and including that submission is complete (or at least “present-safe”).

Guest-side rules:

- Fence completion is tracked in a bitset/map.
- Blocking calls:
  - `PresentEx` without `DONOTWAIT` may wait until `in_flight < MaxLatency`
  - `GetData` may wait/poll for a fence associated with the query
  - `GetPresentStats` may report stats from the last completed present (or the last submitted present if initial bring-up prefers optimism)

This model is intentionally simple: it is enough for DWM frame pacing without requiring full D3D9 query semantics on day one.

---

## Suggested implementation layout

The concrete file paths will depend on the current codebase layout, but a typical split looks like:

- Guest Windows UMD:
  - `drivers/aerogpu/umd/d3d9/` (D3D9 + D3D9Ex; or split Ex-specific code into a submodule)
- Guest Windows KMD:
  - `drivers/aerogpu/kmd/` (WDDM kernel-mode display driver)
- Guest tests:
  - `drivers/aerogpu/tests/win7/d3d9ex_dwm_probe/` (D3D9Ex + DWM compatibility smoke test)
- Host protocol + command processor:
  - `crates/aero-gpu/src/protocol.rs` (opcode + payload definitions; event types)
  - `crates/aero-gpu/src/command_processor.rs` (implement `PRESENT_EX`, shared surface import/export, fence signaling)

Keeping the protocol definitions in one crate and having both guest and host generate/consume the same structs helps avoid silent ABI drift.

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
