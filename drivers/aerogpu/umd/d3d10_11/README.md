# AeroGPU Windows 7 D3D10/11 User-Mode Driver (UMD)

This directory contains the **Direct3D 10 / Direct3D 11 Windows 7 SP1 user-mode driver** for AeroGPU.

The UMD is responsible for translating the D3D10DDI/D3D11DDI calls made by the D3D runtime into the **AeroGPU command stream** defined in `drivers/aerogpu/protocol/`.

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

## Feature level strategy

The initial feature claim is **D3D_FEATURE_LEVEL_10_0**:

- D3D11 runtime compatibility (can create a D3D11 device at FL10_0)
- Avoids implementing SM5.0-only features (tessellation/CS/etc.) early
- Matches the minimal pipeline required for a triangle sample

## Command stream mapping

This UMD emits `drivers/aerogpu/protocol/aerogpu_cmd.h` packets and references objects using **protocol resource handles** (`aerogpu_handle_t`), not an “allocation list index” model:

- Packets reference resources via `resource_handle` / `buffer_handle` / `texture_handle` fields.
- When a resource is backed by guest memory, create packets may set `backing_alloc_id` (and `backing_offset_bytes`). `backing_alloc_id` is resolved via the optional per-submission `aerogpu_alloc_table` supplied by the KMD in `aerogpu_submit_desc` (see `drivers/aerogpu/protocol/aerogpu_ring.h`).
- `aerogpu_handle_t` values are protocol object IDs; they are intentionally **not** WDDM allocation handles/IDs.

The core emission happens in `src/aerogpu_d3d10_11_umd.cpp` by building a linear command buffer consisting of:

```
[aerogpu_cmd_stream_header]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  [aerogpu_cmd_hdr + packet fields (+ payload...)]
  ...
```

### Shared surface note

DXGI/D3D10/11 shared resource interop is not implemented in this UMD yet. The protocol supports it (primarily for D3D9Ex/DWM) via `AEROGPU_CMD_EXPORT_SHARED_SURFACE` / `AEROGPU_CMD_IMPORT_SHARED_SURFACE` and a stable cross-process `share_token`.

## Build

This code is intended to be built as a **DLL UMD** for Windows 7 SP1.

Build files provided:

- `aerogpu_d3d10_11.sln`
- `aerogpu_d3d10_11.vcxproj` (Win32 + x64)

The project is configured to output binaries that match the Win7 packaging INF:

- Win32: `aerogpu_d3d10.dll`
- x64: `aerogpu_d3d10_x64.dll`

Recommended build entrypoint (builds KMD + required D3D9 UMD + optional D3D10/11 UMD and stages outputs under `drivers/aerogpu/build/out/`):

```cmd
cd \path\to\repo
drivers\aerogpu\build\build_all.cmd fre
```

The project expects the Windows SDK/WDK to provide D3D10/11 DDI headers (e.g. `d3d10umddi.h`, `d3d11umddi.h`) when building the real UMD.  
