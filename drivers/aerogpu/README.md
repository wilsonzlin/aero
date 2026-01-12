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

CI-produced packages (`out/packages/aerogpu/<arch>/`) stage the following by default:

- `aerogpu_dx11.inf` at the **package root** (canonical HWID; DX11-capable)
- `legacy/aerogpu.inf` under `legacy/` (legacy HWID; D3D9-only)

The D3D9-only canonical variant (`aerogpu.inf`) is not staged unless you customize `drivers/aerogpu/ci-package.json`.
The legacy DX11-capable INF (`legacy/aerogpu_dx11.inf`) exists in-tree (see `drivers/aerogpu/legacy/`) but is also not shipped in CI packages by default.

The canonical DX11-capable variant uses `aerogpu_dx11.inf` (adds D3D10/11 UMDs). It is staged in CI by default;
see `drivers/aerogpu/packaging/win7/README.md` for install notes.

## CI (GitHub Actions)

The canonical CI pipeline for Windows 7 drivers (including AeroGPU) is:

- `.github/workflows/drivers-win7.yml`

Artifacts produced by the workflow:

- `win7-drivers` (from `out/artifacts/`; installable ZIP/ISO bundles)
- `win7-drivers-signed-packages` (from `out/packages/**` + `out/certs/aero-test.cer`; raw INF/CAT staging)
- `aero-guest-tools` (Guest Tools ISO/zip/manifest built from the signed packages)

## CI packaging manifest (`ci-package.json`)

Catalog generation (`ci/make-catalogs.ps1`) is driven by `drivers/aerogpu/ci-package.json`:

- `infFiles` selects which INF(s) to stage at the **package root**. AeroGPU CI currently stages:
  - `packaging/win7/aerogpu_dx11.inf` (DX11-capable package; canonical binding)

  If you stage both `aerogpu.inf` and `aerogpu_dx11.inf`, Windows PnP should prefer `aerogpu_dx11.inf` (lower `FeatureScore`: `0xF7` vs `0xF8`),
  and `packaging/win7/install.cmd` also prefers `aerogpu_dx11.inf` when it is present at the package root.

  Legacy binding INFs are shipped separately under `legacy/` (see `drivers/aerogpu/legacy/`):
  - `legacy/aerogpu.inf` (D3D9-only; shipped in CI packages by default)
  - `legacy/aerogpu_dx11.inf` (D3D9 + D3D10/11; add to `additionalFiles` to ship it in CI packages)
- `wow64Files` lists x86 UMD DLLs that must be present in the x64 package during `Inf2Cat`.
  AeroGPU includes:
  - `aerogpu_d3d9.dll` (required for Win7 x64 WOW64 D3D9)
  - `aerogpu_d3d10.dll` (required for Win7 x64 WOW64 D3D10/11 when staging DX11-capable INFs)

Details: `docs/16-driver-packaging-and-signing.md`.

## Key docs / entrypoints

* Build + toolchain setup: `drivers/aerogpu/build/README.md`
* Win7 packaging/signing/install: `drivers/aerogpu/packaging/win7/README.md`
* Guest-side validation tests: `drivers/aerogpu/tests/win7/README.md`
* AeroGPU PCI IDs + ABI generations (new vs legacy): `docs/abi/aerogpu-pci-identity.md`
* Protocol / device ABI (canonical): `drivers/aerogpu/protocol/README.md`, `drivers/aerogpu/protocol/aerogpu_pci.h`, `drivers/aerogpu/protocol/aerogpu_ring.h`, `drivers/aerogpu/protocol/aerogpu_cmd.h`, `drivers/aerogpu/protocol/aerogpu_escape.h`
  * Note: the legacy bring-up ABI is `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h` (deprecated/retired; legacy PCI identity; feature-gated in the emulator).
* Debug control tool (bring-up): `drivers/aerogpu/tools/win7_dbgctl/README.md`

## Direct MSBuild (optional)

If you already have a working WDK/MSBuild environment, you can build the solution directly:

```cmd
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=x64
msbuild drivers\aerogpu\aerogpu.sln /m /p:Configuration=Release /p:Platform=Win32
```
