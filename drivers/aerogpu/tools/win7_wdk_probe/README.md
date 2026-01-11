# AeroGPU Win7 WDK header/layout probe (D3D10/11 UMD submission + fences)

This is a small **Windows-only** console tool intended to be built in an environment that provides the
Win7-era D3D10/11 UMD DDI headers (typically from a Windows SDK/WDK install).

It exists to catch “wrong header version / wrong packing / wrong target arch” problems early by
printing `sizeof`/`offsetof` for the key structs involved in Win7 (WDDM 1.1) D3D10/D3D11 UMD:

- DMA buffer acquisition: `D3DDDICB_GETCOMMANDINFO`
- submission: `D3DDDICB_RENDER`, `D3DDDICB_PRESENT`
- fence waits: `D3DDDICB_WAITFORSYNCHRONIZATIONOBJECT`, `D3DKMT_WAITFORSYNCHRONIZATIONOBJECT`

Related reference doc (symbol-name contract): `docs/graphics/win7-d3d10-11-umd-callbacks-and-fences.md`

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
