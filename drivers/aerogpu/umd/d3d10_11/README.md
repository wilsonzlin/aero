# AeroGPU Windows 7 D3D10/11 User-Mode Driver (UMD)

This directory contains the **Direct3D 10 / Direct3D 11 Windows 7 SP1 user-mode driver** for AeroGPU.

The UMD is responsible for translating the D3D10DDI/D3D11DDI calls made by the D3D runtime into the **AeroGPU command stream** defined in `drivers/aerogpu/protocol/`.

This UMD targets only the **command stream** ABI (`drivers/aerogpu/protocol/aerogpu_cmd.h`). The kernel-mode driver (KMD) owns the submission transport and supports both the versioned (`aerogpu_pci.h` + `aerogpu_ring.h`) and legacy (`aerogpu_protocol.h`) device ABIs, auto-detected via MMIO magic; see `drivers/aerogpu/kmd/README.md`.

## Status / scope (initial)

This implementation is intentionally conservative and targets **the minimum functionality needed for a basic D3D11 triangle**:

- Device + immediate context
- Buffers and 2D textures
- Vertex/pixel shaders (DXBC payload passthrough)
- Input layout + vertex/index buffers
- RTV + clear + draw/draw-indexed
- Blend/raster/depth state objects (accepted; currently conservative/stubbed)
- Windowed swapchain present (sync interval 0/1) via `AEROGPU_CMD_PRESENT`

Unsupported functionality must fail cleanly (returning `E_NOTIMPL` / `E_INVALIDARG`) rather than crashing or dereferencing null DDI function pointers.

For a full “bring-up spec” (Win7 driver model overview, minimal D3D10DDI/D3D11DDI entrypoints to implement, swapchain behavior expectations, shader handling, and a test plan), see:

- `docs/graphics/win7-d3d10-11-umd-minimal.md`
- `docs/graphics/win7-d3d11ddi-function-tables.md` (DDI function-table checklist: REQUIRED vs stub for FL10_0)

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

The core emission happens in `src/aerogpu_d3d10_11_umd.cpp` by building a linear command buffer consisting of:

```
[aerogpu_cmd_stream_header]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  ...
```

### Command stream writer

Command serialization uses the shared D3D9 implementation:

- `../d3d9/src/aerogpu_cmd_stream_writer.h`

This provides both a `std::vector`-backed stream (portable bring-up/tests) and a span/DMA-backed stream (`{uint8_t* buf, size_t capacity}`) suitable for writing directly into a WDDM runtime-provided command buffer.

### Shared surface note

DXGI/D3D10/11 shared resource interop is not implemented in this UMD yet. The protocol supports it (primarily for D3D9Ex/DWM) via `AEROGPU_CMD_EXPORT_SHARED_SURFACE` / `AEROGPU_CMD_IMPORT_SHARED_SURFACE` and a stable cross-process `share_token` provided by the KMD via preserved WDDM allocation private data (`aerogpu_wddm_alloc_priv`; see `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`). The recommended scheme is `share_token = (uint64_t)alloc_id` (it is **not** a process-local `HANDLE` value).

## Build

This code is intended to be built as a **DLL UMD** for Windows 7 SP1.

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

Recommended build entrypoint (MSBuild/WDK10):

```cmd
cd \path\to\repo
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
```

CI builds the same solution (and stages outputs under `out/drivers/aerogpu/`) via `ci/build-drivers.ps1`.

Optional: `drivers\aerogpu\build\build_all.cmd` is a convenience wrapper around MSBuild/WDK10 that stages outputs under `drivers\aerogpu\build\out\win7\...`.

The project expects the Windows SDK/WDK to provide D3D10/11 DDI headers (e.g. `d3d10umddi.h`, `d3d11umddi.h`) when building the real UMD.  

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
```

Collect the output using one of:

- **DebugView** (Sysinternals): run DebugView and enable *Capture Win32*.
- **WinDbg**: attach to the process and watch the debug output stream.
