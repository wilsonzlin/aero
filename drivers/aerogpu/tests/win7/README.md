# AeroGPU Windows 7 Guest-Side Direct3D Validation Suite

This directory contains small Windows 7 **guest-side** test programs intended to validate the AeroGPU WDDM driver stack end-to-end.

Each test prints a clear `PASS:` / `FAIL:` line to stdout and returns a non-zero exit code on failure. Some tests can optionally dump a `.bmp` to disk for manual inspection (`--dump`).

The suite also includes an optional `aerogpu_timeout_runner.exe` helper (built by default) used by `run_all.cmd` to enforce a per-test timeout. Override the default timeout by setting `AEROGPU_TEST_TIMEOUT_MS` in the environment.

Common flags:

 * `--dump` – write a `*.bmp` next to the executable.
 * `--hidden` – for the windowed triangle tests: create the window but do not show it (useful for automation).
 * `--validate-sharing` – for `d3d9ex_shared_surface`: also validate cross-process pixel sharing via readback (implied by `--dump`).
 * `--samples=N` – control sample count for pacing/sampling tests (defaults vary per test).
 * `--wait-timeout-ms=N` – for `wait_vblank_pacing` only: per-wait timeout for `D3DKMTWaitForVerticalBlankEvent` (default 2000).
 * `--require-vid=0x####` / `--require-did=0x####` – fail the test if the active adapter VID/DID does not match.
 * `--allow-microsoft` – allow running on the Microsoft Basic Render Driver (normally treated as a failure to avoid false PASS when AeroGPU isn’t active).
 * `--allow-non-aerogpu` – allow running on adapters whose description does not contain `AeroGPU` (by default, rendering tests expect to be running on an AeroGPU adapter).
 * `--allow-remote` – skip tests that are not meaningful under RDP (`SM_REMOTESESSION=1`): `d3d9ex_dwm_probe`, `dwm_flush_pacing`, `wait_vblank_pacing`, `vblank_wait_pacing`, `vblank_wait_sanity`, `get_scanline_sanity`, `d3d9_raster_status_sanity`, `d3d9_raster_status_pacing`.
 * `--help` / `/?` – print per-test usage.

## Layout

```
drivers/aerogpu/tests/win7/
  build_all_vs2010.cmd
  run_all.cmd
  d3d9ex_triangle/
  d3d9ex_event_query/
  d3d9ex_query_latency/
   d3d9ex_shared_surface/
   d3d9ex_shared_allocations/
   d3d9ex_dwm_probe/
   d3d9ex_event_query/
   wait_vblank_pacing/
   vblank_wait_sanity/
   vblank_wait_pacing/
  get_scanline_sanity/
  d3d9_raster_status_sanity/
  d3d9_raster_status_pacing/
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

Or (equivalently) pass it as an argument:

```cmd
run_all.cmd --timeout-ms=120000
```

To run without enforcing a timeout (even if `aerogpu_timeout_runner.exe` is present):

```cmd
run_all.cmd --no-timeout
```

To require a specific PCI VID/DID (recommended for automation):

```cmd
:: Versioned ABI device model (AEROGPU_PCI_VENDOR_ID=0xA3A0 in aerogpu_pci.h)
run_all.cmd --require-vid=0xA3A0 --require-did=0x0001

:: Legacy bring-up device model (AEROGPU_PCI_VENDOR_ID=0x1AED in aerogpu_protocol.h)
run_all.cmd --require-vid=0x1AED --require-did=0x0001
```

You can find the correct VID/DID in the Win7 guest via:

* Device Manager → Display adapters → Properties → Details → **Hardware Ids**
* Or by reading the PCI ID from the AeroGPU driver INF you installed (see `drivers/aerogpu/packaging/win7/README.md`).

## Expected results

In a Win7 VM with AeroGPU installed and working correctly:

* `d3d9ex_dwm_probe` reports composition enabled (or successfully enables it)
* `d3d9ex_event_query` validates that `D3DQUERYTYPE_EVENT` polls non-blockingly (returns `S_FALSE` until signaled)
* `vblank_wait_sanity` validates that `D3DKMTWaitForVerticalBlankEvent` blocks on vblank and does not show huge stalls (fails fast on missing/broken vblank interrupt wiring)
* `wait_vblank_pacing` directly measures `D3DKMTWaitForVerticalBlankEvent()` pacing on VidPn source 0 (AeroGPU MVP) and fails on immediate returns (avg < 2ms) or stalls (max > 250ms). On a 60 Hz display it typically reports ~16.6ms.
* `dwm_flush_pacing` measures `DwmFlush()` pacing and fails on extremely fast returns (not vsync paced) or very large gaps (`--samples=N` controls sample count; default 120)
* `vblank_wait_pacing` directly measures `D3DKMTWaitForVerticalBlankEvent()` pacing and fails on immediate returns (avg ≤ 2ms) or stalls (avg ≥ 50ms / max ≥ 250ms). On a 60 Hz display it typically reports ~16.6ms.
* `get_scanline_sanity` calls `D3DKMTGetScanLine()` repeatedly and validates that scanline values vary and stay within the visible screen height (`--samples=N` controls sample count; default 200)
* `d3d9_raster_status_sanity` samples `IDirect3DDevice9::GetRasterStatus` and fails if vblank state never toggles or `ScanLine` is stuck (validates `D3DKMTGetScanLine` → `DxgkDdiGetScanLine` basic correctness)
* `d3d9_raster_status_pacing` samples `IDirect3DDevice9::GetRasterStatus` and fails if `InVBlank` never becomes true or scanline is stuck (useful for `DxgkDdiGetScanLine` bring-up)
* `d3d9ex_triangle` renders a green triangle over a red clear and confirms **corner red + center green** via readback
* `d3d9ex_query_latency` validates D3D9Ex `D3DQUERYTYPE_EVENT` polling + max frame latency APIs (prints query completion timing + configured latency)
* `d3d9ex_shared_surface` creates a D3D9Ex shared render-target (prefers texture; falls back to shared surface), duplicates the shared handle into a child process, and verifies the child can open it and submit GPU work (pass `--validate-sharing` / `--dump` to also validate pixels)
  * When debugging the KMD, this is also a good repro for validating stable `alloc_id` / `share_token` via allocation private driver data: the miniport should log the same IDs for `DxgkDdiCreateAllocation` (parent) and `DxgkDdiOpenAllocation` (child).
* `d3d9ex_shared_allocations` creates shared D3D9Ex resources (shared render-target surface + shared mipmapped texture) to exercise shared-surface allocation behavior
* `d3d11_triangle` renders a green triangle over a red clear and confirms **corner red + center green** via readback
* `readback_sanity` renders to an offscreen render target and validates readback pixels (corner red, center green)

All rendering tests also print the active adapter description + VendorId/DeviceId to help confirm the expected GPU/driver is being exercised.
