# AeroGPU Windows 7 Guest-Side Direct3D Validation Suite

This directory contains small Windows 7 **guest-side** test programs intended to validate the AeroGPU WDDM driver stack end-to-end.

Each test prints a clear `PASS:` / `FAIL:` line to stdout and returns a non-zero exit code on failure. Some tests can optionally dump a `.bmp` to disk for manual inspection (`--dump`).

## Layout

```
drivers/aerogpu/tests/win7/
  build_all_vs2010.cmd
  run_all.cmd
  d3d9ex_triangle/
  d3d9ex_dwm_probe/
  d3d11_triangle/
  readback_sanity/
  common/
```

## Prerequisites (Windows 7 guest)

### Runtime

* Windows 7 SP1 (x86 or x64)
* AeroGPU driver installed (KMD + UMDs)

### Build toolchain

The recommended build path is **Visual Studio 2010** (or the VS2010 toolchain) using `cl.exe`.

* Visual Studio 2010 (or “Visual C++ 2010 Express” + Windows SDK 7.1)
* **DirectX SDK (June 2010)** (recommended) – provides `fxc.exe` needed to compile the D3D11 shaders.
  * Ensure `fxc.exe` is on `PATH` (e.g. add `%DXSDK_DIR%Utilities\bin\x86`).

> Note: The D3D11 tests do **not** compile shaders at runtime. Shaders are compiled by `fxc.exe` during the build and written as `.cso` next to the `.exe`.

## Build (VS2010 command prompt)

Open the appropriate “Visual Studio 2010 Command Prompt” for your guest architecture and run:

```cmd
cd \path\to\repo\drivers\aerogpu\tests\win7
build_all_vs2010.cmd
```

Outputs are placed in:

```
drivers\aerogpu\tests\win7\bin\
```

## Run

From the same directory:

```cmd
run_all.cmd
```

To also write BMP dumps next to the binaries:

```cmd
run_all.cmd --dump
```

## Expected results

In a Win7 VM with AeroGPU installed and working correctly:

* `d3d9ex_dwm_probe` reports composition enabled (or successfully enables it)
* `d3d9ex_triangle` renders a green triangle over a red clear and confirms the center pixel is green
* `d3d11_triangle` renders a green triangle over a red clear and confirms the center pixel is green
* `readback_sanity` renders to an offscreen render target and validates readback pixels (corner red, center green)

