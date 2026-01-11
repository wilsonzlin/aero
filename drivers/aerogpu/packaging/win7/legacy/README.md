# Legacy AeroGPU Win7 INF bindings (VEN_1AED)

This folder contains **legacy** INF files that bind the AeroGPU Win7 driver stack to the
deprecated AeroGPU PCI identity (**VEN_1AED&DEV_0001**).

The canonical (non-legacy) AeroGPU PCI IDs are defined in
`drivers/aerogpu/protocol/aerogpu_pci.h` (**VEN_A3A0&DEV_0001**) and are what the main
`drivers/aerogpu/packaging/win7/*.inf` files match.

Use these legacy INFs only when running an emulator build that intentionally exposes the
legacy AeroGPU device model/ABI.

This requires building the emulator with the legacy device model enabled:

`cargo build -p emulator --features emulator/aerogpu-legacy`
