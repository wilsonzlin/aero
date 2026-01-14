# AeroGPU Win7 D3D9Ex UMD (User-Mode Display Driver)

This directory contains the **AeroGPU Direct3D 9Ex user-mode display driver** (UMD) for Windows 7 SP1.

The UMD’s job is to:

1. expose the D3D9 adapter/device entrypoints expected by the Win7 D3D9 runtime, and
2. translate D3D9 DDI calls into the **AeroGPU high-level command stream** (`drivers/aerogpu/protocol/aerogpu_cmd.h`).

The kernel-mode driver (KMD) is responsible for accepting submissions and forwarding them to the emulator. The UMD only targets the **command stream** ABI (`aerogpu_cmd.h`); the KMD↔emulator submission transport is an implementation detail of the KMD.

The in-tree Win7 KMD supports both the **versioned** ring/MMIO transport (`drivers/aerogpu/protocol/aerogpu_pci.h` + `aerogpu_ring.h`)
and the legacy bring-up transport (auto-detected via BAR0 MMIO magic). UMDs in this repo emit the versioned command stream
(`aerogpu_cmd.h`); see `drivers/aerogpu/kmd/README.md` for device model/VID selection.

The command stream does **not** reference resources by a per-submission “allocation-list index”; instead it uses two separate ID spaces:

- **Protocol resource handles** (`aerogpu_handle_t`, exposed in packets as `resource_handle` / `buffer_handle` / `texture_handle`, etc): these are 32-bit, UMD-chosen handles that identify logical GPU objects in the command stream. They are *not* WDDM allocation IDs/handles.
- **Backing allocation IDs** (`alloc_id`): a stable 32-bit ID for a WDDM allocation (not a process-local handle and not a per-submit index). When a resource is backed by guest memory, create packets may set `backing_alloc_id` to a non-zero `alloc_id`.
  - `alloc_id` is a **driver-defined** ID carried via WDDM allocation private driver data (`aerogpu_wddm_alloc_priv.alloc_id` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
- The KMD uses it to build the per-submit `aerogpu_alloc_table` (`drivers/aerogpu/protocol/aerogpu_ring.h`) mapping `alloc_id → {gpa, size_bytes, flags}` for the emulator. `backing_alloc_id` in packets is the `alloc_id` lookup key (not an index into an allocation list).
    - `flags` are per-submit and reflect read/write intent (via the WDDM 1.1 allocation list `WriteOperation` bit; `DXGK_ALLOCATIONLIST::Flags.Value & 0x1`). The host enforces `AEROGPU_ALLOC_FLAG_READONLY` by rejecting guest-memory writeback into read-only allocations.
  - For **shared** allocations, dxgkrnl preserves and replays the private-data blob on `OpenResource`/`OpenAllocation` so all guest processes observe consistent IDs.
  - `backing_alloc_id = 0` means “host allocated” (no guest backing). Portable/non-WDDM builds typically use host-allocated resources and leave `backing_alloc_id = 0`. In Win7/WDDM builds, most default-pool resources are backed by WDDM allocations and use non-zero `alloc_id` values so the KMD can build a per-submit `alloc_id → GPA` table for the emulator.

## Stub checklist

The canonical list of missing / stubbed D3D9UMDDI entrypoints lives later in this README: [Currently stubbed DDIs](#currently-stubbed-ddis).

If you add or remove a DDI stub, update that list so other workstreams can quickly assess bring-up risk.

## D3D9 device cursor (hardware cursor + software fallback)

The Win7 D3D9 runtime exposes a device-managed cursor API (`SetCursorProperties`, `SetCursorPosition`, `ShowCursor`).

On AeroGPU, cursor support is implemented in two layers:

1. **Hardware cursor (preferred)** — when the Win7 KMD exposes the cursor MMIO feature (`AEROGPU_FEATURE_CURSOR`), the D3D9 UMD
   programs the KMD cursor state via driver-private escapes (see `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h` ops
   `AEROGPU_ESCAPE_OP_SET_CURSOR_SHAPE/_POSITION/_VISIBILITY`). This is required for Win7 guest validation (`cursor_state_sanity`)
   which queries cursor MMIO state via `AEROGPU_ESCAPE_OP_QUERY_CURSOR`.
2. **Software cursor overlay (fallback)** — if the escape path is unavailable or the KMD returns `STATUS_NOT_SUPPORTED`, the UMD
   composites the cursor bitmap over the present source surface immediately before emitting `AEROGPU_CMD_PRESENT_EX`.

Supported cursor bitmap formats: `A8R8G8B8`, `X8R8G8B8` (treated as opaque alpha=1.0), `A8B8G8R8`.

Code anchors (see `src/aerogpu_d3d9_driver.cpp` unless noted):

- Cursor state DDIs: `device_set_cursor_properties_dispatch()` / `device_set_cursor_position_dispatch()` / `device_show_cursor_dispatch()`
- Cursor overlay at present time (software fallback): `overlay_device_cursor_locked()` (called by `device_present()` / `device_present_ex()`)

## Win7/WDDM submission callbacks (render vs present)

On Win7/WDDM 1.1, the D3D9 runtime provides a `D3DDDI_DEVICECALLBACKS` table during `CreateDevice`. The UMD must submit DMA buffers back to dxgkrnl via these callbacks so the KMD can:

- distinguish **render** vs **present** submissions (`DxgkDdiRender` vs `DxgkDdiPresent`), and
- build/attach the per-submit allocation table (`alloc_id → GPA`) for guest-backed resources.

In practice, different header/runtime combinations can expose different callback entrypoints. The AeroGPU D3D9 UMD prefers:

1. `pfnPresentCb` for present submissions and `pfnRenderCb` for render submissions, and
2. falls back to `pfnSubmitCommandCb` (`D3DDDIARG_SUBMITCOMMAND`) when needed.

For present submissions specifically, some runtimes expose only `pfnRenderCb` (with an explicit “present” bit in the callback args) while others route present work through `pfnSubmitCommandCb` (bypassing `DxgkDdiPresent`). AeroGPU supports both:

- when the callback arg struct can explicitly signal “present”, the UMD sets that bit so dxgkrnl routes the submit through `DxgkDdiPresent`; and
- otherwise, the UMD uses `pfnSubmitCommandCb` and relies on the stamped `AEROGPU_DMA_PRIV.Type` (and the KMD’s “build meta from allocation list” fallback) to keep submission classification and allocation-table attachment correct.

The UMD logs the available callback pointers once at `CreateDevice` so Win7 bring-up can confirm which path the runtime is using.

## Win7/WDDM DMA buffer acquisition (CreateContext vs Allocate/GetCommandBuffer)

Win7-era D3D runtimes are not entirely consistent about how they provide the **DMA command buffer** + **allocation list** + **DMA private data** needed for submission:

- Some runtimes return **persistent** pointers in `D3DDDIARG_CREATECONTEXT` and then rotate them through the submit callback out-params (e.g. `pNewCommandBuffer` / `pNewAllocationList`).
- Other runtimes return **NULL or undersized** buffers from `CreateContext` and expect the UMD to acquire per-submit buffers via the callback trio:
  - `pfnGetCommandBufferCb` (preferred when available), or
  - `pfnAllocateCb` / `pfnDeallocateCb`.

The AeroGPU D3D9 UMD supports both models. Command emission calls `ensure_cmd_space()`, which (in WDDM builds) routes through `wddm_ensure_recording_buffers()` to guarantee that:

- `Device::cmd` is bound to a runtime-owned DMA buffer large enough for the next packet,
- `AllocationListTracker` is rebound to the active runtime-provided allocation list, and
- `pDmaBufferPrivateData` is present and at least `AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES`.

When the UMD acquires transient buffers via `AllocateCb`, it returns them via `DeallocateCb` after submission (or at device teardown if the buffer was never submitted).
Some runtimes size the returned buffers to the exact requested byte count, so when using `AllocateCb` the UMD requests at
least one page (4KB) to avoid immediately reacquiring submit buffers after tracking allocations.
If submit-buffer reacquisition does occur while the command stream is still empty, `wddm_ensure_recording_buffers()`
preserves and replays any tracked allocations across the re-acquire (see `AllocationListTracker::snapshot_tracked_allocations()`
and `AllocationListTracker::replay_tracked_allocations()` in `src/aerogpu_wddm_alloc_list.*`).

### Runtime variance: pDmaBuffer vs pCommandBuffer

Some WDDM callback structs expose both:

- a base **DMA buffer** pointer/size (`pDmaBuffer` / `DmaBufferSize`), and
- a potentially offset **command buffer** pointer (`pCommandBuffer`).

When `pCommandBuffer` is an offset within the DMA buffer, the effective writable command-buffer capacity is reduced by the offset. The AeroGPU D3D9 UMD handles this by tracking `WddmContext::pDmaBuffer` separately and by adjusting capacities via `AdjustCommandBufferSizeFromDmaBuffer()` (`src/aerogpu_wddm_submit_buffer_utils.h`) whenever it must fall back to a `DmaBufferSize`-derived capacity.

### DMA buffer private data (UMD→dxgkrnl→KMD) and security

Win7/WDDM submission callbacks include a `pDmaBufferPrivateData` pointer + size.
dxgkrnl copies these bytes from user mode into kernel mode for every submission,
so the UMD must ensure they are **deterministic** and do not contain
uninitialized stack/heap bytes.

The AeroGPU D3D9 UMD initializes this blob immediately before each submission via
`InitWin7DmaBufferPrivateData()` (`src/aerogpu_d3d9_dma_priv.h`):

- validates the pointer is non-NULL and the size is at least
  `AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES` (16), otherwise fails the
  submit cleanly (log-once),
- zeros the 16-byte ABI prefix, and
- stamps a deterministic `AEROGPU_DMA_PRIV` header (including the submission
  type: render vs present) so even SubmitCommandCb-only runtimes convey a valid
  submit type to the KMD.

### Runtime variance: patch list + sync object

Win7 D3D9 runtimes (and different WDK header/interface vintages) can legitimately vary in the WDDM submission buffers returned by `CreateContext`:

- The patch-location list pointer/size (`pPatchLocationList` / `PatchLocationListSize`) may be **NULL** and/or **0-sized**.
- The monitored-fence sync object handle (`hSyncObject`) may be **0** on some paths.

AeroGPU intentionally uses a **no patch list** strategy and always submits with **`NumPatchLocations = 0`**.

For fence waiting / throttling:

- If `hSyncObject` is present, the UMD prefers kernel waits via `D3DKMTWaitForSynchronizationObject` for bounded waits (e.g. PresentEx max-frame-latency throttling).
- If `hSyncObject` is absent, the UMD falls back to polling the AeroGPU KMD fence counters via `D3DKMTEscape` (`AerogpuKmdQuery`), throttled to avoid spamming syscalls in tight EVENT-query polling loops.

## HWND occlusion handling (Present / PresentEx / CheckDeviceState)

On Windows 7, the D3D9Ex runtime (notably `dwm.exe`) uses the UMD’s handling of **occlusion/minimized** scenarios to avoid pathological present loops. AeroGPU implements a shared, best-effort HWND heuristic (`hwnd_is_occluded()` in `src/aerogpu_d3d9_driver.cpp`) used by:

- `pfnPresent` (`Device::Present`)
- `pfnPresentEx` (`Device::PresentEx`)
- `pfnCheckDeviceState` (`Device::CheckDeviceState`)

### Return behavior

- `CheckDeviceState(hWnd)`:
  - returns `S_PRESENT_OCCLUDED` when the destination window is considered occluded,
  - otherwise returns `S_OK` (or `D3DERR_DEVICELOST` if the device is lost).
- `Present` / `PresentEx`:
  - if the destination window is considered occluded, returns `S_PRESENT_OCCLUDED`.
    - The call is still treated as a **flush point**: the UMD submits any pending render work and advances the D3D9Ex present statistics (used by `GetPresentStats` / `GetLastPresentCount`).
    - The UMD does **not** submit a present packet while occluded.
  - otherwise, proceeds normally (including PresentEx max-frame-latency throttling and `D3DERR_WASSTILLDRAWING` behavior when `D3DPRESENT_DONOTWAIT` is used).

### Occlusion heuristics

The occlusion heuristic is intentionally **conservative** and **non-blocking** (it only queries cached Win32 window state; no waits). A window is treated as occluded when any of the following are true:

- The resolved top-level window is minimized/iconic (`IsIconic(topLevel) != 0`).
- The client rectangle has non-positive size (`GetClientRect(hwnd)` yields `width <= 0` or `height <= 0`).
  - If the present target is a child HWND, both the child and its root/top-level window are checked.
- Optional: when `AEROGPU_D3D9_OCCLUDE_INVISIBLE=1` is set, an invisible top-level window (`IsWindowVisible(topLevel) == FALSE`) is also treated as occluded.

Explicitly: `IsWindowVisible(topLevel) == FALSE` is **not** treated as occluded by default. Some Win7 guest tests intentionally present to hidden windows to validate pacing/throttling paths; treating invisible windows as occluded would change those return codes and can mask regressions.

Notes:

- `NULL` or stale/invalid HWNDs are treated as “not occluded” to avoid spurious `S_PRESENT_OCCLUDED` returns. When callers pass `NULL` to `Present`/`PresentEx`, AeroGPU falls back to the swap chain’s HWND (when available).

### Environment variable: `AEROGPU_D3D9_OCCLUDE_INVISIBLE`

`AEROGPU_D3D9_OCCLUDE_INVISIBLE` controls whether `IsWindowVisible(topLevel) == FALSE` is considered “occluded” by the heuristic above.

- Default: **disabled**
- Set `AEROGPU_D3D9_OCCLUDE_INVISIBLE=1` to enable treating invisible windows as occluded

This exists so Win7 bring-up/debugging can opt into more aggressive occlusion behavior when needed, without breaking hidden-window tests/pacing coverage by default.

## Command stream writer

UMD command emission uses a small serialization helper in:

- `src/aerogpu_cmd_stream_writer.h`

It supports both:

- a `std::vector`-backed stream (portable builds/tests), and
- a span/DMA-backed stream (`{uint8_t* buf, size_t capacity}`), suitable for writing directly into a WDDM runtime-provided command buffer.

Packets are always padded to 4-byte alignment and encode `aerogpu_cmd_hdr::size_bytes` accordingly, so unknown opcodes can be skipped safely.

All append helpers return `nullptr` (and set `CmdStreamError`) on failure. When using the span/DMA-backed mode, callers are expected to split/flush submissions and retry if the WDDM DMA buffer fills.

### Shared surfaces (D3D9Ex / DWM)

Cross-process shared resources are expressed explicitly in the command stream:

- `AEROGPU_CMD_EXPORT_SHARED_SURFACE` associates an existing `resource_handle` with a stable 64-bit `share_token`.
- `AEROGPU_CMD_IMPORT_SHARED_SURFACE` creates a new `resource_handle` aliasing the exported resource by `share_token`.
- `AEROGPU_CMD_RELEASE_SHARED_SURFACE` invalidates a `share_token` mapping on the host (emitted by the Win7 KMD when the final cross-process allocation wrapper is released; the D3D9 UMD does not emit this directly).

`share_token` must be stable across guest processes. On Win7/WDDM 1.1, AeroGPU does
**not** use the numeric value of the user-mode shared `HANDLE` as `share_token`: for real
NT handles the numeric value is process-local (commonly different after
`DuplicateHandle`), and some D3D9Ex stacks use token-style shared handles that
still must not be treated as a stable protocol key (and should not be passed to
`CloseHandle`).

Canonical contract: on Win7/WDDM 1.1, the Win7 KMD generates a stable non-zero 64-bit
`share_token` and persists it in the preserved WDDM allocation private driver data blob
(`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`),
which dxgkrnl returns verbatim on cross-process opens.

For allocations that may be referenced together in a single submission (notably DWM mixing shared surfaces with its own resources), `alloc_id` must avoid collisions across guest processes and must stay in the UMD-owned range (`alloc_id <= 0x7fffffff`). In the current AeroGPU D3D9 UMD:

- `alloc_id` values (for both shared and non-shared allocations) are derived from a cross-process monotonic counter (`allocate_shared_alloc_id_token()` in `src/aerogpu_d3d9_driver.cpp`, backed by a named file mapping + `InterlockedIncrement64`, masked to 31 bits with 0 skipped).
- `share_token` is returned by the KMD via `aerogpu_wddm_alloc_priv.share_token` (filled during `DxgkDdiCreateAllocation` and preserved across cross-process opens).

See `docs/graphics/win7-shared-surfaces-share-token.md` for the end-to-end contract and the full Win7 cross-process
shared-surface validation test list (under `drivers/aerogpu/tests/win7/`).

#### Cross-API note: D3D9Ex consuming DXGI shared handles (DWM scenario)

On Windows 7, the desktop compositor (`dwm.exe`, D3D9Ex) commonly consumes **DXGI shared handles**
produced by D3D10/D3D11 apps. In this case, the preserved WDDM allocation private data blob is
typically `aerogpu_wddm_alloc_priv_v2` and the `reserved0` field may carry a **pitch encoding**
(rather than a D3D9 `format/width/height` descriptor marker).

To support this DWM-style path, the AeroGPU D3D9 UMD `OpenResource` implementation falls back to
the v2 metadata (`width/height/DXGI format/row_pitch_bytes`) and maps a small set of common DXGI
formats (BGRA/RGBA, plus common 16-bit formats like B5G6R5 and B5G5R5A1) to their nearest D3D9
`D3DFORMAT` values, so `Lock` can report the correct `RowPitch` and CPU-side helpers can compute a
consistent surface layout.

## Build

This project is intended to be built in a Windows/WDK environment as a DLL for both x86 and x64:

- `aerogpu_d3d9.dll` (x86 / Win32)
- `aerogpu_d3d9_x64.dll` (x64)

Build files:

- Visual Studio project: `aerogpu_d3d9_umd.vcxproj`
- Exports:
  - `aerogpu_d3d9_x86.def` (exports `OpenAdapter`, `OpenAdapter2`, `OpenAdapterFromHdc`, `OpenAdapterFromLuid` from stdcall-decorated x86 symbols)
  - `aerogpu_d3d9_x64.def` (exports `OpenAdapter`, `OpenAdapter2`, `OpenAdapterFromHdc`, `OpenAdapterFromLuid`)

Recommended build entrypoint (MSBuild/WDK10):

```cmd
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32 /p:AeroGpuUseWdkHeaders=1
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64 /p:AeroGpuUseWdkHeaders=1
```

CI builds the same solution (and stages outputs under `out/drivers/aerogpu/`) via `ci/build-drivers.ps1`.

Optional: `drivers\aerogpu\build\build_all.cmd` is a convenience wrapper around MSBuild/WDK10 that stages outputs under `drivers\aerogpu\build\out\win7\...`.

## Win7 WDK ABI verification (recommended)

The D3D9 runtime loads the UMD by **ABI contract**: exported entrypoint names, calling conventions, and the exact layout of D3D9UMDDI/WDDM structs passed across the boundary.

To make ABI drift obvious *before* you debug a Win7 loader crash, the repo includes:

- A standalone **WDK ABI probe** tool:
  - `tools/wdk_abi_probe/`
- Optional **compile-time ABI asserts** wired into WDK builds:
  - `src/aerogpu_d3d9_wdk_abi_asserts.h` (included automatically; inert unless `AEROGPU_D3D9_USE_WDK_DDI=1`, which is
    implied by `AEROGPU_UMD_USE_WDK_HEADERS=1` / `/p:AeroGpuUseWdkHeaders=1`)

### Step-by-step

1. **Run the probe in a Win7-era WDK environment** (x86 + x64):
   - Follow: `tools/wdk_abi_probe/README.md`
   - Save the output for both architectures.

2. **Update `.def` exports (x86)** if needed:
   - Compare the probe’s “x86 stdcall decoration” section against:
     - `aerogpu_d3d9_x86.def`
   - The `@N` stack byte counts must match.

3. **Freeze ABI expectations (checked-in; recommended)**:
   - The repo pins ABI-critical values in:
     - `src/aerogpu_d3d9_wdk_abi_expected.h`
   - If you update the WDK/toolchain and ABI asserts start failing, regenerate the expected header from probe output:
     - `tools/wdk_abi_probe/gen_expected_header.py` (see `tools/wdk_abi_probe/README.md`)
   - In the MSBuild project, strict ABI enforcement is enabled automatically for WDK-header builds via:
     - `AEROGPU_D3D9_WDK_ABI_ENFORCE_EXPECTED` (set when `/p:AeroGpuUseWdkHeaders=1`).

4. **Rebuild the UMD**:
    - In WDK mode (`/p:AeroGpuUseWdkHeaders=1`), the build will fail if:
      - the WDK headers/toolchain no longer match the expected Win7 ABI, or
      - the UMD no longer compiles cleanly against the canonical Win7 D3D9UMDDI headers (the code uses
        member-name tolerant accessors to handle minor header drift).

### Notes

- The code in `include/aerogpu_d3d9_umd.h` includes a tiny “compat” subset of the Win7 D3D9 UMD DDI types so the core translation code is self-contained in this repository. When building in a real Win7 WDK environment, define `AEROGPU_UMD_USE_WDK_HEADERS=1` (or set `/p:AeroGpuUseWdkHeaders=1` in the VS project) to compile against the canonical WDK headers (`d3dumddi.h`, `d3d9umddi.h`).
- For ABI verification against the real Win7 D3D9 UMD headers (struct sizes/offsets + x86 stdcall export decoration), see `tools/wdk_abi_probe/`.
- Logging is done via `OutputDebugStringA` (view with DebugView/WinDbg) and is intentionally lightweight.
  - Set `AEROGPU_D3D9_LOG_SUBMITS=1` before loading the UMD to enable per-submission fence logs (useful for `drivers/aerogpu/tests/win7/d3d9ex_submit_fence_stress` and debugging submit ordering).
  - Set `AEROGPU_LOG_MIC=1` before loading the UMD to trace whether the cross-process counter file mappings (e.g. `Local\\AeroGPU.GlobalHandleCounter`, `Local\\AeroGPU.D3D9.ShareToken.<luid>`) were created/opened with a Low Integrity label (or fell back to the legacy NULL-DACL path).

## Install / Register (INF)

On Windows 7, the D3D9 runtime loads the display driver’s UMD(s) based on registry values written by the display driver INF. For D3D9, this is typically done via `InstalledDisplayDrivers` (base name, no extension).

In the Win7 packaging INFs in this repo (`drivers/aerogpu/packaging/win7/aerogpu.inf` and `aerogpu_dx11.inf`), the D3D9 UMD is registered via `InstalledDisplayDrivers` (base name, no extension):

```inf
[AeroGPU_Device_AddReg_x86]
HKR,,InstalledDisplayDrivers,%REG_MULTI_SZ%,"aerogpu_d3d9"

[AeroGPU_Device_AddReg_amd64]
HKR,,InstalledDisplayDrivers,%REG_MULTI_SZ%,"aerogpu_d3d9_x64"
HKR,,InstalledDisplayDriversWow,%REG_MULTI_SZ%,"aerogpu_d3d9"
```

Then ensure the DLLs are copied into the correct system directories during installation:

- x86 Windows: `System32\aerogpu_d3d9.dll`
- x64 Windows:
  - `System32\aerogpu_d3d9_x64.dll` (64-bit)
  - `SysWOW64\aerogpu_d3d9.dll` (32-bit)

After installation, reboot (or restart the display driver) and confirm:

1. DWM starts without falling back to Basic mode.
2. Debug output shows `module_path=...`, `OpenAdapterFromHdc`/`OpenAdapterFromLuid` (depending on caller), and subsequent command submissions.

## Supported feature subset (bring-up)

The current implementation targets:

- **DWM/Aero composition** (D3D9Ex)
- the in-tree Win7 validation programs under `drivers/aerogpu/tests/win7/`

### Core rendering / formats

- **Render targets / swapchain backbuffers**: `D3DFMT_X8R8G8B8` (opaque alpha), `D3DFMT_A8R8G8B8`, `D3DFMT_A8B8G8R8`,
  `D3DFMT_R5G6B5`, `D3DFMT_X1R5G5B5`, `D3DFMT_A1R5G5B5`
- **Depth/stencil**: `D3DFMT_D24S8`
- **Mipmapped textures**: default-pool textures with `MipLevels > 1` (and array layers via `Depth > 1`) are supported for
  common uncompressed formats (validated by `d3d9_mipmapped_texture_smoke`). BC/DXT mip chains are also supported when BC
  formats are exposed (see “BC/DXT textures” below).
- Cube textures are supported (advertised via `D3DPTEXTURECAPS_CUBEMAP`) and are represented as 2D array textures with
  6 layers (`Depth/array_layers == 6`). Some runtime/header combinations may not populate a meaningful `Depth` field for
  cube resources (it may be `Depth == 1`); the UMD normalizes `D3DRTYPE_CUBETEXTURE` resources to 6 layers. Cube textures
  must be square; invalid descriptors are rejected at `CreateResource` / `OpenResource` time. Volume textures
  (`D3DRTYPE_VOLUME` / `D3DRTYPE_VOLUMETEXTURE`) are not supported.
- Compatibility: some D3D9 runtimes/WDK header vintages may pass `Type == 0` for non-buffer surface resources; the UMD
  treats it as a non-array 2D surface descriptor (`Depth == 1`) rather than rejecting it as an unknown type.
- On the Win7/WDDM path, multi-subresource textures currently fall back to **host-backed storage** (no guest allocation /
  `alloc_id`), because guest-backed allocations are single-subresource today (see `force_host_backing` in
  `device_create_resource()`).
- Shared resources still require `MipLevels == 1` and `Depth == 1` (single-allocation MVP shared-surface policy).
- `pfnGenerateMipSubLevels` is implemented as a CPU downsample for:
  - `A8R8G8B8` / `X8R8G8B8` / `A8B8G8R8`
  - packed 16-bit RGB formats: `R5G6B5` / `X1R5G5B5` / `A1R5G5B5`
  - block-compressed formats: `D3DFMT_DXT1..DXT5` (when exposed; see “BC/DXT textures” below)
  - see `device_generate_mip_sub_levels()` in `src/aerogpu_d3d9_driver.cpp`.
- **Packed 16-bit RGB formats**: `D3DFMT_R5G6B5`, `D3DFMT_X1R5G5B5`, `D3DFMT_A1R5G5B5` are supported for
  render targets (including swapchain backbuffers) and texture sampling (validated by `d3d9ex_texture_16bit_formats`;
  `d3d9_texture_16bit_sampling` additionally exercises `R5G6B5` and optionally `A1R5G5B5`). `X1R5G5B5` is treated as
  alpha=1 when sampling.
- **BC/DXT textures**: `D3DFMT_DXT1..DXT5` are only exposed when the active device reports
  ABI minor `>= 2` via `KMTQAITYPE_UMDRIVERPRIVATE` (`aerogpu_umd_private_v1.device_abi_version_u32`).
  - When unsupported, `GetCaps(GETFORMAT*)` omits them and `CreateResource` rejects them to avoid emitting
    packets older hosts can't decode.

### Draw calls

- VB/IB draws: `DrawPrimitive*` / `DrawIndexedPrimitive*` with DEFAULT-pool buffers, including dynamic
  `Lock/Unlock` dirty-range tracking (`d3d9ex_multiframe_triangle`, `d3d9ex_vb_dirty_range`).
  - Dynamic buffer locks honor `D3DLOCK_DISCARD` / `D3DLOCK_NOOVERWRITE` via buffer renaming + in-flight range tracking
    (validated by `d3d9_dynamic_vb_lock_semantics`).
  - Stream instancing (`SetStreamSourceFreq`) is supported for a bring-up subset of shader-based triangle-list draws
    (requires a user VS) via CPU expansion of per-instance streams/indices into scratch UP buffers (validated by
    `d3d9ex_instancing_sanity`).
- User-pointer draws: `DrawPrimitiveUP` / `DrawIndexedPrimitiveUP` (`d3d9ex_triangle`,
  `d3d9ex_draw_indexed_primitive_up`, `d3d9ex_fixedfunc_textured_triangle`, `d3d9ex_fixedfunc_texture_stage_state`).

### Blit / compositor operations

- `ColorFill`, `UpdateSurface`, `UpdateTexture` and `StretchRect`-style copies (validated by `d3d9ex_stretchrect`).
- `GetRenderTargetData` readback into `D3DPOOL_SYSTEMMEM` surfaces (used by most rendering tests).
  - When the device exposes `AEROGPU_UMDPRIV_FEATURE_TRANSFER` (ABI minor `>= 1`), the UMD emits `AEROGPU_CMD_COPY_TEXTURE2D`
    with `AEROGPU_COPY_FLAG_WRITEBACK_DST` so the host writes pixels into the destination surface's backing allocation.
    This requires allocation-backed systemmem surfaces (`backing_alloc_id != 0`).
    - In Win7/WDDM builds, the UMD backs `D3DPOOL_SYSTEMMEM` surfaces with a guest allocation (creating a system-memory
      allocation when needed) so writeback lands in guest memory and `LockRect` can read the final pixels.
    (The same transfer/writeback path is also used for full-surface `CopyRects` into allocation-backed systemmem surfaces.)
  - Otherwise, the UMD falls back to a submit+wait and a CPU-side copy.
- `WaitForVBlank` and `GetRasterStatus` (scanline/vblank) are implemented for pacing/diagnostics (validated by `d3d9ex_dwm_ddi_sanity` and `d3d9_raster_status_sanity`).
- **EVENT queries (DWM):** `CreateQuery`/`IssueQuery`/`GetQueryData` are implemented for `D3DQUERYTYPE_EVENT` only; other query
  types return `D3DERR_NOTAVAILABLE` (see `device_create_query()` / `device_issue_query()` / `device_get_query_data()` in
  `src/aerogpu_d3d9_driver.cpp`; validated by `d3d9ex_query_latency` and `d3d9ex_event_query`).

Unsupported states are handled defensively; unknown state enums are accepted and forwarded as generic “set render/sampler state” commands so the emulator can decide how to interpret them.

### Fixed-function vertex formats (FVF)

The AeroGPU D3D9 UMD includes a small **fixed-function fallback path** used by DWM and older D3D9 apps that rely on FVFs instead of explicit shaders + vertex declarations.

Supported FVF combinations (bring-up subset):

- **Pre-transformed** (`POSITIONT`):
  - `D3DFVF_XYZRHW | D3DFVF_DIFFUSE`
  - `D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1`
  - `D3DFVF_XYZRHW | D3DFVF_TEX1` (no per-vertex diffuse; driver supplies default white)
- **Untransformed** (`POSITION`):
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE` (WVP transform)
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1` (WVP transform)
  - `D3DFVF_XYZ | D3DFVF_TEX1` (no per-vertex diffuse; driver supplies default white; WVP transform)
  - `D3DFVF_XYZ | D3DFVF_NORMAL` (no per-vertex diffuse; driver supplies default white when unlit; WVP transform; optional fixed-function lighting)
  - `D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_TEX1` (no per-vertex diffuse; driver supplies default white when unlit; WVP transform; optional fixed-function lighting)
  - `D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_DIFFUSE` (WVP transform; optional fixed-function lighting)
  - `D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_DIFFUSE | D3DFVF_TEX1` (WVP transform; optional fixed-function lighting)

Code anchors (see `src/aerogpu_d3d9_driver.cpp` unless noted):

- Fixed-function FVF → internal variant/decl mapping (table-driven; `src/aerogpu_d3d9_objects.h`):
  - `enum class FixedFuncVariant`
  - `kFixedFuncVariantDeclTable` (canonical FVF + canonical vertex-declaration signature)
  - `fixedfunc_variant_from_fvf()` / `fixedfunc_fvf_from_variant()` / `fixedfunc_decl_desc()`
- `fixedfunc_fvf_supported()` (internal FVF-driven decl subset required by patch emulation; **XYZRHW + DIFFUSE (+ optional TEX1) variants only**)
- `ensure_fixedfunc_pipeline_locked()` / `ensure_shader_bindings_locked()`
- Fixed-function PS generation from texture stage state (stages 0..3): `fixedfunc_ps_key_locked()` + `ensure_fixedfunc_pixel_shader_locked()` (`fixedfunc_ps20` token builder)
- Fixed-function shader token streams: `src/aerogpu_d3d9_fixedfunc_shaders.h` (`fixedfunc::kVsPassthroughPosColor`, `fixedfunc::kVsPassthroughPosColorTex1`, `fixedfunc::kVsWvpPosColor`, `fixedfunc::kVsWvpPosColorTex0`, `fixedfunc::kVsTransformPosWhiteTex1`, etc)
- XYZRHW conversion path: `fixedfunc_fvf_is_xyzrhw()` + `convert_xyzrhw_to_clipspace_locked()`
- Fixed-function WVP VS constant upload: `fixedfunc_fvf_needs_matrix()` + `ensure_fixedfunc_wvp_constants_locked()` (`WORLD0*VIEW*PROJECTION` into reserved `c240..c243`)
- Fixed-function lighting VS constant upload (NORMAL variants): `ensure_fixedfunc_lighting_constants_locked()` (reserved `c208..c236` constant block when `D3DRS_LIGHTING` is enabled)
- FVF selection paths: `device_set_fvf()` and the `SetVertexDecl` pattern detection in `device_set_vertex_decl()`
  - `device_set_fvf()` synthesizes/binds an internal vertex declaration for each supported fixed-function FVF
    (see supported combinations above), so the draw path can bind a known input layout.
  - `device_set_vertex_decl()` pattern-matches common decl layouts and sets an implied `dev->fvf` so the fixed-function
    fallback path can activate even when `SetFVF` is never invoked.

FVF-derived input layouts for user shaders:

- Many D3D9 apps bind **user shaders** but still use `SetFVF` (instead of `SetVertexDeclaration`) to describe vertex
  inputs. In this case the runtime expects the driver to derive an input layout from the FVF.
- `device_set_fvf()` therefore also translates a broader common subset of FVFs into an internal vertex declaration
  (input layout) even when the fixed-function fallback is not active.
- Supported for **input layout translation only** (does *not* imply fixed-function lighting/stage-state emulation):
  - `D3DFVF_XYZ`, `D3DFVF_XYZW`, `D3DFVF_XYZRHW`, or `D3DFVF_XYZB1..D3DFVF_XYZB5`
    - optional `D3DFVF_LASTBETA_UBYTE4` / `D3DFVF_LASTBETA_D3DCOLOR` (BLENDINDICES)
  - optional `D3DFVF_NORMAL`
  - optional `D3DFVF_PSIZE`
  - optional `D3DFVF_DIFFUSE` (COLOR0) and `D3DFVF_SPECULAR` (COLOR1)
  - `D3DFVF_TEX0..D3DFVF_TEX8` with per-set `D3DFVF_TEXCOORDSIZE1/2/3/4`
  - Note: internal decls are cached per-device keyed by the full FVF DWORD and capped at 256 entries.

Not yet implemented for **fixed-function emulation** (examples; expected by some fixed-function apps):

- Fixed-function specular (`D3DFVF_SPECULAR`, `D3DRS_SPECULARENABLE`, `D3DMATERIAL9.Specular/Power`)
- More complete fixed-function lighting (spot cone falloff, `Attenuation1/2`, more lights, etc). The bring-up path
  implements only a small bounded subset (see “Limitations (bring-up)” below).
- Vertex blending / indexed vertex blending (`D3DFVF_XYZB*`, `D3DRS_VERTEXBLEND`, `D3DRS_INDEXEDVERTEXBLENDENABLE`, etc)
- Multiple texture coordinate sets (`D3DFVF_TEX2+`)
- Full fixed-function texture stage state coverage (many `D3DTSS_*` values are cached-only today, including `D3DTSS_TEXCOORDINDEX`).
- Full fixed-function fog mode/semantics coverage (the bring-up path implements only linear fog via `D3DRS_FOG*`; EXP/EXP2, range fog, and exact D3D9 fog coordinate semantics are still TODO).

Implementation notes (bring-up):

- For `POSITIONT`/`XYZRHW` vertices, the fallback path converts screen-space `XYZRHW` to clip-space on the CPU
  (`convert_xyzrhw_to_clipspace_locked()` in `src/aerogpu_d3d9_driver.cpp`) and then draws using a tiny built-in
  `vs_2_0`/`ps_2_0` pair.
  - The conversion uses the current D3D9 viewport (`X/Y/Width/Height`) and inverts the D3D9 `-0.5` pixel center
    convention so typical pre-transformed vertices line up with pixel centers.
- When `D3DFVF_DIFFUSE` is omitted (supported `*TEX1` subsets and `XYZ|NORMAL{,TEX1}`), the fixed-function fallback uses
  internal vertex shader variants that supply a constant **opaque white** diffuse color (unlit). Lit `NORMAL` variants
  compute a lit diffuse color without per-vertex diffuse.
- Supported FVFs can be selected via either `SetFVF` (internal declaration synthesized) or `SetVertexDecl` (UMD infers an
  implied FVF from common declaration layouts in `device_set_vertex_decl()`).
  - For untransformed `D3DFVF_XYZ*` fixed-function FVFs, the fixed-function fallback uses small internal vertex shader
  variants that apply `WORLD0*VIEW*PROJECTION` from a reserved VS constant range (`c240..c243`) uploaded by
  `ensure_fixedfunc_wvp_constants_locked()`.
  - When the **full fixed-function pipeline** is active (no user shaders bound), `SetTransform`/`MultiplyTransform` may
    also upload the constants eagerly (not just at draw time) so the next draw does not redundantly re-upload unchanged
    constants.
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE` uses `fixedfunc::kVsWvpPosColor`.
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1` uses `fixedfunc::kVsWvpPosColorTex0`.
  - `D3DFVF_XYZ | D3DFVF_TEX1` (no diffuse) uses `fixedfunc::kVsTransformPosWhiteTex1`.
  - `D3DFVF_XYZ | D3DFVF_NORMAL{,TEX1}` (no diffuse) uses the `fixedfunc::kVsWvpPosNormalWhite{,Tex0}` (unlit) variants,
    or `fixedfunc::kVsWvpLitPosNormal{,Tex1}` when `D3DRS_LIGHTING` is enabled.
  - For `D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_DIFFUSE{,TEX1}`, the fixed-function fallback selects a normal-aware WVP VS
    variant. When `D3DRS_LIGHTING` is enabled, it uses the lit variant and uploads a reserved lighting constant block
    (`c208..c236`) via `ensure_fixedfunc_lighting_constants_locked()`.
  - The reserved constant ranges are intentionally high so they are unlikely to collide with app/user shader constants when
    switching between fixed-function and programmable paths.
    - Even so, when switching back to fixed-function WVP (and lit `NORMAL` variants) the UMD forces a re-upload
      (`fixedfunc_matrix_dirty` / `fixedfunc_lighting_dirty`) since user shaders may have written overlapping VS constant
      registers.
      - Some runtimes expect the WVP refresh to happen immediately when the user VS is unbound (`SetShader(VS, NULL)`), so the
        UMD may upload WVP constants at shader-unbind time (not just lazily at the next draw).
  - See also: `docs/graphics/win7-d3d9-fixedfunc-wvp.md` (WVP draw-time paths + `ProcessVertices` notes).
- Shader-stage interop is supported: when exactly one stage is bound (VS-only or PS-only), the draw paths bind a
  fixed-function fallback shader for the missing stage at draw time (see `ensure_shader_bindings_locked()`).
  - VS-only interop (PS is NULL) uses a fixed-function PS generated from texture stage state (stages 0..3; validated by `d3d9_shader_stage_interop`).
  - PS-only interop (VS is NULL) uses a fixed-function VS derived from the active FVF (validated by `d3d9ex_ps_only_triangle`).
- For indexed draws in this mode, indices may be expanded into a temporary vertex stream (conservative but sufficient
  for bring-up).
- Patch rendering (`DrawRectPatch` / `DrawTriPatch`) is supported for the bring-up subset of **cubic Bezier patches**:
  the UMD tessellates the patch on the CPU into scratch UP buffers and draws it through the same fixed-function fallback
  pipeline. `DeletePatch` evicts the cached tessellation for a handle.

Limitations (bring-up):

- The fixed-function fallback supports only the FVFs listed above (see `ensure_fixedfunc_pipeline_locked()` in `src/aerogpu_d3d9_driver.cpp`). Other FVFs may be accepted for `SetFVF`/`GetFVF`/state-block round-tripping, but fixed-function draws will fail with `D3DERR_INVALIDCALL` if the active FVF is unsupported.
- For `D3DFVF_XYZRHW*` FVFs, the UMD converts `POSITIONT` (screen-space `XYZRHW`) vertices to clip-space on the CPU (`convert_xyzrhw_to_clipspace_locked()`).
  - The conversion uses the current viewport (`X/Y/Width/Height`) and treats `w = 1/rhw` (with a safe fallback when
    `rhw==0`); `z` is passed through as D3D9 NDC depth (`0..1`) and currently ignores viewport `MinZ`/`MaxZ`.
- For untransformed `D3DFVF_XYZ*` fixed-function FVFs, the bring-up path uses internal WVP vertex shaders with a reserved VS
  constant range (`c240..c243`) uploaded by `ensure_fixedfunc_wvp_constants_locked()`.
  - For `D3DFVF_XYZ | D3DFVF_NORMAL{,DIFFUSE}{,TEX1}`, the bring-up path also applies the minimal fixed-function lighting
    subset below when `D3DRS_LIGHTING` is enabled.
  (Implementation notes: [`docs/graphics/win7-d3d9-fixedfunc-wvp.md`](../../../../docs/graphics/win7-d3d9-fixedfunc-wvp.md).)
- Fixed-function lighting/material is implemented only for a **minimal subset**:
  - gated by `D3DRS_LIGHTING` (off = unlit behavior),
  - uses `D3DRS_AMBIENT` as a global ambient term,
  - consumes a bounded set of enabled lights (packed from `SetLight`/`LightEnable` state):
    - up to 4 directional lights
    - up to 2 point lights (spot treated as point; no cone falloff)
  - consumes `D3DMATERIAL9` diffuse/ambient/emissive (`SetMaterial`),
  - computes a simple per-vertex `N·L` diffuse term and passes the lit vertex color to the pixel shader,
  - point lights use constant attenuation (`1/Attenuation0`) and a range clamp based on `dist²` (`max(1 - dist²/range², 0)`).
  Specular, spot cone attenuation, and linear/quadratic attenuation are not implemented yet.
  Note: lighting is only applied for the full fixed-function fallback pipeline (no user VS/PS bound); in shader-stage
  interop paths the light state is cached-only.
- The fixed-function fallback's `TEX1` path consumes a single set of texture coordinates (`TEXCOORD0`) and uses the first
  two components as `(u, v)`. `TEXCOORD0` may be declared as `float1/2/3/4` via `D3DFVF_TEXCOORDSIZE*` (extra components
  are ignored; `float1` implies `v=0`). Multiple texture coordinate sets still require user shaders (layout translation is
  supported; fixed-function shading is not).
- Texture stage state (`D3DTSS_COLOROP/ALPHAOP` + `D3DTSS_COLORARG*/ALPHAARG*`) is interpreted for a guarded, multi-stage subset (stages 0..3) to synthesize a `ps_2_0` fixed-function combiner shader (validated by `d3d9ex_fixedfunc_texture_stage_state` and `d3d9_fixedfunc_multitexture`):
  - Supported COLOR/ALPHA ops:
    - `DISABLE`, `SELECTARG1`, `SELECTARG2`
    - `MODULATE`, `MODULATE2X`, `MODULATE4X`
    - `ADD`, `SUBTRACT`, `ADDSIGNED`
    - `BLENDTEXTUREALPHA`, `BLENDDIFFUSEALPHA`
  - Supported arg sources:
    - `DIFFUSE` / `CURRENT` (stage0 `CURRENT` treated as diffuse)
    - `TEXTURE`
    - `TFACTOR` (`D3DRS_TEXTUREFACTOR`; provided to the fixed-function PS as `c255` in normalized RGBA)
  - Supported arg modifiers for the sources above: `COMPLEMENT`, `ALPHAREPLICATE`
  - Guardrails:
    - Unsupported stage-state combinations are cached for `Get*`/state blocks, but fixed-function draws fail cleanly with `D3DERR_INVALIDCALL` (only when the fixed-function path is actually used; i.e., no user pixel shader is bound).
    - If a stage references `TEXTURE` but the corresponding stage texture is unbound, the UMD truncates the stage chain (passthrough) instead of emitting a `texld` from an invalid slot.
    - Stage chain termination follows D3D9 semantics: `COLOROP=DISABLE` on stage *N* disables stage *N* and all subsequent stages.
  - Stages `> 3` are cached only for `Get*`/state blocks and are ignored by the fixed-function shader selection for now.
- Fixed-function lighting/material beyond the minimal subset above is cached-only (for `Get*` and state blocks) and is not
  forwarded into shader generation (specular, spot cone falloff, additional attenuation terms, more lights, etc).

### Known limitations / next steps

- **Fixed-function pipeline is still limited:** `ensure_fixedfunc_pipeline_locked()` synthesizes a small `ps_2_0` token stream for the supported subset of texture stage state (stages 0..3; see above).
  - The pixel shader bytecode is generated at runtime by a tiny “ps_2_0 token builder” in `src/aerogpu_d3d9_driver.cpp` (no offline shader generation step is required).
  - The current implementation supports up to 4 texture stages (`MaxTextureBlendStages = 4`) and uses `TEXCOORD0` for all stages (no `D3DTSS_TEXCOORDINDEX` support yet).
  - Fixed-function fog is implemented for a minimal subset (linear fog via `D3DRS_FOG*`) in the fixed-function fallback pixel shaders; EXP/EXP2 and full fog semantics are still TODO.
    - Fog uses `TEXCOORD0.z`. For `D3DFVF_XYZRHW | D3DFVF_DIFFUSE` (RHW_COLOR), the base passthrough VS already provides this coordinate (`oT0=v0`), so fog does not require a dedicated fog VS variant.
  - More complete fixed-function lighting (specular, spot cones, more lights, etc) is still TODO.
- **Shader int/bool constants are supported:** `DeviceSetShaderConstI/B` (`device_set_shader_const_i_impl()` / `device_set_shader_const_b_impl()` in `src/aerogpu_d3d9_driver.cpp`) update the UMD-side caches + state blocks and emit constant updates into the AeroGPU command stream (`AEROGPU_CMD_SET_SHADER_CONSTANTS_I/B`).
- **Bring-up no-ops:** `pfnSetConvolutionMonoKernel` and `pfnSetDialogBoxMode` are wired as `S_OK` no-ops via
  `AEROGPU_D3D9_DEFINE_DDI_NOOP(...)` in the “Stubbed entrypoints” section of `src/aerogpu_d3d9_driver.cpp`.
  `pfnComposeRects` is also accepted as an `S_OK` no-op (see `device_compose_rects()`).

### Validation

This subset is validated via:

- **Host-side unit tests** under `drivers/aerogpu/umd/d3d9/tests/` (command-stream and fixed-function/FVF translation coverage).
- **Win7 guest tests** under `drivers/aerogpu/tests/win7/` (recommended smoke tests:
  `umd_private_sanity`, `transfer_feature_sanity`,
  `d3d9ex_triangle`, `d3d9_mipmapped_texture_smoke`, `d3d9ex_fixedfunc_textured_triangle`,
  `d3d9ex_fixedfunc_texture_stage_state`, `d3d9_fixedfunc_xyz_diffuse`, `d3d9_fixedfunc_xyz_diffuse_tex1`,
  `d3d9_fixedfunc_multitexture`,
  `d3d9_fixedfunc_textured_wvp`, `d3d9_fixedfunc_wvp_triangle`, `d3d9_fixedfunc_fog_smoke`, `d3d9_fixedfunc_lighting_directional`,
  `d3d9_fixedfunc_lighting_multi_directional`, `d3d9_fixedfunc_lighting_point`,
  `d3d9_shader_stage_interop`, `d3d9ex_ps_only_triangle`,
  `d3d9ex_texture_16bit_formats`, `d3d9_texture_16bit_sampling`, `d3d9_patch_sanity`, `d3d9_patch_rendering_smoke`,
  `d3d9_process_vertices_sanity`, `d3d9_process_vertices_smoke`,
  `d3d9_caps_smoke`, `d3d9_validate_device_sanity`, `d3d9ex_getters_sanity`, `d3d9_get_state_roundtrip`, `d3d9ex_stateblock_sanity`,
  `d3d9ex_draw_indexed_primitive_up`, `d3d9ex_instancing_sanity`, `d3d9ex_scissor_sanity`, `d3d9ex_query_latency`, `d3d9ex_event_query`, `d3d9ex_submit_fence_stress`,
  `d3d9ex_stretchrect`, `d3d9_raster_status_sanity`, `d3d9ex_multiframe_triangle`, `d3d9ex_vb_dirty_range`,
  `d3d9_dynamic_vb_lock_semantics`, `d3d9ex_shared_surface`, `d3d9ex_shared_surface_ipc`, and the DWM-focused `d3d9ex_dwm_ddi_sanity` / `d3d9ex_dwm_probe`).
  - On Win7 x64, `d3d9ex_shared_surface_wow64` validates cross-bitness shared-surface interop (WOW64 producer → native consumer; DWM scenario).
  - For DWM-like multi-producer batching / alloc_id collision coverage, also run `d3d9ex_shared_surface_many_producers` and `d3d9ex_alloc_id_persistence`.
  - For MVP shared-surface allocation policy coverage (shared resources must be single-allocation), also run `d3d9ex_shared_allocations`.
  - For open/close churn coverage (repeated create → open → destroy; catches hangs/crashes), also run `d3d9ex_shared_surface_stress`.

#### Running host-side unit tests (portable)

These tests build and run on non-Windows hosts (no WDK required).

From repo root:

```bash
cmake -S drivers/aerogpu/umd/d3d9/tests -B build-d3d9-tests -G "Unix Makefiles"
cmake --build build-d3d9-tests -j
ctest --test-dir build-d3d9-tests --output-on-failure
```

Note: remember to re-run the build step after pulling new commits; `ctest` runs the previously-built binaries in the build directory.

## Call tracing (bring-up / debugging)

The D3D9 UMD contains a lightweight **in-process call trace** facility that can record D3D9UMDDI entrypoints (including stubs) and dump them via `OutputDebugStringA`/stderr.

See:

- `docs/graphics/win7-d3d9-umd-tracing.md`

Notes:

- Tracing is disabled by default; enable it by setting `AEROGPU_D3D9_TRACE=1` in the target process environment.
- On Windows, trace dumps are emitted via `OutputDebugStringA` by default (view with DebugView/WinDbg). If you want to capture trace output to a console/CI log, set `AEROGPU_D3D9_TRACE_STDERR=1` to also echo the output to `stderr`.
- `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1` dumps once when the trace sees a call whose name is explicitly tagged with `(stub)`. Host tests use the trace-only `TraceTestStub` entrypoint to exercise this behavior without mislabeling real DDIs.

## Crash-proof D3D9UMDDI vtables (Win7 runtime)

The Win7 D3D9 runtime (and `dwm.exe`) can call a wider set of DDIs than the initial AeroGPU bring-up implementation provides. The UMD **must never** return a partially-populated `D3D9DDI_DEVICEFUNCS` / `D3D9DDI_ADAPTERFUNCS` table where the runtime can call a **NULL** function pointer (that would crash the process before we can even trace the call).

In WDK builds (`AEROGPU_D3D9_USE_WDK_DDI=1`), the UMD populates every *known* function-table member with either a real implementation, a safe no-op, or a safe stub:

- Stubs log once (`aerogpu-d3d9: stub <name>`)
- Stubs emit a `D3d9TraceCall` record so trace dumps show which missing DDI was exercised
- Stubs return a stable HRESULT so callers fail cleanly instead of AV'ing
- The UMD validates at runtime that the returned `D3D9DDI_DEVICEFUNCS` / `D3D9DDI_ADAPTERFUNCS` tables contain no NULL entries; if any are found, `OpenAdapter*` / `CreateDevice` fails cleanly instead of handing the runtime a partially-populated vtable. The log includes the missing slot index/byte offset and (when possible) the `pfn*` member name.

### Currently stubbed DDIs

These DDIs are present in the Win7 D3D9UMDDI surface but are not implemented yet (they currently return `D3DERR_NOTAVAILABLE`):

*(none)*

### Patch rendering (Bezier / RT patches)

Patch rendering DDIs (`pfnDrawRectPatch` / `pfnDrawTriPatch` / `pfnDeletePatch`) are implemented for a bring-up subset
(see “Fixed-function vertex formats (FVF)” above); they are no longer treated as “stubs”.

Note: These DDIs correspond to D3D9 **RT patches** (`D3DDEVCAPS_RTPATCHES`) / Bezier patch rendering. They are distinct from
the D3D9 “N-patches” / Truform feature (`D3DDEVCAPS_NPATCHES`).

- `pfnDrawRectPatch` / `pfnDrawTriPatch` / `pfnDeletePatch`

Code anchors (all in `src/aerogpu_d3d9_driver.cpp`):

- `device_draw_rect_patch()` / `device_draw_tri_patch()` / `device_delete_patch()`
- CPU tessellation helpers (Bezier cubic): `tessellate_rect_patch_cubic()` / `tessellate_tri_patch_cubic()`

Limitations:

- Only the fixed-function fallback path is supported (no user shaders).
- Only pre-transformed `XYZRHW` control points are supported: patch entrypoints require `fixedfunc_fvf_supported(dev->fvf)`,
  i.e. `D3DFVF_XYZRHW | D3DFVF_DIFFUSE` (+ optional `D3DFVF_TEX1`).
- When `D3DFVF_TEX1` is used for patches, tessellation consumes `TEXCOORD0`:
  - `float1`: uses `.x` as `u` and treats `v = 0`
  - `float2/float3/float4`: uses `.xy` as `(u, v)` (extra components are ignored)
- Only Bezier cubic patches are supported (`Basis=BEZIER`, `Degree=CUBIC`).

### ProcessVertices

`pfnProcessVertices` is implemented and is **not** treated as a stub.

When the device is lost, `ProcessVertices` fails with `D3DERR_DEVICELOST` (or a more specific device-removed style HRESULT
when available).

Current behavior is intentionally bring-up level, with two paths:

- **Fixed-function CPU transform (small subset):** when **no user vertex shader** is bound (pixel shader binding does not
  affect `ProcessVertices`) and the current fixed-function hint
  (`dev->fvf`, set via `SetFVF` or inferred from `SetVertexDecl`) is one of:
  - `D3DFVF_XYZRHW`
  - `D3DFVF_XYZRHW | D3DFVF_DIFFUSE`
  - `D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1`
  - `D3DFVF_XYZRHW | D3DFVF_TEX1`
  - `D3DFVF_XYZW`
  - `D3DFVF_XYZW | D3DFVF_DIFFUSE`
  - `D3DFVF_XYZW | D3DFVF_DIFFUSE | D3DFVF_TEX1`
  - `D3DFVF_XYZW | D3DFVF_TEX1`
  - `D3DFVF_XYZ`
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE`
  - `D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1`
  - `D3DFVF_XYZ | D3DFVF_TEX1`
  the UMD reads vertices from **stream 0** and writes **screen-space `XYZRHW`** position into **stream 0** of the
  destination layout described by `hVertexDecl` (declaration elements in other streams are ignored):
  - Destination stride: uses `DestStride` when provided and non-zero; otherwise infers the effective destination stride
    from **stream 0** of `hVertexDecl`. If it cannot be inferred, the fixed-function CPU transform path fails with
    `D3DERR_INVALIDCALL`.
  - for `D3DFVF_XYZ*` / `D3DFVF_XYZW*` inputs: applies a CPU-side **World/View/Projection + viewport (x/y)** transform to
    produce `XYZRHW` (for `D3DFVF_XYZW*` the input `w` is respected; output `z` remains D3D9 NDC depth `0..1` and does not
    apply viewport `MinZ`/`MaxZ`), and
  - for `D3DFVF_XYZRHW*` inputs: passes through the source `XYZRHW` position as-is (already `POSITIONT` screen space).

  Unless `D3DPV_DONOTCOPYDATA` is set, the UMD clears the full destination vertex (`DestStride` bytes) before writing
  outputs so any destination decl elements that are not written (e.g. dst has TEX0 but src does not) become 0 and the
  output is deterministic.

  When the destination declaration includes `DIFFUSE`, the UMD copies it from the source when present, otherwise fills it
  with opaque white (matching fixed-function “no diffuse means white” behavior). `TEXCOORD0` is copied only when present
  in both the source and destination layouts (supports `FLOAT1/2/3/4`; source texcoord size is derived from
  `D3DFVF_TEXCOORDSIZE*` when set). Some D3D9 runtimes appear to synthesize destination decls where `TEXCOORD0` uses
  `Usage=POSITION` (`0`) rather than `D3DDECLUSAGE_TEXCOORD`; the UMD is intentionally permissive and accepts this.
  - Flags: when `D3DPV_DONOTCOPYDATA` is set in `ProcessVertices.Flags`, the UMD writes only the output position
    (`POSITIONT`) and preserves all other destination bytes (no DIFFUSE/TEX writes, no zeroing).
- **Fallback memcpy-style path:** for all other cases, `ProcessVertices` performs a conservative buffer-to-buffer copy from
  the active stream 0 vertex buffer into the destination buffer. The copy is stride-aware (copies
  `min(stream0_stride, dest_stride)` bytes per vertex) and uses the same “upload/dirty-range” notifications used by
  `Unlock`.
  - Destination stride: uses `DestStride` when provided and non-zero; otherwise it tries to infer it from **stream 0** of
    `hVertexDecl` when possible, falling back to the currently-bound stream 0 stride.
  - When `D3DPV_DONOTCOPYDATA` is set and the source is a pre-transformed `XYZRHW*` FVF, the memcpy fallback copies only the
    first 16 bytes (the `POSITIONT` float4) and preserves the remaining destination bytes.
  - In-place overlap safety: when the source and destination buffers alias the same resource and the strided ranges overlap
    (notably when `src_stride != dest_stride`), the implementation stages the source bytes before writing destinations to
    avoid self-overwrite and match “read all source first, then write all destinations” semantics.

Code anchors (all in `src/aerogpu_d3d9_driver.cpp`):

- `device_process_vertices()` (DDI entrypoint / dispatcher)
- `device_process_vertices_internal()` + `parse_process_vertices_dest_decl()` (fixed-function CPU transform subset)

Limitations:

- Only buffer resources are supported (source VB and destination must both be `ResourceKind::Buffer`).
- Stream 0 only:
  - source: additional vertex streams are ignored (matching the D3D9 `ProcessVertices` contract),
  - destination: only stream 0 of the output vertex declaration is written/used for stride inference.
- No shader execution: neither the fixed-function CPU transform path nor the memcpy fallback executes user vertex shaders
  (or fixed-function lighting/material). When outside the supported fixed-function subset, the implementation is a
  byte-copy, not vertex processing.
- The fixed-function CPU transform path is limited to the fixed-function FVF subset listed above and requires that the
  destination declaration contain a writable float4 position (`POSITIONT`/`POSITION`) for the `XYZRHW` output.

### Bring-up no-op DDIs

These DDIs are treated as benign no-ops for bring-up (returning `S_OK`). They are still traced, but are **not** tagged as
`(stub)` in trace output (so they do not trigger `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1`).

- `pfnSetConvolutionMonoKernel`
- `pfnSetDialogBoxMode`
- `pfnComposeRects` (`device_compose_rects()` in `src/aerogpu_d3d9_driver.cpp`)

Note: `pfnSetConvolutionMonoKernel` and `pfnSetDialogBoxMode` are wired via `AEROGPU_D3D9_DEFINE_DDI_NOOP(...)` in the
“Stubbed entrypoints” section of `src/aerogpu_d3d9_driver.cpp`. `pfnComposeRects` is implemented directly as an `S_OK`
no-op to keep D3D9Ex composition paths alive.

### Cached legacy state (Set*/Get* round-trip)

Several fixed-function/resource state paths are cached for deterministic `Get*` queries and state-block compatibility.
Some cached values are also consumed by fixed-function emulation/pipeline selection or draw-time emulation (for example:
stages 0..3 `D3DTSS_*` influence fixed-function pixel shader selection, cached transforms feed fixed-function WVP paths, and
`SetStreamSourceFreq` drives CPU-expanded instancing), but most are still cached-only and are not forwarded 1:1 into the
AeroGPU command stream. This includes:

- texture stage state (D3DTSS_*)
- transforms / clip planes / N-patch mode
- stream source frequency (instancing; CPU expansion subset)
- software vertex processing
- lighting/material
- palettes / clip status / gamma ramp
- resource priority
- autogen filter type

These cached values participate in D3D9 state blocks:

- `BeginStateBlock`/`EndStateBlock` records them when the corresponding `Set*` calls are made.
- `CreateStateBlock` snapshots the current cached values when the state block is created.
- `CaptureStateBlock` refreshes them from the current device state.
- `ApplyStateBlock` restores them (updating the UMD-side caches so `Get*` reflects the applied state).

`ValidateDevice` is implemented and reports a conservative `NumPasses = 1` for the supported shader pipeline (validated by
`d3d9_validate_device_sanity`).

### Caps/feature gating

Some bring-up entrypoints correspond primarily to **fixed-function** and legacy code paths. Keep the reported D3D9 caps conservative so the runtime and apps prefer the shader/VB/IB paths that the UMD does implement (while still enabling the fixed-function subset above for DWM/legacy apps).

In particular:

- **Patch caps**: keep N-patch/patch caps conservative. Patch rendering entrypoints may still exist, but the caps are
  intentionally conservative so the runtime/apps prefer the core triangle/VB/IB paths.
  - `D3DDEVCAPS_RTPATCHES` is advertised for the supported cubic Bezier patch subset (see “Patch rendering”), with a finite
    `MaxNpatchTessellationLevel` (currently 64.0) to avoid unbounded tessellation requests.
  - `D3DDEVCAPS_NPATCHES` and `D3DDEVCAPS_QUINTICRTPATCHES` are not advertised.
- **Format caps**: BC/DXT formats are only advertised when the device ABI minor version indicates the
  guest↔host protocol understands them (see `aerogpu_d3d9_caps.cpp` / `supports_bc_formats()`).
- **FVF caps**: keep `D3DCAPS9.FVFCaps` conservative (advertise only 1 texture coordinate set / `TEX1`) so fixed-function
  apps don't assume multi-texcoord fixed-function coverage. The UMD can translate additional FVFs into input layouts for
  user shaders (see “FVF-derived input layouts for user shaders” above), but this is not fully advertised in caps.
- **TextureOpCaps**: `D3DCAPS9.TextureOpCaps` advertises the exact fixed-function combiner ops implemented by the
  fixed-function fallback path (see `aerogpu_d3d9_caps.cpp` and “Limitations (bring-up)” below).
