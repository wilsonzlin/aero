# Legacy AeroGPU Win7 INF bindings (VEN_1AED)

This folder contains **legacy** INF files that bind the AeroGPU Win7 driver stack to the
deprecated AeroGPU PCI identity (**VEN_1AED&DEV_0001**).

The canonical (non-legacy) AeroGPU PCI IDs are defined in
`drivers/aerogpu/protocol/aerogpu_pci.h` (**VEN_A3A0&DEV_0001**) and are what the primary INF
(`aerogpu.inf`) matches.

This directory provides two legacy-binding INF variants:

- `legacy/aerogpu.inf` (D3D9-only; included in CI driver packages)
- `legacy/aerogpu_dx11.inf` (D3D9 + optional D3D10/11 UMDs; not included in CI packages by default)

This folder is intended to be shipped alongside the canonical driver package so Guest Tools can
install against either device model:

- Canonical: `PCI\VEN_A3A0&DEV_0001`
- Legacy (deprecated): `PCI\VEN_1AED&DEV_0001`

Notes on on-disk layout:

- These INFs expect the driver binaries (`aerogpu.sys`, `aerogpu_d3d9*.dll`, `aerogpu.cat`, etc) to
  live in the **parent directory** of this `legacy/` folder. This matches:
  - CI driver package layout (`out/packages/aerogpu/<arch>/legacy/`), and
  - packaged Guest Tools layout (`drivers/<arch>/aerogpu/legacy/`).

For repo/dev installs that stage `drivers/aerogpu/packaging/win7/`, continue using the legacy INFs
under `drivers/aerogpu/packaging/win7/legacy/`.
