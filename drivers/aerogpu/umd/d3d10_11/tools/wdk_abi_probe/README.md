# Win7 D3D10/11 UMD ABI Probe (WDK headers)

This directory contains a **standalone console program** intended to be built against the **real Win7-era D3D10/D3D11 UMD DDI headers** (`d3d10umddi.h`, `d3d10_1umddi.h`, `d3d11umddi.h`, `d3dumddi.h`, `d3dkmthk.h`) and prints ABI-critical information:

- `sizeof(...)` / selected `offsetof(...)` values for key DDI structs.
- The **expected x86 stdcall-decorated export names** for the UMD entrypoints:
  - `_OpenAdapter10@N`
  - `_OpenAdapter10_2@N`
  - `_OpenAdapter11@N`

The goal is to make header/version drift and calling-convention mismatches obvious *before* debugging a Win7 loader crash.

> This is tooling-only and is **not** part of the normal AeroGPU build.

## Build (WDK environment)

1. Install a WDK that provides the D3D10/11 UMD DDI headers (WDK 7.1 / WinDDK layout is known to work).
2. Open the appropriate build environment command prompt (so headers/libs are on `INCLUDE`/`LIB`):
   - “Windows 7 x86 Free Build Environment” (x86)
   - “Windows 7 x64 Free Build Environment” (x64)
3. Build and run:

```cmd
cd \path\to\repo\drivers\aerogpu\umd\d3d10_11\tools\wdk_abi_probe

rem x86
cl /nologo /W4 /EHsc d3d10_11_wdk_abi_probe.cpp /Fe:d3d10_11_wdk_abi_probe_x86.exe
d3d10_11_wdk_abi_probe_x86.exe

rem x64 (from the x64 build environment)
cl /nologo /W4 /EHsc d3d10_11_wdk_abi_probe.cpp /Fe:d3d10_11_wdk_abi_probe_x64.exe
d3d10_11_wdk_abi_probe_x64.exe
```

## What to check (strict)

### 1) Export decoration / `.def` stack sizes (x86)

Run the x86 probe and confirm that the reported `_OpenAdapter*@N` values match:

- `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_x86.def`

### 2) Verify UMD exports with `dumpbin`

From a Visual Studio Developer Command Prompt:

```cmd
dumpbin /exports aerogpu_d3d10.dll
dumpbin /exports aerogpu_d3d10_x64.dll
```

Confirm the export table contains **undecorated**:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

And on x86, also contains the raw decorated forms:

- `_OpenAdapter10@N`
- `_OpenAdapter10_2@N`
- `_OpenAdapter11@N`

