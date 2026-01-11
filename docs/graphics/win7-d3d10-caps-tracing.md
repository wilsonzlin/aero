# Win7 D3D10DDI caps + entrypoint tracing (AeroGPU UMD)

This note describes how to enable a **lightweight tracing facility** in the AeroGPU D3D10/11 UMD so you can quickly discover:

* Which `D3D10DDIARG_GETCAPS::Type` values the D3D10 runtime/DXGI request during device + swapchain creation.
* Which D3D10DDI entrypoints the runtime calls beyond the minimal “triangle” set (helps avoid NULL DDI function pointer crashes).

Tracing is implemented with **`OutputDebugStringA`** and can be captured in a Windows 7 VM using **Sysinternals DebugView**.

---

## 1) Enable tracing

### 1.1 Compile-time gate

Build the UMD with:

* `AEROGPU_D3D10_TRACE=1`

If you are building the real Win7 driver against WDK headers, also build with:

* `AEROGPU_UMD_USE_WDK_HEADERS=1`

### 1.2 Runtime gate (environment variable)

At runtime, set:

* `AEROGPU_D3D10_TRACE=1` – log high-level calls (adapter open, `GetCaps`, device creation, resource/view creation, `Present`, etc.)
* `AEROGPU_D3D10_TRACE=2` – verbose (includes per-draw/per-state calls like `SetRenderTargets`, clears, draws, etc.)

Example (per-process):

```cmd
set AEROGPU_D3D10_TRACE=2
your_test_app.exe
```

To enable globally for system processes (e.g. if tracing `dwm.exe`), set it via System Properties or:

```cmd
setx AEROGPU_D3D10_TRACE 2
```

---

## 2) Capture output on Win7

1. Copy **DebugView** (`DbgView.exe`) into the VM (Sysinternals suite).
2. Run it as Administrator (recommended).
3. Enable:
   * `Capture Win32`
   * (optional) `Capture Global Win32`
4. Run the D3D10 app/test (e.g. a simple `D3D10CreateDeviceAndSwapChain` sample).

---

## 3) Interpreting the trace

Typical lines look like:

```text
[AeroGPU:D3D10 t=123456 tid=1337 #0] OpenAdapter10
[AeroGPU:D3D10 t=123456 tid=1337 #1] OpenAdapterCommon iface=... ver=...
[AeroGPU:D3D10 t=123457 tid=1337 #2] GetCaps hAdapter=0x... Type=12 DataSize=64 pData=0x...
[AeroGPU:D3D10 t=123457 tid=1337 #3] GetCaps -> hr=0x00000000
[AeroGPU:D3D10 t=123458 tid=1337 #4] CreateDevice hAdapter=0x... hDevice=0x...
[AeroGPU:D3D10 t=123458 tid=1337 #5] CreateDevice -> hr=0x00000000
[AeroGPU:D3D10 t=123500 tid=1337 #6] CreateRTV hDevice=0x... hResource=0x...
[AeroGPU:D3D10 t=123500 tid=1337 #7] Present hDevice=0x... syncInterval=1 backbuffer=0x...
```

### 3.1 `GetCaps` sequencing

* The `Type=<n>` field is the raw `D3D10DDIARG_GETCAPS::Type` value requested by the runtime.
* Cross-reference `<n>` against the Win7-era WDK header (`d3d10umddi.h`) to find the corresponding `D3D10DDI_GETCAPS_TYPE` / `D3D10DDICAPS_TYPE_*` enum entry.

Bring-up workflow:

1. Enable tracing.
2. Run the app until it fails.
3. Find the last `GetCaps Type=...` query; implement that caps type (or return a conservative “not supported” result).
4. Repeat until you reach RTV creation + `Present`.

### 3.2 Unexpected entrypoints / NULL-vtable avoidance

When you start wiring up the real WDK DDI tables, make sure **every function pointer** in the returned DDI tables is non-NULL (even if it’s just a stub that logs and returns `E_NOTIMPL` / calls `pfnSetErrorCb`).

The intent of this trace facility is to make those unexpected calls visible quickly so you can either:

* implement the entrypoint, or
* stop advertising the capability that triggers it.

