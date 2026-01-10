# AeroGPU Windows 7 Guest-Side Direct3D Validation Suite

This directory contains small Windows 7 **guest-side** test programs intended to validate the AeroGPU WDDM driver stack end-to-end.

Each test prints a clear `PASS:` / `FAIL:` line to stdout and returns a non-zero exit code on failure. Some tests can optionally dump a `.bmp` to disk for manual inspection (`--dump`).

The suite also includes an optional `aerogpu_timeout_runner.exe` helper (built by default) used by `run_all.cmd` to enforce a per-test timeout. Override the default timeout by setting `AEROGPU_TEST_TIMEOUT_MS` in the environment.

Common flags:

* `--dump` – write a `*.bmp` next to the executable.
* `--hidden` – for the windowed triangle tests: create the window but do not show it (useful for automation).
* `--require-vid=0x####` / `--require-did=0x####` – fail the test if the active adapter VID/DID does not match.
* `--allow-microsoft` – allow running on the Microsoft Basic Render Driver (normally treated as a failure to avoid false PASS when AeroGPU isn’t active).
* `--allow-non-aerogpu` – allow running on adapters whose description does not contain `AeroGPU` (by default, rendering tests expect to be running on an AeroGPU adapter).
* `--allow-remote` – for `d3d9ex_dwm_probe` only: skip the composition check when running under RDP (`SM_REMOTESESSION=1`).
* `--help` / `/?` – print per-test usage.

## Layout

```
drivers/aerogpu/tests/win7/
  build_all_vs2010.cmd
  run_all.cmd
  d3d9ex_triangle/
  d3d9ex_dwm_probe/
  dwm_flush_pacing/
  d3d11_triangle/
  readback_sanity/
  timeout_runner/
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

For suite usage:

```cmd
run_all.cmd --help
```

To increase the per-test timeout (default: 30000 ms):

```cmd
set AEROGPU_TEST_TIMEOUT_MS=120000
run_all.cmd
```

To require a specific PCI VID/DID (recommended for automation):

```cmd
run_all.cmd --require-vid=0x1234 --require-did=0x1111
```

You can find the correct VID/DID in the Win7 guest via:

* Device Manager → Display adapters → Properties → Details → **Hardware Ids**
* Or by reading the PCI ID from the AeroGPU driver INF you installed (see `drivers/aerogpu/packaging/win7/README.md`).

## Expected results

In a Win7 VM with AeroGPU installed and working correctly:

* `d3d9ex_dwm_probe` reports composition enabled (or successfully enables it)
* `dwm_flush_pacing` measures `DwmFlush()` pacing and fails on extremely fast returns (not vsync paced) or very large gaps
* `d3d9ex_triangle` renders a green triangle over a red clear and confirms **corner red + center green** via readback
* `d3d11_triangle` renders a green triangle over a red clear and confirms **corner red + center green** via readback
* `readback_sanity` renders to an offscreen render target and validates readback pixels (corner red, center green)

All rendering tests also print the active adapter description + VendorId/DeviceId to help confirm the expected GPU/driver is being exercised.
