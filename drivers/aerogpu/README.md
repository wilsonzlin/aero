# AeroGPU (Windows 7) driver stack
This directory contains the in-tree **AeroGPU WDDM 1.1** driver stack for **Windows 7 SP1**:

* **KMD** (kernel-mode miniport): `aerogpu.sys`
* **UMDs** (user-mode display drivers):
  * **Required:** D3D9Ex UMD (`aerogpu_d3d9.dll` + `aerogpu_d3d9_x64.dll`)
  * **Optional:** D3D10/11 UMD (`aerogpu_d3d10.dll` + `aerogpu_d3d10_x64.dll`)

## Build (recommended / CI-like)

Build host: **Windows 10/11 x64** (WDK 10 + MSBuild).

From repo root:

```powershell
pwsh ci/install-wdk.ps1
pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu
pwsh ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh ci/package-drivers.ps1
```

Binaries are staged under:

- `out/drivers/aerogpu/x86/` and `out/drivers/aerogpu/x64/` (raw build outputs)
- `out/packages/aerogpu/x86/` and `out/packages/aerogpu/x64/` (INF+CAT staged packages)
- `out/artifacts/` (ZIP/ISO bundles)

## CI (GitHub Actions)

The canonical CI pipeline for Windows 7 drivers (including AeroGPU) is:

- `.github/workflows/drivers-win7.yml`

Artifacts produced by the workflow:

- `win7-drivers` (from `out/artifacts/`; installable ZIP/ISO bundles)
- `win7-drivers-signed-packages` (from `out/packages/**` + `out/certs/aero-test.cer`; raw INF/CAT staging)
- `aero-guest-tools` (Guest Tools ISO/zip/manifest built from the signed packages)

## Key docs / entrypoints

* Build + toolchain setup: `drivers/aerogpu/build/README.md`
* Win7 packaging/signing/install: `drivers/aerogpu/packaging/win7/README.md`
* Guest-side validation tests: `drivers/aerogpu/tests/win7/README.md`
* AeroGPU PCI IDs + ABI generations (new vs legacy): `docs/abi/aerogpu-pci-identity.md`
* Protocol / device ABI: `drivers/aerogpu/protocol/README.md` (see `aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
* Debug control tool (bring-up): `drivers/aerogpu/tools/win7_dbgctl/README.md`

## Direct MSBuild (optional)

If you already have a working WDK/MSBuild environment, you can build the solution directly:

```cmd
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
```
