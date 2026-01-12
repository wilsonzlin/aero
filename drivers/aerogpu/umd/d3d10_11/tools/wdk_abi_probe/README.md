# Win7 D3D10/11 UMD ABI Probe (WDK headers)

This directory contains a **standalone console program** intended to be built against the **real Win7 D3D10/D3D11 UMD DDI headers** (`d3d10umddi.h`, `d3d10_1umddi.h`, `d3d11umddi.h`, `d3dumddi.h`, `d3dkmthk.h`) and prints ABI-critical information:

- `sizeof(...)` / selected `offsetof(...)` values for key DDI structs.
- The **expected x86 stdcall-decorated export names** for the UMD entrypoints:
  - `_OpenAdapter10@N`
  - `_OpenAdapter10_2@N`
  - `_OpenAdapter11@N`

The goal is to make header/version drift and calling-convention mismatches obvious *before* debugging a Win7 loader crash.

> This is tooling-only and is **not** part of the normal AeroGPU build.

## Build (WDK environment)

1. Install a WDK that provides the D3D10/11 UMD DDI headers (WinDDK 7600-era layout is known to work).
2. Open the appropriate build environment command prompt (so headers/libs are on `INCLUDE`/`LIB`):
   - “Windows 7 x86 Free Build Environment” (x86)
   - “Windows 7 x64 Free Build Environment” (x64)
3. Build and run:

```cmd
cd \path\to\repo\drivers\aerogpu\umd\d3d10_11\tools\wdk_abi_probe

rem x86
cl /nologo /W4 /EHsc d3d10_11_wdk_abi_probe.cpp /Fe:d3d10_11_wdk_abi_probe_x86.exe
d3d10_11_wdk_abi_probe_x86.exe

rem x64 (from the x64 build environment)
cl /nologo /W4 /EHsc d3d10_11_wdk_abi_probe.cpp /Fe:d3d10_11_wdk_abi_probe_x64.exe
d3d10_11_wdk_abi_probe_x64.exe
```

## What to check (strict)

### 1) Export decoration / `.def` stack sizes (x86)

Run the x86 probe and confirm that the reported `_OpenAdapter*@N` values match:

- `drivers/aerogpu/umd/d3d10_11/aerogpu_d3d10_x86.def`

These values are also checked at compile time in WDK builds via:

- `drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_wdk_abi_asserts.h`
- `drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_wdk_abi_expected.h`

Note: some WDK header sets may not expose the `PFND3D10DDI_OPENADAPTER` / `PFND3D11DDI_OPENADAPTER` typedefs. The probe will still print `_OpenAdapter*@N` values by falling back to the canonical `HRESULT(__stdcall*)(D3D10DDIARG_OPENADAPTER*)` signature so the `.def` file can be validated and the expected-header generator can run.

### 2) Verify UMD exports with `dumpbin`

From a Visual Studio Developer Command Prompt:

```cmd
dumpbin /exports aerogpu_d3d10.dll
dumpbin /exports aerogpu_d3d10_x64.dll
```

Confirm the export table contains **undecorated**:

- `OpenAdapter10`
- `OpenAdapter10_2`
- `OpenAdapter11`

And on x86, also contains the raw decorated forms:

- `_OpenAdapter10@N`
- `_OpenAdapter10_2@N`
- `_OpenAdapter11@N`

## Updating the checked-in expected ABI snapshot

The WDK build enables compile-time ABI conformance checks by default (see
`AEROGPU_D3D10_11_WDK_ABI_ENFORCE_EXPECTED` in `aerogpu_d3d10_11.vcxproj`).

If you intentionally upgrade/downgrade the toolchain/WDK headers used to build the
UMD, you must regenerate the expected snapshot so header drift is caught
deterministically.

1. Build and run the probe for **both architectures** (x86 and x64) in a WDK build
   environment (see the build steps above).
   * Save the output (for example: `out_x86.txt` and `out_x64.txt`).
2. (Optional) Regenerate the expected header automatically:

```cmd
python3 drivers/aerogpu/umd/d3d10_11/tools/wdk_abi_probe/gen_expected_header.py ^
  --x86 out_x86.txt ^
  --x64 out_x64.txt ^
  --out drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_wdk_abi_expected.h
```

If your probe output contains `offsetof(...) = <n/a>` entries for optional members, you can pass `--allow-na` to omit those macros from the generated header. (This applies to both the default curated output and `--all` mode.)

Optional: pass `--all` to emit *all* parsed `sizeof(...)`/`offsetof(...)` values from the probe output, not just the curated “high-value anchors”. This can be useful when expanding the set of ABI checks without hand-editing the header.

3. If you prefer, you can also copy the reported values manually into:
   - `drivers/aerogpu/umd/d3d10_11/src/aerogpu_d3d10_11_wdk_abi_expected.h`
4. Rebuild the UMD with `AeroGpuUseWdkHeaders=1` and ensure the build passes without
   any ABI assertion failures.

If the ABI assertions fail unexpectedly, common root causes are:

- Using a different Windows Kits / WDK version than CI.
- Using headers for a different target OS (e.g. Win10+) instead of Win7 (`_WIN32_WINNT`
  should be `0x0601` for Win7 builds).
- Accidental x86 calling convention drift (e.g. export compiled as `__cdecl` instead
  of `__stdcall`), causing `.def` decoration mismatches.

## Validating `.def` exports against expected stdcall stack sizes

The x86 `.def` file encodes the expected stdcall stack bytes in the decorated symbol
name:

- `_OpenAdapter10@N` means the export pops `N` bytes of arguments (`__stdcall`).

When you update the expected ABI snapshot, ensure that:

- The probe’s `_OpenAdapter*@N` values match `aerogpu_d3d10_x86.def`.
- `dumpbin /exports` on the built DLL shows both the undecorated exports and the
  raw decorated forms.
