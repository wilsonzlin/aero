# Legacy AeroGPU Win7 INF bindings (VEN_1AED)

This folder contains **legacy** INF files that bind the AeroGPU Win7 driver stack to the
deprecated AeroGPU PCI identity (**VEN_1AED&DEV_0001**).

The canonical (non-legacy) AeroGPU PCI IDs are defined in
`drivers/aerogpu/protocol/aerogpu_pci.h` (**VEN_A3A0&DEV_0001**) and are what the canonical AeroGPU
INFs match:

- `aerogpu_dx11.inf` (DX11-capable; CI/Guest Tools default)
- `aerogpu.inf` (D3D9-only; useful for bring-up/regression)

This directory provides two legacy-binding INF variants:

- `legacy/aerogpu.inf` (D3D9-only; included in CI driver packages)
- `legacy/aerogpu_dx11.inf` (D3D9 + optional D3D10/11 UMDs; not included in CI packages by default)

To stage the legacy DX11-capable INF in CI, add `legacy/aerogpu_dx11.inf` to `drivers/aerogpu/ci-package.json`
(`additionalFiles`) and ensure the D3D10/11 UMDs are staged (see `drivers/aerogpu/packaging/win7/README.md`).

This folder is shipped alongside the canonical driver package so installs can target either
device model:

- Canonical: `PCI\VEN_A3A0&DEV_0001`
- Legacy (deprecated): `PCI\VEN_1AED&DEV_0001`

Note: the canonical Windows device contract / Guest Tools verification scripts intentionally
validate only the canonical (`VEN_A3A0`) AeroGPU identity. The legacy bring-up device model is
supported for optional compatibility/regression testing but is not part of the default Guest Tools
hardware-ID contract.

Notes on on-disk layout:

- These INFs expect the driver binaries (`aerogpu.sys`, `aerogpu_d3d9*.dll`, `aerogpu.cat`, etc) to
  live in the **parent directory** of this `legacy/` folder. This matches:
  - CI driver package layout (`out/packages/aerogpu/<arch>/legacy/`), and
  - packaged Guest Tools layout (`drivers/<arch>/aerogpu/legacy/`).

For repo/dev installs that stage `drivers/aerogpu/packaging/win7/`, continue using the legacy INFs
under `drivers/aerogpu/packaging/win7/legacy/`.
