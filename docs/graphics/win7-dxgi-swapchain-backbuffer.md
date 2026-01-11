# Win7 DXGI swapchain backbuffer: UMD `CreateResource` inputs + allocation flags (trace guide)

This note documents how to **empirically capture** the `CreateResource` parameters the Windows 7 **DXGI 1.1 + D3D10/11 runtime** passes to the AeroGPU **D3D10/11 UMD** when creating **swapchain backbuffers**, and how to translate those parameters into allocation flags that keep `Present` stable.

The main goal is to avoid “guessing” the backbuffer recipe: on Win7/WDDM 1.1, swapchain buffers are created by DXGI/runtime on the app’s behalf, and the *UMD must match what the runtime expects*.

## Capturing the runtime’s `CreateResource` calls

### 1) Build an instrumented UMD

`drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_umd.cpp` contains trace logging guarded by:

* `AEROGPU_UMD_TRACE_RESOURCES`

The Visual Studio project `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_11.vcxproj` defines this macro for **Debug** builds only.

The trace is emitted via the standard D3D10/11 UMD logging helper (`AEROGPU_D3D10_11_LOG`), which writes to `OutputDebugStringA`.
Lines are prefixed by the logging helper (currently `AEROGPU_D3D11DDI:`) and then tagged with:

* `trace_resources:`

> Note: the trace hooks are compiled into both the repo “portable ABI subset” UMD path and the WDK-backed Win7 UMD DDIs
> (`aerogpu_d3d10_umd_wdk.cpp`, `aerogpu_d3d10_1_umd_wdk.cpp`, `aerogpu_d3d11_umd_wdk.cpp`). This means the default
> WDK build (`/p:AeroGpuUseWdkHeaders=1`) will still emit the `trace_resources:` lines.

### 2) Run the DXGI probe app on Win7

The guest-side probe lives at:

* `drivers/aerogpu/tests/win7/dxgi_swapchain_probe/`

It creates:

* a D3D11 device by default (`--api=d3d11`)
  * or a D3D10 device (`--api=d3d10`)
  * or a D3D10.1 device (`--api=d3d10_1`)
* a **windowed** `DXGI_SWAP_CHAIN_DESC` swapchain with **2 buffers**
* RTVs for both buffers
* a few `Present(1,0)` frames (vsync)

Build on Win7 (VS2010 toolchain):

```cmd
cd \path\to\repo\drivers\aerogpu\tests\win7
build_all_vs2010.cmd
```

Run:

```cmd
bin\dxgi_swapchain_probe.exe --api=d3d11 --require-vid=0xA3A0 --require-did=0x0001
bin\dxgi_swapchain_probe.exe --api=d3d10 --require-vid=0xA3A0 --require-did=0x0001
bin\dxgi_swapchain_probe.exe --api=d3d10_1 --require-vid=0xA3A0 --require-did=0x0001
```

### 3) Capture the UMD output

Use Sysinternals **DebugView** (or any debugger) to capture `OutputDebugStringA` output while the probe runs.

Alternatively, the UMD logging helper can also append to a file (useful when DebugView/WinDbg is not convenient):

```cmd
set AEROGPU_D3D10_11_LOG=1
set AEROGPU_D3D10_11_LOG_FILE=C:\aerogpu_d3d10_11_umd.log
bin\dxgi_swapchain_probe.exe ...
```

Note: `AEROGPU_D3D10_11_LOG` defaults to enabled in `_DEBUG` builds; for Release builds you must set it explicitly.

## What to extract from the trace

The UMD prints three key call sites:

* `CreateResource` (resource descriptors)
* `RotateResourceIdentities` (the set of swapchain buffer identities, before/after rotation)
* `Present` (which backbuffer identity is presented and with what sync interval)

> Note: depending on which UMD build you are running, the `Present` trace line may
> print the presented resource as either `src_handle=<id>` (WDK-backed DDIs) or
> `backbuffer_handle=<id>` (portable ABI subset). They refer to the same protocol
> resource handle space.

To identify *which* `CreateResource` calls are swapchain backbuffers:

1. Find the handles printed by `RotateResourceIdentities`.
2. Match those handles to the immediately preceding `CreateResource => created tex2d handle=...` lines.

> Tip: when using the WDK-backed DDI path, `CreateResource` descriptors may also include:
>
> * `primary_desc=<ptr>` (mirrors `D3D10DDIARG_CREATERESOURCE::pPrimaryDesc` / `D3D11DDIARG_CREATERESOURCE::pPrimaryDesc`)
> * `primary=0/1` (derived from `primary_desc != NULL`)
>
> `primary_desc != NULL` / `primary=1` is a strong signal that the resource is a **DXGI primary/backbuffer**
> allocation, which can make it easier to scan logs manually. The parser script will include `primary` when present
> (and can infer it from `primary_desc` for older logs).

### Optional: automated extraction

For convenience, the repo includes a small host-side parser that scans a captured log and prints the
backbuffer handles observed via `RotateResourceIdentities` along with their matching `CreateResource`
descriptors:

```bash
python scripts/parse_win7_dxgi_swapchain_trace.py aerogpu_d3d10_11_umd.log
python scripts/parse_win7_dxgi_swapchain_trace.py --json=swapchain_trace.json aerogpu_d3d10_11_umd.log
```

### Capturing KMD-facing allocation flags (optional but recommended)

To understand which **WDDM allocation flags** are required for `Present` stability, capture what
dxgkrnl/runtime passes into the miniport via `DxgkDdiCreateAllocation`.

`drivers/aerogpu/kmd/src/aerogpu_kmd.c` supports two capture paths:

1. **Escape-based (recommended; no kernel debugger required)**  
   The KMD maintains a small ring buffer of recent `DxgkDdiCreateAllocation` events and exposes it via
   the dbgctl escape `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION`.

   Build and run the Win7 dbgctl tool (`drivers/aerogpu/tools/win7_dbgctl`) and use:

   ```cmd
   aerogpu_dbgctl --dump-createalloc
   ```

   The dump includes:
   * the **incoming** `DXGK_ALLOCATIONINFO::Flags.Value` from dxgkrnl/runtime (`flags_in`)
   * the **final** flags after AeroGPU applies its required bits (`flags_out`, currently adds `CpuVisible` + `Aperture`)

2. **DbgPrint-based (DBG builds; optional extra verbosity)**  
   Build the KMD with:

   * `AEROGPU_KMD_TRACE_CREATEALLOCATION=1`

   This logs the first few `CreateAllocation` calls via `DbgPrintEx` and includes `flags=0xIN->0xOUT` style lines:

   ```
   aerogpu-kmd: CreateAllocation: alloc_id=... flags=0x12345678->0x1234D678
   ```

   These are easiest to capture under WinDbg (kernel debug) or any setup that collects `DbgPrintEx`.

## Backbuffer allocation recipe (Win7 / WDDM 1.1)

The backbuffer “recipe” should be derived directly from the `CreateResource` trace lines, but the stable *invariants* that the allocation logic should enforce are:

### Resource descriptor invariants

For a standard Win7 windowed swapchain (`DXGI_SWAP_EFFECT_DISCARD`, `SampleDesc.Count = 1`):

* `Dimension`: `TEX2D`
* `Width`/`Height`: swapchain buffer size
* `MipLevels`: `1`
* `ArraySize`: `1`
* `Format`: swapchain format (commonly `DXGI_FORMAT_B8G8R8A8_UNORM` on Win7 + DWM)
* `BindFlags`: must include render-target output (e.g. `D3D11_BIND_RENDER_TARGET`)
  * may include shader input if the swapchain `BufferUsage` requested it
* `CPUAccessFlags`: `0`
* `Usage`: typically `DEFAULT` (driver should treat any other value as suspicious for swapchain buffers)
* `SampleDesc`: typically `(Count=1, Quality=0)` (MSAA swapchains are out-of-scope for early bring-up)

### Allocation flag invariants (KMD-facing)

For AeroGPU’s current MVP memory model (single system-memory segment), stability requirements are:

* **Preserve runtime-requested flags**:
  * In `DxgkDdiCreateAllocation`, do **not** zero `DXGK_ALLOCATIONINFO::Flags` for normal allocations.
    DXGI/runtime may set “special” bits for swapchain buffers; clearing them can break `Present`.
* **Ensure CPU visibility** (so the emulator can read/write the backing):
  * Set `DXGK_ALLOCATIONINFO::Flags.CpuVisible = 1`
  * Set `DXGK_ALLOCATIONINFO::Flags.Aperture = 1`

These invariants are intentionally conservative; as the trace data is collected, tighten the rules to match
exact Win7 runtime behavior.
