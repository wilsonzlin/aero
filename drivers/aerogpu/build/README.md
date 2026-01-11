# AeroGPU (Win7) build guide (MSBuild + WDK 10)

AeroGPU is built with **MSBuild**:

- **KMD** (kernel-mode display miniport): `drivers/aerogpu/aerogpu_kmd.vcxproj`
  - Uses the WDK 10 MSBuild toolset (`WindowsKernelModeDriver10.0`)
- **UMDs** (user-mode drivers): Visual Studio C++ projects under `drivers/aerogpu/umd/`

The top-level entrypoint is:

- `drivers/aerogpu/aerogpu.sln`

## Recommended build flow (same as CI)

From repo root:

```powershell
pwsh ci/install-wdk.ps1
pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu
pwsh ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh ci/package-drivers.ps1
```

Outputs:

- `out/drivers/aerogpu/x86/` and `out/drivers/aerogpu/x64/` (built binaries)
- `out/packages/aerogpu/x86/` and `out/packages/aerogpu/x64/` (staged + signed driver packages)
- `out/artifacts/` (ZIP/ISO bundles)

## Direct MSBuild (optional)

If you already have MSBuild + WDK installed/configured, you can build directly:

```cmd
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
```

## Legacy CMD wrappers (optional)

This directory still contains convenience wrappers for local, repo-relative output staging:

- `build_all.cmd` builds into `drivers/aerogpu/build/out/win7/<arch>/<fre|chk>/{kmd,umd}` (free/checked map to Release/Debug).
- `stage_packaging_win7.cmd` copies binaries into `drivers/aerogpu/packaging/win7/` for manual signing/install workflows.

For the recommended CI-style flow (packages staged under `out/packages/` and signed under `out/`), see:

- `drivers/aerogpu/packaging/win7/README.md` (host-signed install flow via `trust_test_cert.cmd` + `pnputil`)

### Win7 DDI header mode (D3D10/11 UMD)

The real Win7 D3D10/11 UMD must be built against the official D3D10/11 user-mode DDI headers
(`d3d10umddi.h`, `d3d10_1umddi.h`, `d3d11umddi.h`, `d3dumddi.h`) provided by the
**Windows Driver Kit (WDK)** (Windows Kits).

The repo-local `drivers\\aerogpu\\build\\build_all.cmd` wrapper forces the WDK
header mode for the D3D10/11 UMD build by passing:

* `/p:AeroGpuUseWdkHeaders=1`

If a WinDDK-style root is detected (Win7-era `inc\\{api,ddk}` layout), it also passes:

* `/p:AeroGpuWdkRoot="<root>"`

If no WinDDK-style root is found, the build falls back to the toolchain's
standard include paths (common for Windows Kits 10+ installs).

On a typical modern WDK install, these headers live under:

* `C:\\Program Files (x86)\\Windows Kits\\10\\Include\\<ver>\\um\\d3d11umddi.h`
* `C:\\Program Files (x86)\\Windows Kits\\10\\Include\\<ver>\\shared\\d3d11umddi.h`

If you hit a build error about missing `d3d11umddi.h`, install the Windows WDK
(for CI we use the `Microsoft.WindowsWDK` winget package) and rebuild.

For a self-contained repo-only build (no WDK UMDDI headers installed), you can
build the D3D10/11 UMD with `/p:AeroGpuUseWdkHeaders=0`, which compiles against
the repo’s minimal compat ABI subset instead. That mode is intended for local
development and is not expected to be ABI-compatible with the real Win7 runtimes.
