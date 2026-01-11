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
  - `alloc_id` is **UMD-owned** and is stored in WDDM allocation private driver data (`aerogpu_wddm_alloc_priv` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
  - The KMD validates it and uses it to build the per-submit `aerogpu_alloc_table` (`drivers/aerogpu/protocol/aerogpu_ring.h`) mapping `alloc_id → {gpa, size_bytes, flags}` for the emulator. `backing_alloc_id` in packets is the `alloc_id` lookup key (not an index into an allocation list).
  - For **shared** allocations, `alloc_id` should avoid collisions across guest processes: DWM may open and compose many redirected surfaces from different processes in a single submission, and the per-submit allocation table is keyed by `alloc_id`.
  - `backing_alloc_id = 0` means “host allocated” (no guest backing). The current bring-up UMD uses host-allocated resources and typically sets `backing_alloc_id = 0`.

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

`share_token` must be stable across guest processes. On Win7/WDDM 1.1, the token is
persisted in WDDM allocation private driver data (`aerogpu_wddm_alloc_priv.share_token`
in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`): the UMD chooses it at creation
time and dxgkrnl preserves the bytes for shared allocations and returns them
verbatim on `OpenResource` so another process can observe the same token.

Do **not** use the numeric value of the D3D shared `HANDLE` as `share_token`: handle values are process-local and not stable cross-process.

For shared allocations, `alloc_id` must avoid collisions across guest processes and must stay in the UMD-owned range (`alloc_id <= 0x7fffffff`). In the current AeroGPU D3D9 UMD:

- `alloc_id` is derived from a cross-process monotonic counter (`allocate_shared_alloc_id_token()` in `src/aerogpu_d3d9_driver.cpp`, backed by a named file mapping + `InterlockedIncrement64`, masked to 31 bits with 0 skipped).
- `share_token` is generated independently via `ShareTokenAllocator::allocate_share_token()` (`src/aerogpu_d3d9_shared_resource.h`, crypto RNG preferred with a fallback entropy + SplitMix64 path).

See `docs/graphics/win7-shared-surfaces-share-token.md` for the end-to-end contract and the cross-process validation test.

## Build

This project is intended to be built in a Windows/WDK environment as a DLL for both x86 and x64:

- `aerogpu_d3d9.dll` (x86 / Win32)
- `aerogpu_d3d9_x64.dll` (x64)

Build files:

- Visual Studio project: `aerogpu_d3d9_umd.vcxproj`
- Exports:
  - `aerogpu_d3d9_x86.def` (exports `OpenAdapter*` entrypoints from stdcall-decorated x86 symbols)
  - `aerogpu_d3d9_x64.def` (exports `OpenAdapter*` entrypoints)

Recommended build entrypoint (MSBuild/WDK10):

```cmd
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
```

CI builds the same solution (and stages outputs under `out/drivers/aerogpu/`) via `ci/build-drivers.ps1`.

Optional: `drivers\aerogpu\build\build_all.cmd` is a convenience wrapper around MSBuild/WDK10 that stages outputs under `drivers\aerogpu\build\out\win7\...`.

### Notes

- The code in `include/aerogpu_d3d9_umd.h` includes a tiny “compat” subset of the D3D9 DDI types so the core translation code is self-contained in this repository. When integrating into a real Win7 WDK build, wire the entrypoints to the real WDK D3D9 DDI headers and structures (the exported names are the key ABI contract).
- For ABI verification against the real Win7 D3D9 UMD headers (struct sizes/offsets + x86 stdcall export decoration), see `tools/wdk_abi_probe/`.
- Logging is done via `OutputDebugStringA` (view with DebugView/WinDbg) and is intentionally lightweight.

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

## Crash-proof D3D9UMDDI vtables (Win7 runtime)

The Win7 D3D9 runtime (and `dwm.exe`) can call a wider set of DDIs than the initial AeroGPU bring-up implementation provides. The UMD **must never** return a partially-populated `D3D9DDI_DEVICEFUNCS` / `D3D9DDI_ADAPTERFUNCS` table where the runtime can call a **NULL** function pointer (that would crash the process before we can even trace the call).

In WDK builds (`AEROGPU_D3D9_USE_WDK_DDI`), the UMD populates every *known* function-table member with either a real implementation or a safe stub:

- Stubs log once (`aerogpu-d3d9: stub <name>`)
- Stubs emit a `D3d9TraceCall` record so trace dumps show which missing DDI was exercised
- Stubs return a stable HRESULT so callers fail cleanly instead of AV'ing

### Currently stubbed DDIs

These DDIs are present in the Win7 D3D9UMDDI surface but are not implemented yet:

- `pfnSetTextureStageState` (no-op, returns `S_OK`)
- `pfnSetTransform` / `pfnMultiplyTransform` / `pfnSetClipPlane` (no-op, returns `S_OK`)
- `pfnSetShaderConstI` / `pfnSetShaderConstB` (no-op, returns `S_OK`)
- `pfnSetMaterial` / `pfnSetLight` / `pfnLightEnable` (no-op, returns `S_OK`)
- `pfnSetNPatchMode` / `pfnSetStreamSourceFreq` / `pfnSetGammaRamp` (no-op, returns `S_OK`)
- `pfnCreateStateBlock` / `pfnDeleteStateBlock` / `pfnCaptureStateBlock` / `pfnApplyStateBlock` / `pfnValidateDevice`
  (returns `D3DERR_NOTAVAILABLE`)
- `pfnSetSoftwareVertexProcessing`, `pfnSetCursorProperties` / `pfnSetCursorPosition` / `pfnShowCursor`,
  `pfnSetPaletteEntries` / `pfnSetCurrentTexturePalette`, `pfnSetClipStatus` (no-op, returns `S_OK`)
- `pfnGetClipStatus` / `pfnGetGammaRamp` (returns `D3DERR_NOTAVAILABLE`)
- `pfnDrawRectPatch` / `pfnDrawTriPatch` / `pfnDeletePatch` / `pfnProcessVertices` / `pfnGetRasterStatus`
  (returns `D3DERR_NOTAVAILABLE`)
- `pfnSetDialogBoxMode` (no-op, returns `S_OK`)
- `pfnDrawIndexedPrimitiveUP` (returns `D3DERR_NOTAVAILABLE`)
- `pfnGetSoftwareVertexProcessing`, `pfnGetTransform`, `pfnGetClipPlane`, `pfnGetViewport`, `pfnGetScissorRect`
  (returns `D3DERR_NOTAVAILABLE`)
- `pfnBeginStateBlock` / `pfnEndStateBlock`, `pfnGetMaterial`, `pfnGetLight` / `pfnGetLightEnable`,
  `pfnGetRenderTarget` / `pfnGetDepthStencil`, `pfnGetTexture`, `pfnGetTextureStageState`, `pfnGetSamplerState`,
  `pfnGetRenderState`, `pfnGetPaletteEntries` / `pfnGetCurrentTexturePalette`, `pfnGetNPatchMode`,
  `pfnGetFVF` / `pfnGetVertexDecl` (returns `D3DERR_NOTAVAILABLE`)
- `pfnGetStreamSource` / `pfnGetStreamSourceFreq`, `pfnGetIndices`, `pfnGetShader`, `pfnGetShaderConstF/I/B`
  (returns `D3DERR_NOTAVAILABLE`)

### Caps/feature gating

The stubbed entrypoints above correspond primarily to **fixed-function** and legacy code paths. Until real implementations exist, keep the reported D3D9 caps conservative so the runtime and apps prefer the shader/VB/IB paths that the UMD does implement.
