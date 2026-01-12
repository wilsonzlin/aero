# Win7 D3D9 UMD call tracing (AeroGPU)

This repo contains a small **in-UMD smoke-test trace facility** for the Win7 D3D9Ex user-mode display driver (UMD).

It is intended to answer one question during bring-up:

> “Which D3D9UMDDI entrypoints does `dwm.exe` (or a small D3D9Ex test) actually call, and with what key parameters / HRESULTs?”

The tracing implementation is **logging/introspection only**:

- No allocations on hot paths
- No file I/O on hot paths
- In-memory fixed-size buffer + a one-shot dump trigger via `OutputDebugStringA` (optionally also `stderr`)

Source: `drivers/aerogpu/umd/d3d9/src/aerogpu_trace.*`

The recommended repro apps below are part of the Win7 guest validation suite.
Build/run instructions live in: `drivers/aerogpu/tests/win7/README.md`.

Host-side unit tests (no Win7 VM required) live in:

- `drivers/aerogpu/umd/d3d9/tests/`

They validate the trace filtering and dump trigger behavior (including unique-mode force-record cases) and run under CI via `ctest` when `AERO_AEROGPU_BUILD_TESTS=ON`.

---

## Enabling tracing

Tracing is **disabled by default**. Enable it by setting environment variables in the target process (or globally, then restarting the process).

### Required

- `AEROGPU_D3D9_TRACE=1`  
  Enables trace recording.

### Optional controls

- `AEROGPU_D3D9_TRACE_MODE=unique|all` (default: `unique`)
  - `unique`: records only the **first call per entrypoint** (best for `dwm.exe`, avoids log spam)
  - `all`: records every call until the fixed buffer is full

- `AEROGPU_D3D9_TRACE_MAX=<N>` (default: 512)  
  Maximum number of records to store (clamped to `<= 512`). `0` is treated as “use the default” (512).

- `AEROGPU_D3D9_TRACE_FILTER=<TOKENS>`  
  Records only entrypoints whose trace name contains any of the comma-separated tokens (case-insensitive substring match).
  Leading/trailing whitespace around tokens is ignored.
  If the filter value is empty or contains only commas/whitespace (for example `AEROGPU_D3D9_TRACE_FILTER=,, ,`), the filter is treated as **unset** (`filter_on=0`).
  Note: the filter applies to recording and per-entrypoint dump triggers (`AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1` / `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1` will only fire for filtered-in entrypoints).
  The present-count dump trigger (`AEROGPU_D3D9_TRACE_DUMP_PRESENT`) is not suppressed by the filter, but the filter still controls which calls are recorded (including whether the triggering `Present` call is force-recorded).
  Example: `AEROGPU_D3D9_TRACE_FILTER=StateBlock,ValidateDevice`
  Tip: use `AEROGPU_D3D9_TRACE_FILTER=stub` to record only stubbed entrypoints (trace names include the substring `(stub)`).

- `AEROGPU_D3D9_TRACE_STDERR=1` (Windows-only; optional)  
  By default, trace output on Windows is emitted via `OutputDebugStringA` (for DebugView/WinDbg). When this is set, the trace output is also echoed to `stderr` (useful for console repro apps and host-side unit tests).

### Common recipes

#### Debug StateBlock / ValidateDevice (minimal repro apps)

For `d3d9_validate_device_sanity` and `d3d9ex_stateblock_sanity`, a useful setup is:

```cmd
set AEROGPU_D3D9_TRACE=1
set AEROGPU_D3D9_TRACE_MODE=all
set AEROGPU_D3D9_TRACE_FILTER=StateBlock,ValidateDevice
set AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1
```

If you suspect the app is failing early, you can also use:

```cmd
set AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1
```

#### Identify the first stubbed DDI hit

When you suspect the runtime or `dwm.exe` is calling an unimplemented DDI, a useful setup is:

```cmd
set AEROGPU_D3D9_TRACE=1
set AEROGPU_D3D9_TRACE_MODE=unique
set AEROGPU_D3D9_TRACE_FILTER=stub
set AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1
set AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1
```

### Dump triggers (on-demand)

The trace buffer is only dumped when triggered:

- `AEROGPU_D3D9_TRACE_DUMP_PRESENT=<N>`  
  Dumps once when the UMD device `present_count` reaches `N` (works for both `Present` and `PresentEx`).
  Note: when `AEROGPU_D3D9_TRACE_MODE=unique`, the triggering `Present`/`PresentEx` call is force-recorded so the dump still includes the call that caused the trigger (unless it is filtered out by `AEROGPU_D3D9_TRACE_FILTER`).
  The dump still triggers even if the present entrypoints are filtered out; the dump just won't include the present call record.

- `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1`  
  Dumps once on `DllMain(DLL_PROCESS_DETACH)`.

- `AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1`  
  Dumps once on the first traced entrypoint that returns a failing HRESULT (`FAILED(hr)`). The dump reason string is the failing entrypoint name.
  Note: when `AEROGPU_D3D9_TRACE_MODE=unique`, the failing call is force-recorded so the dump still includes the call that caused the trigger (unless it is filtered out by `AEROGPU_D3D9_TRACE_FILTER`).

- `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1`  
  Dumps once on the first traced entrypoint whose trace name is marked as a stub (contains the substring `(stub)`). This is useful for quickly identifying when the Win7 D3D9 runtime (or `dwm.exe`) exercises an unimplemented DDI.
  Note: some DDIs are intentionally treated as benign bring-up **no-ops** and are **not** marked as stubs in trace output, so they do not trigger this dump (see `drivers/aerogpu/umd/d3d9/README.md`).
  Note: when `AEROGPU_D3D9_TRACE_MODE=unique`, the triggering stub call is force-recorded so the dump still includes the call that caused the trigger (unless it is filtered out by `AEROGPU_D3D9_TRACE_FILTER`).

For `dwm.exe`, prefer `AEROGPU_D3D9_TRACE_DUMP_PRESENT` so you get logs *while DWM is running*, rather than only at shutdown.
For small repro apps that don't call `Present`/`PresentEx` (for example `d3d9_validate_device_sanity` and `d3d9ex_stateblock_sanity`), prefer `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1` so the trace dumps when the process exits.

---

## Capturing logs (DebugView)

The dump uses `OutputDebugStringA` by default.

If you are tracing a **console app** (for example one of the Win7 guest validation tests), you can also set `AEROGPU_D3D9_TRACE_STDERR=1` so trace output appears in the console `stderr` stream.

If you run the guest tests via `aerogpu_test_runner.exe --log-dir=...`, enabling `AEROGPU_D3D9_TRACE_STDERR=1` will capture trace dumps into the per-test `*.stderr.txt` files, which is often more convenient than DebugView.

Recommended workflow on Win7:

1. Run **Sysinternals DebugView** as Administrator
2. Enable:
   - `Capture Win32`
   - `Capture Global Win32`
3. Start the target app:
   - `drivers/aerogpu/tests/win7/d3d9ex_dwm_probe`
   - `drivers/aerogpu/tests/win7/d3d9_validate_device_sanity`
   - `drivers/aerogpu/tests/win7/d3d9ex_triangle`
   - `drivers/aerogpu/tests/win7/d3d9ex_stateblock_sanity`
   - or restart `dwm.exe` after setting env vars

   Note: `d3d9_validate_device_sanity` and `d3d9ex_stateblock_sanity` don't call `Present`/`PresentEx`, so `AEROGPU_D3D9_TRACE_DUMP_PRESENT` won't trigger for them. Use `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1` instead.

You should see lines starting with:

```
aerogpu-d3d9-trace: dump reason=...
```

---

## Reading the output

When tracing is enabled, the UMD prints a one-line banner describing the active configuration.
When a dump trigger fires, the first dump line repeats key parts of that configuration:

- `mode`: `unique` or `all`
- `max`: effective record capacity (after applying `AEROGPU_D3D9_TRACE_MAX`)
- `dump_present`: the configured `AEROGPU_D3D9_TRACE_DUMP_PRESENT` count (0 = disabled)
- `dump_on_detach`: whether `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1` is enabled
- `dump_on_fail`: whether `AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1` is enabled
- `dump_on_stub`: whether `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1` is enabled
- `stderr_on`: whether `AEROGPU_D3D9_TRACE_STDERR=1` is enabled (Windows-only echo)
- `filter_on` / `filter_count`: whether `AEROGPU_D3D9_TRACE_FILTER` is active and how many entrypoints are included

Example:

```
aerogpu-d3d9-trace: #004 t=123456 tid=1234 Device::CreateResource a0=0x... a1=0x... a2=0x... a3=0x... hr=0x00000000
```

Fields:

- `#NNN`: record index (in call order, up to `AEROGPU_D3D9_TRACE_MAX`)
- `t`: raw timestamp (QPC ticks on Windows)
- `tid`: thread id
- function name: DDI entrypoint
- `a0..a3`: key arguments (packed as needed; see below)
- `hr`: HRESULT returned by the entrypoint
  - `hr=0x7fffffff` is a special “pending” marker that indicates the call was recorded but had not yet reached its `return` path when the dump was taken (rare; typically only possible if the process is crashing or a dump trigger fires mid-call).

### Common argument packings

This trace is meant to be lightweight, so most values are logged as raw integers/pointers:

- `Device::CreateResource`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = pack_u32_u32(type, format)`
  - `a2 = pack_u32_u32(width, height)`
  - `a3 = pack_u32_u32(usage, pool)`

- `Device::PresentEx`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = hWnd`
  - `a2 = pack_u32_u32(sync_interval, d3d9_present_flags)`
  - `a3 = hSrc.pDrvPrivate`

- `Device::Present`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = hSwapChain.pDrvPrivate`
  - `a2 = hSrc.pDrvPrivate`
  - `a3 = pack_u32_u32(sync_interval, d3d9_present_flags)`

- `Device::GetQueryData`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = hQuery.pDrvPrivate`
  - `a2 = pack_u32_u32(data_size, flags)`
  - `a3 = pData`

- `Device::CreateStateBlock`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = state block type` (`D3DSBT_ALL=1`, `D3DSBT_PIXELSTATE=2`, `D3DSBT_VERTEXSTATE=3`)
  - `a2 = out stateblock handle pointer` (either `phStateBlock` or the CreateStateBlock args struct pointer)
  - `a3 = (unused)`

- `Device::BeginStateBlock`
  - `a0 = hDevice.pDrvPrivate`

- `Device::EndStateBlock`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = out stateblock handle pointer` (`phStateBlock`)

- `Device::ApplyStateBlock` / `Device::CaptureStateBlock` / `Device::DeleteStateBlock`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = hStateBlock.pDrvPrivate`

- `Device::ValidateDevice`
  - `a0 = hDevice.pDrvPrivate`
  - `a1 = out pass count pointer` (either `pNumPasses` or the ValidateDevice args struct pointer)

- Legacy fixed-function state (cached-only for Get*/StateBlock compatibility):
  - `Device::SetTextureStageState` / `Device::GetTextureStageState`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = pack_u32_u32(stage, state)`
    - `a2 = value` (Set) or `pValue` (Get)
  - `Device::SetTransform` / `Device::MultiplyTransform` / `Device::GetTransform`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = transform state id` (`D3DTRANSFORMSTATETYPE` numeric value)
    - `a2 = matrix pointer`
  - `Device::SetClipPlane` / `Device::GetClipPlane`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = plane index`
    - `a2 = plane pointer`
  - `Device::SetStreamSourceFreq` / `Device::GetStreamSourceFreq`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = stream index`
    - `a2 = value` (Set) or `pValue` (Get)
  - `Device::SetSoftwareVertexProcessing` / `Device::GetSoftwareVertexProcessing`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = enabled` (Set) or `pEnabled` (Get)
  - `Device::SetNPatchMode` / `Device::GetNPatchMode`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = mode` (Set) or `pMode` (Get)
  - `Device::SetShaderConstI/B` / `Device::GetShaderConstI/B`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = shader stage` (VS=0, PS=1)
    - `a2 = pack_u32_u32(start_register, count)`
    - `a3 = data pointer`

- Legacy cached state (not emitted to the AeroGPU command stream):
  - `Device::SetPaletteEntries` / `Device::GetPaletteEntries`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = palette index`
    - `a2 = entries pointer`
  - `Device::SetCurrentTexturePalette` / `Device::GetCurrentTexturePalette`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = palette index` (Set) or `pPalette` pointer (Get)
  - `Device::SetClipStatus` / `Device::GetClipStatus`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = clip status pointer`
  - `Device::SetGammaRamp` / `Device::GetGammaRamp`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = arg1` (runtime-specific)
    - `a2 = arg2` (runtime-specific)
    - `a3 = gamma ramp pointer`

- Resource priority + autogen filter type (cached-only):
  - `Device::SetPriority` / `Device::GetPriority`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = hResource.pDrvPrivate`
    - `a2 = new priority` (Set) or `pPriority` pointer (Get)
    - `a3 = pOldPriority` pointer (Set, when present)
  - `Device::SetAutoGenFilterType` / `Device::GetAutoGenFilterType`
    - `a0 = hDevice.pDrvPrivate`
    - `a1 = hResource.pDrvPrivate`
    - `a2 = new filter type` (Set) or `pFilterType` pointer (Get)

The exact packing per entrypoint is defined where the DDI is instrumented:
`drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_driver.cpp` (search for `D3d9TraceCall`).

---

## How to use this to drive implementation

1. Run `d3d9ex_dwm_probe` (or `dwm.exe`) with `TRACE_MODE=unique`.
2. Dump the trace (present-trigger recommended).
3. Treat the resulting call list as your **bring-up checklist**:
   - Any entrypoints that appear in the trace must be correct/stable for DWM.
   - If you see repeated failures (`hr != S_OK`) for a call, that’s often the *next missing feature*.
4. Iterate:
   - Add support for the next DDI/caps struct/state that the trace indicates is being queried or used.
   - Re-run and compare traces.
