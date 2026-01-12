# AeroGPU Windows 7 D3D10/11 User-Mode Driver (UMD)

This directory contains the **Direct3D 10 / Direct3D 11 Windows 7 SP1 user-mode driver** for AeroGPU.

The UMD is responsible for translating the D3D10DDI/D3D11DDI calls made by the D3D runtime into the **AeroGPU command stream** defined in `drivers/aerogpu/protocol/`.

This UMD targets only the **command stream** ABI (`drivers/aerogpu/protocol/aerogpu_cmd.h`). The kernel-mode driver (KMD)
owns the submission transport and supports both the versioned (`aerogpu_pci.h` + `aerogpu_ring.h`) and legacy
(`legacy/aerogpu_protocol_legacy.h`) device ABIs, auto-detected via MMIO magic; see `drivers/aerogpu/kmd/README.md`.
(The in-tree Win7 driver package binds only to the versioned device by default; legacy uses `drivers/aerogpu/packaging/win7/legacy/`.)

## Status / scope (initial)

This implementation is intentionally conservative and targets **the minimum functionality needed for a basic D3D11 triangle**:

- Device + immediate context
- Buffers and 2D textures
- Vertex/pixel shaders (DXBC payload passthrough)
- Geometry shaders are currently **accepted but ignored** (no GS stage in the AeroGPU/WebGPU pipeline yet). This is sufficient for the Win7 smoke test's pass-through GS that only renames varyings.
- Input layout + vertex/index buffers
- RTV + clear + draw/draw-indexed
- Blend/raster/depth state objects (accepted; currently conservative/stubbed)
- Windowed swapchain present (sync interval 0 vs non-zero) via `AEROGPU_CMD_PRESENT`

Unsupported functionality must fail cleanly (returning `E_NOTIMPL` / `E_INVALIDARG`) rather than crashing or dereferencing null DDI function pointers.

For a full “bring-up spec” (Win7 driver model overview, minimal D3D10DDI/D3D11DDI entrypoints to implement, swapchain behavior expectations, shader handling, and a test plan), see:

- [`docs/graphics/win7-d3d10-11-umd-minimal.md`](../../../../docs/graphics/win7-d3d10-11-umd-minimal.md)
- [`docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md`](../../../../docs/windows/win7-wddm11-d3d10-11-umd-alloc-map.md) (deprecated redirect; kept for link compatibility)
- [`docs/graphics/win7-d3d11ddi-function-tables.md`](../../../../docs/graphics/win7-d3d11ddi-function-tables.md) (DDI function-table checklist: REQUIRED vs stub for FL10_0)
- [`docs/graphics/win7-d3d10-11-umd-allocations.md`](../../../../docs/graphics/win7-d3d10-11-umd-allocations.md) (resource allocation contract: `CreateResource` → `pfnAllocateCb` + `D3DDDI_ALLOCATIONINFO`)
- [`docs/graphics/win7-d3d11-map-unmap.md`](../../../../docs/graphics/win7-d3d11-map-unmap.md) (`Map`/`Unmap` contract: `LockCb`/`UnlockCb`, DO_NOT_WAIT, staging readback sync)
- [`docs/graphics/win7-dxgi-swapchain-backbuffer.md`](../../../../docs/graphics/win7-dxgi-swapchain-backbuffer.md) (trace guide: swapchain backbuffer `CreateResource` parameters and allocation flags)

## Bring-up tracing (Win7)

For early Win7 bring-up it is often useful to trace:

* which `pfnGetCaps` query types the runtime is requesting, and
* which D3D10DDI entrypoints are being called unexpectedly (NULL-vtable avoidance).

See:

* `docs/graphics/win7-d3d10-caps-tracing.md`

## Feature level strategy

The initial feature claim is **D3D_FEATURE_LEVEL_10_0**:

- D3D11 runtime compatibility (can create a D3D11 device at FL10_0)
- Avoids implementing SM5.0-only features (tessellation/CS/etc.) early
- Matches the minimal pipeline required for a triangle sample

## Command stream mapping

This UMD emits `drivers/aerogpu/protocol/aerogpu_cmd.h` packets and references objects using **protocol resource handles** (`aerogpu_handle_t`), not an “allocation list index” model:

- Packets reference protocol objects via `resource_handle` / `buffer_handle` / `texture_handle` fields (`aerogpu_handle_t`), chosen by the UMD.
- When a resource is backed by guest memory, create packets may set `backing_alloc_id` (a stable `alloc_id`) and `backing_offset_bytes`. The `alloc_id` is resolved by looking it up in the optional per-submission `aerogpu_alloc_table` (`drivers/aerogpu/protocol/aerogpu_ring.h`), which maps `alloc_id → {gpa, size_bytes, flags}`. `backing_alloc_id` is a lookup key, not an index. `backing_alloc_id = 0` means “host allocated” (no guest backing).
- `aerogpu_handle_t` values are protocol object IDs; they are intentionally **not** WDDM allocation handles/IDs.

### Win7 submission invariant: allocation list drives the `alloc_table`

On Win7/WDDM 1.1, the KMD builds the per-submit `aerogpu_alloc_table` from the submission’s WDDM allocation list (`DXGK_ALLOCATIONLIST`), so the UMD must ensure:

- Any submission that includes packets requiring `alloc_id` resolution includes the corresponding **WDDM allocation handle** in the submit allocation list (so the KMD can emit the `alloc_id → gpa` mapping). This includes:
  - `CREATE_*` packets with `backing_alloc_id != 0`
  - `AEROGPU_CMD_RESOURCE_DIRTY_RANGE` for guest-backed resources (Map/Unmap upload paths)
  - `COPY_*` packets with `WRITEBACK_DST` (staging readback)
- Do not rely solely on “currently bound” state when building the list: these packets may be emitted while a resource is **not currently bound**, and still require the allocation to be listed for that submit.

The WDK-backed UMDs enforce this via `TrackWddmAllocForSubmitLocked()` in:

- `src/aerogpu_d3d10_umd_wdk.cpp`
- `src/aerogpu_d3d10_1_umd_wdk.cpp`
- `src/aerogpu_d3d11_umd_wdk.cpp`

Related lifetime rule: when destroying a **guest-backed** resource, emit `AEROGPU_CMD_DESTROY_RESOURCE` and flush/submit it before calling `pfnDeallocateCb` (so the submission does not reference a freed allocation handle).

The core emission happens in the WDK-facing UMD entrypoints
(`src/aerogpu_d3d10_1_umd_wdk.cpp`, `src/aerogpu_d3d11_umd_wdk.cpp`) and the shared
encoder/state tracker (`src/aerogpu_d3d10_11_internal.h`) by building a linear
command buffer consisting of:

```
[aerogpu_cmd_stream_header]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  ...
```

### Command stream writer

Command serialization uses the shared UMD implementation:

- `../common/aerogpu_cmd_stream_writer.h`

This provides both a `std::vector`-backed stream (portable bring-up/tests) and a span/DMA-backed stream (`{uint8_t* buf, size_t capacity}`) suitable for writing directly into a WDDM runtime-provided command buffer.

All append helpers return `nullptr` (and set `CmdStreamError`) on failure (for example: bounded DMA buffer full, or invalid payload arguments). Callers targeting a WDDM DMA buffer are expected to split/flush submissions and retry when out of space.

### Shared surface note

DXGI/D3D10/11 shared resource interop is implemented in the **Win7/WDDM 1.1 WDK builds** of this UMD:

- The protocol supports cross-process sharing via `AEROGPU_CMD_EXPORT_SHARED_SURFACE` /
  `AEROGPU_CMD_IMPORT_SHARED_SURFACE` and a stable cross-process `share_token` carried in preserved
  WDDM allocation private driver data (`aerogpu_wddm_alloc_priv.share_token` in
  `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
- Creating a shareable resource (for example: `D3D11_RESOURCE_MISC_SHARED`) causes the UMD to emit
  `AEROGPU_CMD_EXPORT_SHARED_SURFACE` exactly once after allocation, using the stable `share_token`
  returned in preserved WDDM allocation private driver data.
- Opening a shared resource (cross-process `OpenSharedResource`) causes the UMD to parse the preserved
  allocation private driver data and emit `AEROGPU_CMD_IMPORT_SHARED_SURFACE` using the same
  `share_token`.

On Win7/WDDM 1.1, `share_token` must be stable across guest processes. AeroGPU does
**not** use the numeric value of the D3D shared `HANDLE` as `share_token` (handle
values are process-local and not stable cross-process).

Canonical contract: on Win7/WDDM 1.1, the Win7 KMD generates a stable non-zero
`share_token` and persists it in the preserved WDDM allocation private driver data blob
(`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
dxgkrnl returns the same bytes on cross-process opens, so both processes observe the
same `share_token`.

The preserved WDDM allocation private-data blob (`drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`)
is also used to persist a stable `alloc_id` across CreateAllocation/OpenAllocation so the
KMD can build the per-submit allocation table for guest-backed resources.

For shared allocations, `alloc_id` must avoid collisions across guest processes and must stay in the UMD-owned range (`alloc_id <= 0x7fffffff`, non-zero).

Canonical contract and rationale: `docs/graphics/win7-shared-surfaces-share-token.md`.

Win7 validation/regression tests:

- `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc/`
- `drivers/aerogpu/tests/win7/d3d11_shared_texture_ipc/`
- `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc/`
- `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc/`

## Build

This code is intended to be built as a **DLL UMD** for Windows 7 SP1.

### Optional tracing (resource creation / swapchain bring-up)

For Win7 bring-up it is often useful to log the runtime's `CreateResource` inputs
for DXGI swapchain backbuffers. The UMD supports a lightweight trace flag:

* `AEROGPU_UMD_TRACE_RESOURCES`

When enabled, the UMD prints `CreateResource`, `RotateResourceIdentities`, and
`Present` details via the standard UMD logging helper (`AEROGPU_D3D10_11_LOG`),
tagged with `trace_resources:`.

The trace hooks are implemented in both the WDK-backed Win7 DDIs and the
repo-local ABI subset build so the same flag can be used regardless of header
source (`/p:AeroGpuUseWdkHeaders=1` vs `0`).

See `docs/graphics/win7-dxgi-swapchain-backbuffer.md` for the recommended probe
app and log interpretation workflow.

Build files provided:

- `aerogpu_d3d10_11.sln`
- `aerogpu_d3d10_11.vcxproj` (Win32 + x64)

The project is configured to output binaries that match the Win7 packaging INF:

- Win32: `aerogpu_d3d10.dll`
- x64: `aerogpu_d3d10_x64.dll`

### Exported entrypoints

The Win7 D3D10/D3D11 runtimes load the UMD and look up these exports by name:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

On Win32, `__stdcall` would normally decorate the symbol names (for example,
`_OpenAdapter10@4`). The build uses module-definition files to ensure the DLL
exports the undecorated names expected by the runtimes:

- `aerogpu_d3d10_x86.def`
- `aerogpu_d3d10_x64.def`

For cross-checking header drift and x86 stdcall export decoration against the
real Win7-era WDK header set, see the tooling-only probe:

- `tools/wdk_abi_probe/`

#### Quick validation

From a Visual Studio Developer Command Prompt, inspect the DLL exports:

```cmd
dumpbin /exports aerogpu_d3d10.dll
dumpbin /exports aerogpu_d3d10_x64.dll
```

Verify the output contains the **undecorated** entrypoints:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

On **Win32** builds, also confirm the raw stdcall-decorated names are present:

- `_OpenAdapter10@4`
- `_OpenAdapter10_2@4`
- `_OpenAdapter11@4`

Recommended build entrypoint (MSBuild/WDK10):

```cmd
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
```

CI builds the same solution (and stages outputs under `out/drivers/aerogpu/`) via `ci/build-drivers.ps1`.

Optional: `drivers\aerogpu\build\build_all.cmd` is a convenience wrapper around MSBuild/WDK10 that stages outputs under `drivers\aerogpu\build\out\win7\...`.

The real Win7 UMD must be compiled against the official D3D10/11 user-mode DDI
headers from the **Windows Driver Kit (WDK)** (Windows Kits):

- `d3d10umddi.h`
- `d3d10_1umddi.h`
- `d3d11umddi.h`
- `d3dumddi.h`

On a typical modern WDK install, `d3d11umddi.h` is under:

- `C:\Program Files (x86)\Windows Kits\10\Include\<ver>\um\`
- `C:\Program Files (x86)\Windows Kits\10\Include\<ver>\shared\`

The Visual Studio project enables WDK headers by defining
`AEROGPU_UMD_USE_WDK_HEADERS=1` when `/p:AeroGpuUseWdkHeaders=1` (the default for
the UMD project, and what `drivers\aerogpu\build\build_all.cmd` passes when
staging Win7 binaries).

For a repo-only/self-contained build (no UMDDI headers installed), pass
`/p:AeroGpuUseWdkHeaders=0` to compile against the repo’s minimal compat ABI.
This mode is **not** expected to be ABI-compatible with the real Win7 D3D11
runtime; use it only for local development/bring-up.

Optional: if you have a WinDDK-style root (Win7-era `inc\\{api,ddk}` layout), set
`/p:AeroGpuWdkRoot="C:\WinDDK\7600.16385.1"` (or `%WINDDK%`/`%WDK_ROOT%`/`%WDKROOT%`)
to add `$(AeroGpuWdkRoot)\inc\{api,ddk}` to the include path and validate the
expected headers exist. If `AeroGpuWdkRoot` is unset, the build falls back to
the toolchain's standard include paths (common for Windows Kits 10+ installs).

## Install / Register (INF)

On Windows 7, the D3D10/D3D11 runtimes load the driver’s UMD based on registry values written by the display driver INF:

- `UserModeDriverName` (`REG_SZ`): native-bitness D3D10/11 UMD filename (include `.dll`)
- `UserModeDriverNameWow` (`REG_SZ`, x64 only): 32-bit D3D10/11 UMD filename for WOW64 apps

In the Win7 packaging INF (`drivers/aerogpu/packaging/win7/aerogpu_dx11.inf`), this UMD is registered as:

```inf
[AeroGPU_Device_AddReg_x86]
HKR,,UserModeDriverName,%REG_SZ%,"aerogpu_d3d10.dll"

[AeroGPU_Device_AddReg_amd64]
HKR,,UserModeDriverName,%REG_SZ%,"aerogpu_d3d10_x64.dll"
HKR,,UserModeDriverNameWow,%REG_SZ%,"aerogpu_d3d10.dll"
```

Then ensure the DLLs are copied into the correct system directories during installation:

- x86 Windows: `System32\aerogpu_d3d10.dll`
- x64 Windows:
  - `System32\aerogpu_d3d10_x64.dll` (64-bit)
  - `SysWOW64\aerogpu_d3d10.dll` (32-bit)

After installation, reboot and confirm adapter open calls in DebugView (`OutputDebugString`), including the resolved DLL path:

- `aerogpu-d3d10_11: module_path=...`
- `aerogpu-d3d10_11: OpenAdapter11 ...`

## DDI call logging (Win7 bring-up)

For early bring-up the UMD can emit a lightweight, grep-friendly trace of runtime → UMD calls.

- Output format: **one line per call**, prefixed with `AEROGPU_D3D11DDI:`
- Sink: `OutputDebugStringA` (and optionally an on-disk log file)

Enable it by setting these environment variables **before launching the app**:

```cmd
set AEROGPU_D3D10_11_LOG=1
rem Optional: also append to a file
set AEROGPU_D3D10_11_LOG_FILE=C:\aerogpu_d3d10_11_umd.log
rem Optional: trace MIC/Low-IL labeling of shared counter mappings (GlobalHandleCounter)
set AEROGPU_LOG_MIC=1
```

Collect the output using one of:

- **DebugView** (Sysinternals): run DebugView and enable *Capture Win32*.
- **WinDbg**: attach to the process and watch the debug output stream.
