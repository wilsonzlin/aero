# Legacy / prototype AeroGPU Windows 7 driver stack (VEN_1AE0)

This directory (`guest/windows/`) contains an **old prototype** Windows 7 (WDDM 1.1) AeroGPU driver stack.
It is kept for reference only and is **not** the supported/canonical AeroGPU implementation.

## Why this exists

This prototype stack:

- Binds to the legacy AeroGPU PCI ID family `PCI\VEN_1AE0&DEV_0001` (see `inf/aerogpu.inf`).
- Uses a **different guest↔host ABI** (see `common/aerogpu_protocol.h`) than the current AeroGPU protocol.

If you are working on D3D9Ex / DWM compatibility or the current AeroGPU device model, **do not start here**.

## Canonical AeroGPU driver stack (supported)

The in-tree, supported AeroGPU stack lives under `drivers/aerogpu/` and uses the current PCI IDs
(`PCI\VEN_A3A0&DEV_0001` and `PCI\VEN_1AED&DEV_0001`) plus the protocol headers under
`drivers/aerogpu/protocol/`.

Start with:

- Protocol / ABI: [`drivers/aerogpu/protocol/README.md`](../../drivers/aerogpu/protocol/README.md)
- Win7 packaging (INF/signing/install): [`drivers/aerogpu/packaging/win7/README.md`](../../drivers/aerogpu/packaging/win7/README.md)
- Guest installation media: [`guest-tools/`](../../guest-tools/)
