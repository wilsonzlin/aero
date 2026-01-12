# AeroGPU Windows 7 Guest-Side Direct3D Validation Suite

This directory contains small Windows 7 **guest-side** test programs intended to validate the AeroGPU WDDM driver stack end-to-end.

Each test prints a clear `PASS:` / `FAIL:` line to stdout and returns a non-zero exit code on failure. Some tests can optionally dump artifacts (usually `.bmp`, sometimes raw `.bin`) to disk for manual inspection (`--dump`). Image `*.bin` dumps are typically tightly-packed BGRA32 pixels (`width*height*4` bytes, no row padding), but some tests also dump raw buffer bytes.

In particular, `d3d10_map_do_not_wait`, `d3d10_1_map_do_not_wait`, and `d3d11_map_do_not_wait` validate that `Map(READ, DO_NOT_WAIT)` behaves like a non-blocking poll (never hanging the caller) and reports `DXGI_ERROR_WAS_STILL_DRAWING` when GPU work is still in flight.

For D3D11 UMD bring-up (Win7 FL10_0), including which `d3d11umddi.h` function-table entries must be non-null vs safely stubbable, see:

* `docs/graphics/win7-d3d11ddi-function-tables.md`

For automation, tests can also optionally emit a machine-readable JSON report (`--json[=PATH]`) with a stable `schema_version`.

The suite includes:

* `aerogpu_test_runner.exe` (preferred) – runs the full manifest, enforces per-test timeouts, and can emit an aggregated suite JSON report.
* `aerogpu_timeout_runner.exe` – legacy helper to run a single child process with a wall-clock timeout (used by older automation/scripts).

Override the default per-test timeout by setting `AEROGPU_TEST_TIMEOUT_MS` in the environment (consumed by both helpers).

Both `build_all_vs2010.cmd` and `run_all.cmd` are driven by `tests_manifest.txt`, which defines the ordered list of tests in the suite.

Common flags:

* `--dump` – write test-specific dump artifacts next to the executable (usually `*.bmp`; some tests also write raw `*.bin`).
* `--hidden` – hide windows for tests that create windows (useful for automation).
* `--show` – show the window for tests that support it (e.g. `d3d9ex_event_query`, `d3d9ex_submit_fence_stress`, `d3d9ex_shared_surface`, `d3d9ex_shared_surface_ipc`, `d3d9ex_shared_surface_wow64`, `d3d9ex_shared_surface_many_producers`, `d3d9ex_shared_surface_stress`; overrides `--hidden`).
* `--json[=PATH]` – emit a machine-readable JSON report (includes a stable `schema_version`).
* `--validate-sharing` – for `d3d9ex_shared_surface`: kept for backwards compatibility (pixel sharing is validated by default; `--dump` always validates).
* `--no-validate-sharing` – for `d3d9ex_shared_surface`: skip cross-process pixel sharing readback.
* `--producers=N` – for `d3d9ex_shared_surface_many_producers`: number of producer processes to spawn (default 8).
* `--samples=N` – control sample count for pacing/sampling tests (defaults vary per test).
* `--interval-ms=N` – for `vblank_state_sanity`, `fence_state_sanity`, and `ring_state_sanity`: delay between escape samples (default 100).
* `--iterations=N` – iteration count for `d3d9ex_event_query` (query submissions; default 6) and `d3d9ex_submit_fence_stress` (submit iterations; default 200).
* `--stress-iterations=N` – for `d3d9ex_event_query`: iterations per device in the multi-device stress phase (default 200).
* `--process-stress` – for `d3d9ex_event_query`: run the stress phase as two separate processes instead of two threads (useful for reproducing multi-process fence contention).
* `--wait-timeout-ms=N` – for `wait_vblank_pacing` and `vblank_wait_sanity`: per-wait timeout for `D3DKMTWaitForVerticalBlankEvent` (default 2000).
* `--require-vid=0x####` / `--require-did=0x####` – fail the test if the active adapter VID/DID does not match.
* `--allow-microsoft` – allow running on the Microsoft Basic Render Driver (normally treated as a failure to avoid false PASS when AeroGPU isn’t active).
* `--allow-non-aerogpu` – allow running on adapters whose description does not contain `AeroGPU` (by default, rendering tests expect to be running on an AeroGPU adapter).
* `--require-umd` – require that the expected AeroGPU user-mode driver DLL is loaded in-process (useful when `--allow-*` flags are set).
* `--require-agpu` – for tests with AGPU-only validation paths (e.g. ring descriptor/alloc table checks), fail instead of skipping when the active device/ring format is legacy.
* `--display \\.\DISPLAYn` – for `vblank_wait`: pick a display (default: primary).
* `--ring-id=N` – for `ring_state_sanity`: which ring ID to dump (default 0).
* `--allow-remote` – skip tests that are not meaningful under RDP (`SM_REMOTESESSION=1`): `device_state_sanity`, `d3d9ex_dwm_probe`, `d3d9ex_submit_fence_stress`, `fence_state_sanity`, `ring_state_sanity`, `dwm_flush_pacing`, `wait_vblank_pacing`, `vblank_wait`, `vblank_wait_pacing`, `vblank_wait_sanity`, `vblank_state_sanity`, `get_scanline_sanity`, `scanout_state_sanity`, `dump_createalloc_sanity`, `umd_private_sanity`, `transfer_feature_sanity`, `d3d9_raster_status_sanity`, `d3d9_raster_status_pacing`.
* `--help` / `/?` – print per-test usage.

## Layout

```
drivers/aerogpu/tests/win7/
  CMakeLists.txt
  build_all_vs2010.cmd
  run_all.cmd
  tests_manifest.txt
  common/
  timeout_runner/
  test_runner/
  device_state_sanity/
  d3d9ex_dwm_probe/
  d3d9ex_event_query/
  d3d9ex_dwm_ddi_sanity/
  d3d9ex_submit_fence_stress/
  fence_state_sanity/
  ring_state_sanity/
  vblank_wait_sanity/
  wait_vblank_pacing/
  vblank_wait_pacing/
  vblank_wait/
  vblank_state_sanity/
  get_scanline_sanity/
  scanout_state_sanity/
  dump_createalloc_sanity/
  umd_private_sanity/
  transfer_feature_sanity/
  d3d9_raster_status_sanity/
  d3d9_raster_status_pacing/
  dwm_flush_pacing/
  d3d9ex_triangle/
  d3d9ex_stateblock_sanity/
  d3d9ex_multiframe_triangle/
  d3d9ex_vb_dirty_range/
  d3d9ex_stretchrect/
  d3d9ex_query_latency/
  d3d9ex_shared_surface/
  d3d9ex_shared_surface_ipc/
  d3d9ex_alloc_id_persistence/
  d3d9ex_shared_surface_wow64/
  d3d9ex_shared_surface_many_producers/
  d3d9ex_shared_allocations/
  d3d9ex_shared_surface_stress/
  d3d10_triangle/
  d3d10_map_do_not_wait/
  d3d10_shared_surface_ipc/
  d3d10_1_triangle/
  d3d10_1_map_do_not_wait/
  d3d10_1_shared_surface_ipc/
  d3d10_caps_smoke/
  d3d11_triangle/
  d3d11_map_do_not_wait/
  d3d11_texture/
  d3d11_caps_smoke/
  d3d11_rs_om_state_sanity/
  d3d11_geometry_shader_smoke/
  dxgi_swapchain_probe/
  d3d11_swapchain_rotate_sanity/
  d3d11_map_dynamic_buffer_sanity/
  d3d11_map_roundtrip/
  d3d11_update_subresource_texture_sanity/
  d3d11_shared_surface_ipc/
  d3d11_texture_sampling_sanity/
  d3d11_dynamic_constant_buffer_sanity/
  d3d11_depth_test_sanity/
  readback_sanity/
```

## Prerequisites (Windows 7 guest)

### Runtime

* Windows 7 SP1 (x86 or x64)
* AeroGPU driver installed (KMD + UMDs)

### Build toolchain

The suite can be built on a modern Windows host using CMake + Visual Studio.

* Visual Studio 2019 or 2022 with C++ desktop development components
* CMake 3.16+
* A Windows SDK that still supports targeting/running on Windows 7 (commonly the **Windows 8.1 SDK**).

> Toolset note: If you need the broadest Win7 compatibility (especially for old guests without newer VC runtimes),
> build with a toolset that supports older targets (e.g. `-T v141_xp` if installed). The suite also defaults to a
> static CRT (`/MT`) to reduce guest-side redistributable requirements.

### D3D shader compiler DLL (guest runtime)

The shader-based tests (D3D10/D3D10.1/D3D11) compile their HLSL shaders at runtime via `D3DCompile`. On some Win7 installs, `d3dcompiler_47.dll`
may not be present by default. If shader compilation fails, install a Windows update that provides it (e.g. KB4019990)
or copy `d3dcompiler_47.dll` next to the test binaries in `win7/bin/`.

## Build (recommended: CMake + Visual Studio)

From a Visual Studio Developer Command Prompt (or any shell with CMake on PATH):

```cmd
cd \path\to\repo\drivers\aerogpu\tests\win7
cmake -S . -B build -G "Visual Studio 17 2022" -A Win32
cmake --build build --config Release
```

### Building the WOW64 cross-bitness test (Win7 x64)

`d3d9ex_shared_surface_wow64` requires both an **x86 producer** (`d3d9ex_shared_surface_wow64.exe`) and an **x64 consumer**
(`d3d9ex_shared_surface_wow64_consumer_x64.exe`) in `win7\bin\`. Build two CMake trees (Win32 + x64) to produce both:

```cmd
cmake -S . -B build-win32 -G "Visual Studio 17 2022" -A Win32
cmake --build build-win32 --config Release

cmake -S . -B build-x64 -G "Visual Studio 17 2022" -A x64
cmake --build build-x64 --config Release --target d3d9ex_shared_surface_wow64_consumer_x64
```

Note: the x64 generator can build x64 variants of the entire suite. Use the explicit `--target` above to avoid overwriting the Win32 binaries in `win7\\bin\\`.

Outputs are placed in:

```
drivers\aerogpu\tests\win7\bin\
```

Note: `d3d9ex_shared_surface_wow64` requires **two** binaries: an x86 producer plus an x64 consumer (`d3d9ex_shared_surface_wow64_consumer_x64.exe`).
When building with the Visual Studio CMake generator (`-G "Visual Studio ..."`) in a Win32 build, the suite can build the consumer binary via a nested x64
build (`AEROGPU_WIN7_BUILD_WOW64_CONSUMER`, enabled by default on 64-bit MSVC hosts; requires the x64 MSVC toolchain to be installed).

## Adding a new test

1. Add a new directory containing `main.cpp` and a `build_vs2010.cmd` that outputs `bin\<test_name>.exe`.
2. Add `<test_name>` to `tests_manifest.txt` at the desired position.
3. For the CMake build, add the new test target to `CMakeLists.txt` (linking the appropriate system libraries).

### Legacy build scripts

The `*_vs2010.cmd` scripts are retained for convenience, but they are not required for the modern build flow.

No other scripts need to be edited: `build_all_vs2010.cmd`, `run_all.cmd`, and `aerogpu_test_runner.exe` iterate the manifest.

## Run

From the same directory:

```cmd
run_all.cmd
```

Or run the native runner directly (preferred):

```cmd
bin\aerogpu_test_runner.exe
```

### Capturing per-test stdout/stderr (automation)

To redirect each test's stdout/stderr to files (useful when running under CI where console output can be truncated):

```cmd
bin\aerogpu_test_runner.exe --log-dir=logs
```

This writes (one pair per test) into `logs\`:

* `<test>.stdout.txt`
* `<test>.stderr.txt`

`--log-dir` may be absolute or relative to `win7\bin\`.

### Capturing driver status on failures (dbgctl)

If you have `aerogpu_dbgctl.exe` available in the guest, the runner can automatically capture a `--status`
snapshot after test failures/timeouts:

```cmd
bin\aerogpu_test_runner.exe --log-dir=logs --dbgctl=aerogpu_dbgctl.exe
```

By default the snapshot is written next to the per-test logs (or next to `report.json` when `--json` is used).
Use `--dbgctl-timeout-ms=NNNN` to bound how long the runner will wait for `aerogpu_dbgctl.exe` itself (default: 5000ms).

To also write BMP dumps next to the binaries:

```cmd
run_all.cmd --dump
```

### JSON output

To write an aggregated suite report:

```cmd
bin\aerogpu_test_runner.exe --json
```

This produces (by default) `win7\bin\report.json` and also writes per-test reports next to it.

Individual tests can also be run directly with `--json[=PATH]` to emit a single-test JSON report.

For suite usage:

```cmd
run_all.cmd --help
```

When running `run_all.cmd --json`, the script prefers delegating to `bin\\aerogpu_test_runner.exe`, producing a `report.json` suite summary plus per-test JSON outputs. If the suite runner is not present, `run_all.cmd` falls back to the legacy per-test loop (optionally using `aerogpu_timeout_runner.exe` to enforce timeouts and write fallback per-test JSON reports on failures/timeouts).

To increase the per-test timeout (default: 30000 ms):

```cmd
set AEROGPU_TEST_TIMEOUT_MS=120000
run_all.cmd
```

Or (equivalently) pass it as an argument:

```cmd
run_all.cmd --timeout-ms=120000
:: or:
run_all.cmd --timeout-ms 120000
```

To run without enforcing a timeout (even if `aerogpu_timeout_runner.exe` is present):

```cmd
run_all.cmd --no-timeout
```

To require a specific PCI VID/DID (recommended for automation):

```cmd
:: Versioned ABI device model (AEROGPU_PCI_VENDOR_ID=0xA3A0 in aerogpu_pci.h)
run_all.cmd --require-vid=0xA3A0 --require-did=0x0001
  
:: Legacy bring-up device model (deprecated; PCI identity defined in drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h)
:: NOTE: the shipped Win7 AeroGPU INFs bind to PCI\VEN_A3A0&DEV_0001 only; installing against the
:: legacy device model requires the legacy INFs under drivers/aerogpu/packaging/win7/legacy/
:: and enabling `emulator/aerogpu-legacy`.
run_all.cmd --require-vid=0x<legacy_vid> --require-did=0x0001
```
 
Note: the in-tree Win7 driver package binds to the versioned device (`VID=0xA3A0`). Running against the legacy device model requires enabling the legacy emulator device model feature (`emulator/aerogpu-legacy`) and installing using the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/`.

You can find the correct VID/DID in the Win7 guest via:

* Device Manager → Display adapters → Properties → Details → **Hardware Ids**
* Or by reading the PCI ID from the AeroGPU driver INF you installed (see `drivers/aerogpu/packaging/win7/README.md`).

## Expected results

In a Win7 VM with AeroGPU installed and working correctly:

* `device_state_sanity` queries AeroGPU device state via the `QUERY_DEVICE(_V2)` escape and validates the returned MMIO magic and ABI version (useful for diagnosing “not actually on AeroGPU” scenarios early)
* `d3d9ex_dwm_probe` reports composition enabled (or successfully enables it)
* `d3d9ex_event_query` validates that `GetData(D3DGETDATA_DONOTFLUSH)` is non-blocking (initial poll before `Flush`), that `D3DQUERYTYPE_EVENT` eventually signals, and stresses interleaved submissions + `PresentEx(D3DPRESENT_DONOTWAIT)` throttling (default: 2 threads; pass `--process-stress` to run the stress phase across 2 processes). Window is hidden by default; pass `--show` to display it.
* `d3d9ex_dwm_ddi_sanity` sanity-checks D3D9Ex/DDI calls used by DWM and common apps (`CheckDeviceState`, `WaitForVBlank`, GPU thread priority, resource residency) to ensure they are non-blocking and return expected values
* `d3d9ex_submit_fence_stress` runs a tight `Clear` + `Issue(D3DISSUE_END)` + `PresentEx(D3DPRESENT_DONOTWAIT)` loop and validates that the AeroGPU D3D9 UMD reports **monotonic per-submission fences** via debug logs (captured with the DBWIN protocol); when possible it also cross-checks `AEROGPU_ESCAPE_OP_QUERY_FENCE` completion against the observed fence. On **AGPU** devices it additionally validates that the most recent PRESENT submission is marked with `AEROGPU_SUBMIT_FLAG_PRESENT` in the ring descriptor and that the submission includes a non-zero `alloc_table_gpa` (skipped on legacy devices unless `--require-agpu`/`--require-umd` are specified).
* `fence_state_sanity` queries the KMD `QUERY_FENCE` escape repeatedly and validates that submitted/completed fences are monotonic and obey `completed <= submitted` (`--samples=N`, `--interval-ms=N`; skips if the escape is not supported)
* `ring_state_sanity` dumps ring state via `AEROGPU_ESCAPE_OP_DUMP_RING_V2` and validates basic invariants (`cmd_gpa/cmd_size` pairing, AGPU `alloc_table_gpa/alloc_table_size` pairing, monotonic head/tail on AGPU) (`--ring-id=N`, `--samples=N`, `--interval-ms=N`; skips if the escape is not supported)
* `vblank_wait_sanity` validates that `D3DKMTWaitForVerticalBlankEvent` blocks on vblank and does not show huge stalls (fails fast on missing/broken vblank interrupt wiring)
* `wait_vblank_pacing` directly measures `D3DKMTWaitForVerticalBlankEvent()` pacing on VidPn source 0 (AeroGPU MVP) and fails on immediate returns (avg < 2ms) or stalls (max > 250ms). On a 60 Hz display it typically reports ~16.6ms.
* `vblank_wait` directly measures `D3DKMTWaitForVerticalBlankEvent()` pacing on the selected display (default: primary) and fails on immediate returns (avg < 2ms) or stalls (max > 250ms). On a 60 Hz display it typically reports ~16.6ms.
* `dwm_flush_pacing` measures `DwmFlush()` pacing and fails on extremely fast returns (not vsync paced) or very large gaps (`--samples=N` controls sample count; default 120)
* `vblank_wait_pacing` directly measures `D3DKMTWaitForVerticalBlankEvent()` pacing and fails on immediate returns (avg ≤ 2ms) or stalls (avg ≥ 50ms / max ≥ 250ms). On a 60 Hz display it typically reports ~16.6ms.
* `vblank_state_sanity` queries the KMD `QUERY_VBLANK` escape repeatedly and validates monotonic vblank sequence/timestamps and that the measured cadence roughly matches the reported `vblank_period_ns` (`--samples=N`, `--interval-ms=N`; skips if the escape is not supported)
* `get_scanline_sanity` calls `D3DKMTGetScanLine()` repeatedly and validates that scanline values vary and stay within the visible screen height (`--samples=N` controls sample count; default 200)
* `scanout_state_sanity` queries AeroGPU scanout state via `AEROGPU_ESCAPE_OP_QUERY_SCANOUT` and validates that cached mode state matches the MMIO scanout registers and desktop resolution (catches broken/missing `DxgkDdiCommitVidPn` mode caching; skips if the escape is not supported)
* `dump_createalloc_sanity` dumps the KMD CreateAllocation trace via `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` and validates it is non-empty and internally consistent (helps diagnose allocation flag/pitch/share_token issues without a kernel debugger; skips if the escape is not supported)
* `umd_private_sanity` probes `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` and validates the returned `aerogpu_umd_private_v1` discovery blob (catches ABI/feature discovery regressions that can break UMD initialization)
* `transfer_feature_sanity` validates that AGPU devices advertising an ABI compatible with the current driver (`AEROGPU_ABI_MAJOR`, minor>=1) also advertise `AEROGPU_UMDPRIV_FEATURE_TRANSFER` via `DXGKQAITYPE_UMDRIVERPRIVATE` (fails fast on missing transfer/copy support required by D3D9/D3D11 readback paths; skipped on legacy device models unless `--require-agpu` is set)
* `d3d9_raster_status_sanity` samples `IDirect3DDevice9::GetRasterStatus` and fails if vblank state never toggles or `ScanLine` is stuck (validates `D3DKMTGetScanLine` → `DxgkDdiGetScanLine` basic correctness)
* `d3d9_raster_status_pacing` samples `IDirect3DDevice9::GetRasterStatus` and fails if `InVBlank` never becomes true or scanline is stuck (useful for `DxgkDdiGetScanLine` bring-up)
* `d3d9_validate_device_sanity` creates a D3D9Ex device and calls `IDirect3DDevice9Ex::ValidateDevice` after setting a few common render/sampler states (expects `D3D_OK` and `NumPasses >= 1`; prints a warning if it is not single-pass)
* `d3d9ex_triangle` renders a blue triangle over a red clear and confirms **corner red + center blue** via readback
* `d3d9ex_stateblock_sanity` validates `IDirect3DStateBlock9` record/apply/capture behavior by recording device state (texture + pixel shader constants + viewport), mutating it, then verifying `Apply()` restores the recorded state and `Capture()` updates it (validated via readback: **green then red**)
* `d3d9ex_draw_indexed_primitive_up` draws a green triangle over a red clear using `DrawIndexedPrimitiveUP` (user-pointer vertex/index data) and confirms **corner red + center green** via readback (can dump `*.bmp` / raw BGRA `*.bin` with `--dump`)
* `d3d9ex_multiframe_triangle` renders multiple frames using a persistent dynamic vertex buffer and confirms the **center pixel changes across frames** via readback (uses non-symmetric colors to catch channel-order regressions)
* `d3d9ex_vb_dirty_range` renders a blue triangle using a vertex buffer updated via `Lock/Unlock` and confirms **corner red + center blue** via readback (catches regressions in vertex-buffer dirty-range tracking / upload)
* `d3d9ex_stretchrect` exercises compositor-critical D3D9Ex DDIs: `ColorFill`, `UpdateSurface`, `StretchRect`, and `UpdateTexture` (validated via readback)
* `d3d9ex_query_latency` validates D3D9Ex `D3DQUERYTYPE_EVENT` polling + max frame latency APIs (prints query completion timing + configured latency)
* `d3d9ex_shared_surface` creates a D3D9Ex shared render-target (prefers texture; falls back to shared surface), duplicates the shared handle into a child process, and validates cross-process pixel visibility via readback (pass `--no-validate-sharing` to skip readback validation)
  * When debugging the KMD, this is also a good repro for validating stable `alloc_id` / `share_token`:
    * `alloc_id` is preserved cross-process via the WDDM allocation private-driver-data blob (`aerogpu_wddm_alloc_priv.alloc_id` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
    * `share_token` is the protocol token used by `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE` and must be stable across processes. For shared allocations, it is preserved cross-process via the WDDM allocation private-driver-data blob (`aerogpu_wddm_alloc_priv.share_token` in `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`).
    The miniport should log the same IDs for `DxgkDdiCreateAllocation` (parent) and `DxgkDdiOpenAllocation` (child).
  * If the shared handle is a real NT handle, the parent also (when supported) confirms `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` maps both process-local handles to the same stable **debug token**.
* `d3d9ex_shared_surface_ipc` creates a shared D3D9Ex render-target texture in one process, opens it in a second process, and validates the consumer can read back the producer’s clear color.
  * If the shared handle is a real NT handle, the producer duplicates it into the consumer (asserting the numeric handle value differs) and (when supported) confirms `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` maps both process-local handles to the same stable **debug token**.
  * If the shared handle is not a real NT handle, it falls back to passing the raw handle value (token-style shared handles).
* `d3d9ex_alloc_id_persistence` creates shared D3D9Ex render-target textures in *both* parent and child processes, opens them cross-process, and runs many iterations of cross-process `StretchRect` + readback. This validates that `alloc_id` values remain collision-free across processes (two alloc_ids in a single submission) and that per-submit allocation tables remain consistent across long-running workloads.
* `d3d9ex_shared_surface_wow64` validates **cross-bitness** D3D9Ex shared-surface interop on Win7 x64: an **x86 (WOW64) producer** duplicates a shared render-target texture handle into an **x64 consumer** and the consumer validates pixel data via readback (skipped on 32-bit OS). This mirrors the Win7 desktop scenario where DWM is 64-bit but many apps are 32-bit.
* `d3d9ex_shared_surface_many_producers` mimics the DWM “many producers → one compositor” workload:
  * spawns N producer processes, each creating its own shared render-target texture (distinct allocation)
  * duplicates/forwards each shared handle to the compositor process via a named mapping + event
  * opens all shared surfaces in the compositor and uses them together in a single submission (batch `ColorFill(...)` calls and one `Flush()`)
  * reads back each surface to validate unique per-producer colors
  * **this is specifically meant to catch `alloc_id` collisions across processes** (if two producers accidentally pick the same per-allocation ID, a single compositor submit referencing both surfaces should fail deterministically)
* `d3d9ex_shared_allocations` exercises allocation behavior for shared resources:
  * creates a non-shared mip chain texture (Levels=4) as a baseline for `NumAllocations` logging
  * creates a shared render-target surface and attempts shared textures that would imply multiple mips (Levels=4 and Levels=0/full chain), which may be rejected by the MVP single-allocation policy
* `d3d9ex_shared_surface_stress` repeatedly creates a shared D3D9Ex render target surface in a parent process, duplicates the shared handle into a child process, and validates the child can open the surface (including opening it twice) and issue basic rendering commands without hanging or crashing (`--iterations=N` controls loop count; default 20)
* `d3d10_triangle` uses `D3D10CreateDeviceAndSwapChain` (hardware), verifies the D3D10 runtime path (`d3d10.dll`) and the AeroGPU `OpenAdapter10` export, and confirms **corner red + center green** via readback
* `d3d10_map_do_not_wait` validates that `Map(READ, DO_NOT_WAIT)` is a non-blocking poll (returns `DXGI_ERROR_WAS_STILL_DRAWING` while work is in flight, never hangs)
* `d3d10_shared_surface_ipc` creates a shareable D3D10 render-target texture in one process, duplicates the shared `HANDLE` into a second process, opens it via `OpenSharedResource`, and validates the consumer can read back the producer’s clear color (catches bugs where the driver treats the numeric handle value as a stable cross-process token)
* `d3d10_1_triangle` uses `D3D10CreateDeviceAndSwapChain1` (hardware), verifies the D3D10.1 runtime path (`d3d10_1.dll`) and the AeroGPU `OpenAdapter10_2` export, and confirms **corner red + center green** via readback
* `d3d10_1_map_do_not_wait` is the D3D10.1 variant of the above `Map(READ, DO_NOT_WAIT)` non-blocking poll test
* `d3d10_1_shared_surface_ipc` is the D3D10.1 variant of `d3d10_shared_surface_ipc` (shared texture cross-process `HANDLE` duplication + `OpenSharedResource` + readback). It additionally validates that the AeroGPU UMD exports the D3D10.1 entrypoint `OpenAdapter10_2`.
* `d3d10_caps_smoke` validates `ID3D10Device::CheckFormatSupport` bits for a few core RT/DS + index/vertex formats used by common apps
* `d3d11_triangle` uses `D3D11CreateDeviceAndSwapChain` (hardware), verifies the D3D11 runtime path (`d3d11.dll`) and the AeroGPU `OpenAdapter11` export, and confirms **corner red + center green** via readback
* `d3d11_map_do_not_wait` validates that `Map(READ, DO_NOT_WAIT)` is a non-blocking poll (returns `DXGI_ERROR_WAS_STILL_DRAWING` while work is in flight, never hangs)
* `d3d11_texture` draws a textured triangle using a 2x2 BGRA texture and validates that the **center pixel samples the expected texel** (corner remains clear color) via staging readback
* `d3d11_caps_smoke` validates the expected D3D11 feature level and common format support bits used by the runtime
* `d3d11_rs_om_state_sanity` validates D3D11 rasterizer + blend state correctness (scissor enable/disable + `RSSetState(NULL)`, cull mode/front-face, depth clip enable/disable, alpha blending + write mask + blend factor + sample mask) via readback (requires feature level >= 10_0)
* `d3d11_geometry_shader_smoke` renders a triangle through the Geometry Shader stage (requires feature level >= 10_0) and confirms **corner red + center green** via readback
* `dxgi_swapchain_probe` creates a 2-buffer windowed DXGI swapchain and presents a few vsync-paced frames (useful for swapchain/backbuffer tracing; use `--api=d3d11|d3d10|d3d10_1` to select the runtime path)
* `d3d11_swapchain_rotate_sanity` creates a 2-buffer swapchain, clears buffer0 red + buffer1 green, presents, then validates that DXGI rotated buffer identities (expects **buffer0 green + buffer1 red**)
* `d3d11_map_dynamic_buffer_sanity` exercises dynamic buffer CPU-write paths (`Map(WRITE_DISCARD)` + `Map(WRITE_NO_OVERWRITE)`), stresses DISCARD renaming hazards, and validates vertex/index/constant buffer map paths via staging readback
* `d3d11_map_roundtrip` validates `Map/Unmap` on a `D3D11_USAGE_STAGING` texture by writing a checker pattern via `Map(WRITE)` and reading it back via `Map(READ)` (no rendering required)
* `d3d11_update_subresource_texture_sanity` validates `UpdateSubresource` on both textures (full + boxed update, padded RowPitch) and a DEFAULT constant buffer (full + boxed range update) via staging readback
* `d3d11_shared_surface_ipc` creates a D3D11 shareable texture in one process, duplicates the shared `HANDLE` into a second process, opens it via `OpenSharedResource`, and validates the consumer can read back the producer's clear color (catches bugs where the driver treats the numeric handle value as a stable cross-process token)
* `readback_sanity` renders to an offscreen render target and validates readback pixels (corner red, center green)
* `d3d11_texture_sampling_sanity` renders a textured quad into an offscreen render target and validates a few sampled texels via readback (requires feature level >= 10_0)
* `d3d11_dynamic_constant_buffer_sanity` draws using a dynamic constant buffer updated with `Map(WRITE_DISCARD)` between draws (blue fullscreen, then green centered triangle) and validates output via readback (requires feature level >= 10_0)
* `d3d11_depth_test_sanity` validates `CreateDepthStencilView`, `ClearDepthStencilView` (clears depth to both 0.0 and 1.0), depth comparisons, and depth writes via readback (requires feature level >= 10_0)

All rendering tests also print the active adapter description + VendorId/DeviceId to help confirm the expected GPU/driver is being exercised.
The D3D9Ex and D3D10/11-based tests also print the resolved path of the loaded AeroGPU UMD DLL (including process bitness and WOW64 state, e.g. `x86 (WOW64)`), to validate WOW64 registration.
