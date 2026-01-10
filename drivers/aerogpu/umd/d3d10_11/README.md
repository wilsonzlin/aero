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
- Windowed swapchain present (sync interval 0/1) via `AEROGPU_CMD_PRESENT`

Unsupported functionality must fail cleanly (returning `E_NOTIMPL` / `E_INVALIDARG`) rather than crashing or dereferencing null DDI function pointers.

## Feature level strategy

The initial feature claim is **D3D_FEATURE_LEVEL_10_0**:

- D3D11 runtime compatibility (can create a D3D11 device at FL10_0)
- Avoids implementing SM5.0-only features (tessellation/CS/etc.) early
- Matches the minimal pipeline required for a triangle sample

## Command stream mapping

All resources (buffers, textures, views) are referenced in the command stream using a monotonically increasing 32-bit **allocation index** (`alloc_index`). This mirrors the intended D3D9 UMD strategy and avoids KMD patching of pointers in submitted command buffers.

The core emission happens in `src/aerogpu_d3d10_11_umd.cpp` by building a linear command buffer consisting of:

```
[AEROGPU_CMD_HEADER][payload...][AEROGPU_CMD_HEADER][payload...]...
```

## Build

This code is intended to be built as a **DLL UMD** for Windows 7 SP1.

Build files provided:

- `aerogpu_d3d10_11.sln`
- `aerogpu_d3d10_11.vcxproj` (Win32 + x64)

The project expects the Windows SDK/WDK to provide D3D10/11 DDI headers (e.g. `d3d10umddi.h`, `d3d11umddi.h`) when building the real UMD.

