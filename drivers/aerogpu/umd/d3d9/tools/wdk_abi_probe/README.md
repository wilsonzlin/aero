# Win7 D3D9 UMD ABI Probe (WDK headers)

This directory contains a **standalone console program** that is intended to be built against the **real Windows 7 D3D UMD headers** (`d3d9umddi.h`, `d3dumddi.h`, `d3dkmthk.h`) and prints **ABI-critical** information:

- `sizeof(...)` and selected `offsetof(...)` values for key DDI structs/tables.
- The **expected x86 stdcall-decorated export names** for the UMD entrypoints (e.g. `_OpenAdapter@4` vs `_OpenAdapter@8`).

The goal is to make header/version drift and calling convention mismatches obvious *before* you try to boot a Win7 VM and debug a loader crash.

> Note: This is tooling-only and is **not** part of the normal AeroGPU build. The in-tree driver stack builds via
> `drivers\aerogpu\aerogpu.sln` (MSBuild + WDK10 toolset). This probe is only for validating ABI details
> against the canonical Win7 D3D UMD header set.

## Build (WDK environment)

1. Install a Windows WDK that provides the Win7-era D3D9 UMD DDI headers (`d3d9umddi.h`, `d3dumddi.h`, `d3dkmthk.h`).
   The Windows 7 WDK (7600-era) is known to include them.
2. Open the appropriate WDK build environment command prompt (so the headers are on the include path):
   - “Windows 7 x86 Free Build Environment” (for x86)
   - “Windows 7 x64 Free Build Environment” (for x64)
3. Build the probe with `cl.exe`:

```cmd
cd \path\to\repo\drivers\aerogpu\umd\d3d9\tools\wdk_abi_probe

rem x86
cl /nologo /W4 /EHsc d3d9_wdk_abi_probe.cpp /Fe:d3d9_wdk_abi_probe_x86.exe
d3d9_wdk_abi_probe_x86.exe

rem x64 (from the x64 WDK environment)
cl /nologo /W4 /EHsc d3d9_wdk_abi_probe.cpp /Fe:d3d9_wdk_abi_probe_x64.exe
d3d9_wdk_abi_probe_x64.exe
```

If compilation fails because a type is missing, you are likely compiling against a header set that does not match Win7/WDDM 1.1.

## What to check (strict checklist)

### 1) Export decoration / `.def` stack sizes (x86)

1. Run the **x86** probe and locate the section:

```
== Exported entrypoints (x86 stdcall decoration) ==
PFND3DDDI_OPENADAPTER  => _OpenAdapter@N
PFND3DDDI_OPENADAPTER2 => _OpenAdapter2@M
PFND3DDDI_OPENADAPTERFROMHDC  => _OpenAdapterFromHdc@K
PFND3DDDI_OPENADAPTERFROMLUID => _OpenAdapterFromLuid@L
```

2. Confirm that `drivers/aerogpu/umd/d3d9/aerogpu_d3d9_x86.def` matches:

- `OpenAdapter=_OpenAdapter@N`
- `OpenAdapter2=_OpenAdapter2@M`
- `OpenAdapterFromHdc=_OpenAdapterFromHdc@K`
- `OpenAdapterFromLuid=_OpenAdapterFromLuid@L`

If any of `N/M/K/L` differ, the UMD will load but the runtime will call the wrong symbol (or corrupt the stack).

### 2) Struct/table layout sanity

Run the probe for **x86** and **x64** and save the output. Confirm the following *do not change unexpectedly* across toolchain/header revisions:

- `sizeof(D3DDDIARG_OPENADAPTER)` and key offsets like `pAdapterFuncs` / `pAdapterCallbacks`
- `sizeof(D3D9DDI_ADAPTERFUNCS)` and offsets for the “anchor” function pointers (Close/GetCaps/CreateDevice)
- `sizeof(D3D9DDI_DEVICEFUNCS)` and offsets for the subset your UMD fills (CreateResource/Present/Flush/etc)
- `sizeof(D3DDDI_DEVICECALLBACKS)` and offsets for the callbacks your UMD uses (Allocate/Submit/Render/etc)
- Submission structs used to pass DMA buffer pointers (`D3DDDIARG_CREATECONTEXT`, `D3DDDIARG_SUBMITCOMMAND`)

### 3) Verify UMD exports with `dumpbin`

From a Developer Command Prompt:

```cmd
dumpbin /exports aerogpu_d3d9.dll
```

Confirm:

1. `OpenAdapter` and `OpenAdapter2` exports exist.
2. On **x86**, the decorated exports match the probe output:
   - `_OpenAdapter@N`
   - `_OpenAdapter2@M`

Tip: the x86 `aerogpu_d3d9_x86.def` intentionally exports both the undecorated and decorated names; verify both show up.

## Notes

- This probe is a **tooling-only** artifact and is not part of the normal repo build.
- Its output is intended to be used to keep the UMD’s exported prototypes, `.def` mappings, and structure layouts in sync with the Win7 D3D UMD headers.

### Optional: compile-time ABI asserts in the UMD

If you want ABI drift to fail the build immediately, there is an optional header:

- `drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_wdk_abi_asserts.h`

In your **WDK build only**:

1. Define `AEROGPU_D3D9_USE_WDK_DDI`.
2. Include the header in one `.cpp` file.
3. Define one or more `AEROGPU_D3D9_WDK_ABI_EXPECT_*` macros (using values captured from this probe).

The header is inert unless `AEROGPU_D3D9_USE_WDK_DDI` is defined, so it will not affect repo-local builds that do not have the WDK installed.
