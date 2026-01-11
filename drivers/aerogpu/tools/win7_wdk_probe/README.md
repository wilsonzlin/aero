# AeroGPU Win7 WDK header/layout probe (D3D10/11 UMD allocations + submission + fences)

This is a small **Windows-only** console tool intended to be built in an environment that provides the
Win7-era D3D10/11 UMD DDI headers (typically from a Windows SDK/WDK install).

It exists to catch “wrong header version / wrong packing / wrong target arch” problems early by
printing `sizeof`/`offsetof` for the key structs involved in Win7 (WDDM 1.1) D3D10/D3D11 UMD:

- CreateResource allocation contract (resource backing allocations):
  - `D3D10DDIARG_CREATERESOURCE`
  - `D3D11DDIARG_CREATERESOURCE`
  - `D3DDDI_ALLOCATIONINFO`
  - `D3DDDICB_ALLOCATE` (resource-allocation fields)
  - `D3DDDICB_DEALLOCATE` (resource-free fields)
- CreateDevice wiring (where `pCallbacks` / `pUMCallbacks` live):
  - `D3D10DDIARG_CREATEDEVICE`
  - `D3D11DDIARG_CREATEDEVICE`
- device/context creation (kernel `hContext` + `hSyncObject` acquisition):
  - `D3DDDICB_CREATEDEVICE`
  - `D3DDDICB_CREATECONTEXT`
- DMA buffer acquisition/release (submission backing store):
  - `D3DDDICB_ALLOCATE`
  - `D3DDDICB_DEALLOCATE`
- DMA buffer acquisition: `D3DDDICB_GETCOMMANDINFO`
- submission: `D3DDDICB_RENDER`, `D3DDDICB_PRESENT`
- resource mapping (Map/Unmap callback structs): `D3DDDICB_LOCK`, `D3DDDICB_UNLOCK`
  - also probes the presence/layout of `D3DDDICB_LOCKFLAGS` and callback-table entries like `pfnLockCb`/`pfnUnlockCb`
- fence waits: `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT`, `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT`

Related reference docs:

- Submission callbacks + fences: `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`
- CreateResource allocation contract: `docs/graphics/win7-d3d10-11-umd-allocations.md`

## Build (Windows / Win7 UMD headers + VS toolchain)

From a VS2010 Developer Command Prompt (or a WDK build env that has `cl.exe` + the WDDM/D3D headers on the include path):

```cmd
cd drivers\aerogpu\tools\win7_wdk_probe
build_vs2010.cmd
```

Output:

- `drivers\\aerogpu\\tools\\win7_wdk_probe\\bin\\win7_wdk_probe.exe`

Run it:

```cmd
bin\\win7_wdk_probe.exe
```

## Notes

- This tool is intentionally not built by CI; it is a developer-side probe.
- It requires the Win7-era user-mode DDI headers (e.g. `d3d10umddi.h`, `d3d11umddi.h`, `d3dumddi.h`, `d3dkmthk.h`).
