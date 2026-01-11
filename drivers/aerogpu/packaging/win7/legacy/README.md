# Legacy AeroGPU Win7 INF bindings (VEN_1AED)

This folder contains **legacy** INF files that bind the AeroGPU Win7 driver stack to the
deprecated AeroGPU PCI identity (**VEN_1AED&DEV_0001**).

The canonical (non-legacy) AeroGPU PCI IDs are defined in
`drivers/aerogpu/protocol/aerogpu_pci.h` (**VEN_A3A0&DEV_0001**) and are what the main
`drivers/aerogpu/packaging/win7/*.inf` files match.

Use these legacy INFs only when running an emulator build that intentionally exposes the
legacy AeroGPU device model/ABI.

This requires building the emulator with the legacy device model enabled:

`cargo build --locked -p emulator --features emulator/aerogpu-legacy`

## Install (repo/dev layout)

These INFs are stored under `packaging/win7/legacy/` so the default driver package does not accidentally bind to
`VEN_1AED`. They are designed to be used with the **same binaries staged into the parent directory**
(`drivers/aerogpu/packaging/win7/`).

From repo root (after building the driver), stage the packaging directory, sign it, then install using the legacy INF:

```bat
drivers\aerogpu\build\stage_packaging_win7.cmd fre x64

cd drivers\aerogpu\packaging\win7
sign_test.cmd
install.cmd legacy\aerogpu.inf
:: or (DX11-capable variant)
install.cmd legacy\aerogpu_dx11.inf
```

`install.cmd` runs `verify_umd_registration.cmd` after install to sanity-check UMD DLL placement and the key HKR registry values.
