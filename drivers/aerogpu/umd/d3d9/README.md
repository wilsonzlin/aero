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
  - For **shared** allocations, dxgkrnl preserves and replays the private-data blob on `OpenResource`/`OpenAllocation` so all guest processes observe consistent IDs.
  - `backing_alloc_id = 0` means “host allocated” (no guest backing). Portable/non-WDDM builds typically use host-allocated resources and leave `backing_alloc_id = 0`. In Win7/WDDM builds, most default-pool resources are backed by WDDM allocations and use non-zero `alloc_id` values so the KMD can build a per-submit `alloc_id → GPA` table for the emulator.

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
**not** use the numeric value of the D3D shared `HANDLE` as `share_token`: handle
values are process-local and not stable cross-process.

Canonical contract: on Win7/WDDM 1.1, the Win7 KMD generates a stable non-zero 64-bit
`share_token` and persists it in the preserved WDDM allocation private driver data blob
(`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`),
which dxgkrnl returns verbatim on cross-process opens.

For shared allocations, `alloc_id` must avoid collisions across guest processes and must stay in the UMD-owned range (`alloc_id <= 0x7fffffff`). In the current AeroGPU D3D9 UMD:

- `alloc_id` is derived from a cross-process monotonic counter (`allocate_shared_alloc_id_token()` in `src/aerogpu_d3d9_driver.cpp`, backed by a named file mapping + `InterlockedIncrement64`, masked to 31 bits with 0 skipped).
- `share_token` is returned by the KMD via `aerogpu_wddm_alloc_priv.share_token` (filled during `DxgkDdiCreateAllocation` and preserved across cross-process opens).

See `docs/graphics/win7-shared-surfaces-share-token.md` for the end-to-end contract and the cross-process validation test.

#### Cross-API note: D3D9Ex consuming DXGI shared handles (DWM scenario)

On Windows 7, the desktop compositor (`dwm.exe`, D3D9Ex) commonly consumes **DXGI shared handles**
produced by D3D10/D3D11 apps. In this case, the preserved WDDM allocation private data blob is
typically `aerogpu_wddm_alloc_priv_v2` and the `reserved0` field may carry a **pitch encoding**
(rather than a D3D9 `format/width/height` descriptor marker).

To support this DWM-style path, the AeroGPU D3D9 UMD `OpenResource` implementation falls back to
the v2 metadata (`width/height/DXGI format/row_pitch_bytes`) and maps a small set of common DXGI
formats (BGRA/RGBA) to their nearest D3D9 `D3DFORMAT` values, so `Lock` can report the correct
`RowPitch` and CPU-side helpers can compute a consistent surface layout.

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
  - `src/aerogpu_d3d9_wdk_abi_asserts.h` (included automatically; inert unless `AEROGPU_D3D9_USE_WDK_DDI=1`)

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

The initial implementation focuses on the minimum D3D9Ex feature set needed for:

- DWM/Aero composition
- a basic triangle app (VB/IB, shaders, textures, alpha blend, present)

Unsupported states are handled defensively; unknown state enums are accepted and forwarded as generic “set render/sampler state” commands so the emulator can decide how to interpret them.

## Call tracing (bring-up / debugging)

The D3D9 UMD contains a lightweight **in-process call trace** facility that can record D3D9UMDDI entrypoints (including stubs) and dump them via `OutputDebugStringA`/stderr.

See:

- `docs/graphics/win7-d3d9-umd-tracing.md`

Notes:

- On Windows, trace dumps are emitted via `OutputDebugStringA` by default (view with DebugView/WinDbg). If you want to capture trace output to a console/CI log, set `AEROGPU_D3D9_TRACE_STDERR=1` to also echo the output to `stderr`.
- `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1` is useful for early bring-up: it dumps once on the first stubbed DDI hit (e.g. unsupported patch rendering).

## Crash-proof D3D9UMDDI vtables (Win7 runtime)

The Win7 D3D9 runtime (and `dwm.exe`) can call a wider set of DDIs than the initial AeroGPU bring-up implementation provides. The UMD **must never** return a partially-populated `D3D9DDI_DEVICEFUNCS` / `D3D9DDI_ADAPTERFUNCS` table where the runtime can call a **NULL** function pointer (that would crash the process before we can even trace the call).

In WDK builds (`AEROGPU_D3D9_USE_WDK_DDI=1`), the UMD populates every *known* function-table member with either a real implementation, a safe no-op, or a safe stub:

- Stubs log once (`aerogpu-d3d9: stub <name>`)
- Stubs emit a `D3d9TraceCall` record so trace dumps show which missing DDI was exercised
- Stubs return a stable HRESULT so callers fail cleanly instead of AV'ing
- The UMD validates at runtime that the returned `D3D9DDI_DEVICEFUNCS` / `D3D9DDI_ADAPTERFUNCS` tables contain no NULL entries; if any are found, `OpenAdapter*` / `CreateDevice` fails cleanly instead of handing the runtime a partially-populated vtable. The log includes the missing slot index/byte offset and (when possible) the `pfn*` member name.

### Currently stubbed DDIs

These DDIs are present in the Win7 D3D9UMDDI surface but are not implemented yet (they currently return `D3DERR_NOTAVAILABLE`):

- `pfnDrawRectPatch` / `pfnDrawTriPatch` / `pfnDeletePatch` / `pfnProcessVertices`

### Bring-up no-op DDIs

These DDIs are treated as benign no-ops for bring-up (returning `S_OK`). They are still traced, but are **not** tagged as `(stub)` in trace output (so they do not trigger `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1`):

- `pfnSetConvolutionMonoKernel`
- `pfnGenerateMipSubLevels`
- `pfnSetCursorProperties` / `pfnSetCursorPosition` / `pfnShowCursor`
- `pfnSetDialogBoxMode`

### Cached legacy state (Set*/Get* round-trip)

Several fixed-function/resource state paths are cached for deterministic `Get*` queries and state-block compatibility, but are not currently emitted to the AeroGPU command stream. This includes:

- texture stage state (D3DTSS_*)
- transforms / clip planes / N-patch mode
- stream source frequency (instancing) / software vertex processing
- shader int/bool constants
- lighting/material
- palettes / clip status / gamma ramp
- resource priority
- autogen filter type

These cached values participate in D3D9 state blocks:

- `BeginStateBlock`/`EndStateBlock` records them when the corresponding `Set*` calls are made.
- `CreateStateBlock` snapshots the current cached values when the state block is created.
- `CaptureStateBlock` refreshes them from the current device state.
- `ApplyStateBlock` restores them (updating the UMD-side caches so `Get*` reflects the applied state).

### Caps/feature gating

The stubbed entrypoints above correspond primarily to **fixed-function** and legacy code paths. Until real implementations exist, keep the reported D3D9 caps conservative so the runtime and apps prefer the shader/VB/IB paths that the UMD does implement.
