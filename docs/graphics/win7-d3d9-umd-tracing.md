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

- `AEROGPU_D3D9_TRACE_MAX=<N>` (default: 512, clamped to `[1, 512]`)  
  Maximum number of records to store.

- `AEROGPU_D3D9_TRACE_FILTER=<TOKENS>`  
  Records only entrypoints whose trace name contains any of the comma-separated tokens (case-insensitive substring match).
  Leading/trailing whitespace around tokens is ignored.
  Note: the filter applies to both recording and dump triggers (for example, `AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1` / `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1` will only fire for filtered-in entrypoints).
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

### Dump triggers (on-demand)

The trace buffer is only dumped when triggered:

- `AEROGPU_D3D9_TRACE_DUMP_PRESENT=<N>`  
  Dumps once when the UMD device `present_count` reaches `N` (works for both `Present` and `PresentEx`).

- `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1`  
  Dumps once on `DllMain(DLL_PROCESS_DETACH)`.

- `AEROGPU_D3D9_TRACE_DUMP_ON_FAIL=1`  
  Dumps once on the first traced entrypoint that returns a failing HRESULT (`FAILED(hr)`). The dump reason string is the failing entrypoint name.

- `AEROGPU_D3D9_TRACE_DUMP_ON_STUB=1`  
  Dumps once on the first traced entrypoint whose trace name is marked as a stub (contains the substring `(stub)`). This is useful for quickly identifying when the Win7 D3D9 runtime (or `dwm.exe`) exercises an unimplemented DDI, even if that stub returns `S_OK`.

For `dwm.exe`, prefer `AEROGPU_D3D9_TRACE_DUMP_PRESENT` so you get logs *while DWM is running*, rather than only at shutdown.
For small repro apps that don't call `Present`/`PresentEx` (for example `d3d9_validate_device_sanity` and `d3d9ex_stateblock_sanity`), prefer `AEROGPU_D3D9_TRACE_DUMP_ON_DETACH=1` so the trace dumps when the process exits.

---

## Capturing logs (DebugView)

The dump uses `OutputDebugStringA` by default.

If you are tracing a **console app** (for example one of the Win7 guest validation tests), you can also set `AEROGPU_D3D9_TRACE_STDERR=1` so trace output appears in the console `stderr` stream.

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
