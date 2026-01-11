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

The D3D10/11 UMD can optionally be built against the Win7-era D3D10/11 DDI headers
(`d3d10umddi.h`, `d3d10_1umddi.h`, `d3d11umddi.h`) from a Windows SDK/WDK install.

The repo-local `drivers\\aerogpu\\build\\build_all.cmd` wrapper detects a WDK root via
`WINDDK` (or `WDK_ROOT`) and, when present, passes these MSBuild properties for the
D3D10/11 UMD build:

* `/p:AeroGpuUseWdkHeaders=1`
* `/p:AeroGpuWdkRoot="%WINDDK%"`

The project expects the DDI headers under:

* `%WINDDK%\\inc\\api`
* `%WINDDK%\\inc\\ddk`

To disable WDK headers (portable/stub mode), omit the properties or pass
`/p:AeroGpuUseWdkHeaders=0`.
