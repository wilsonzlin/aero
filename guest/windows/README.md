# Legacy / prototype AeroGPU Windows 7 driver stack (`guest/windows/`)

This directory is a historical **tombstone** for an early AeroGPU Windows 7 (WDDM 1.1) prototype driver stack.

The prototype sources/INF were removed from the repo to avoid accidental use. If you need them for archaeology, use:

```bash
git log -- guest/windows
```

Notable differences from the supported stack:

- Bound to the legacy AeroGPU PCI ID family `PCI\VEN_1AE0&DEV_0001`
- Used a different guest↔host protocol/ABI than the current AeroGPU device model
- Was **not WOW64-complete** on Win7 x64 (no x86 UMD), so **32-bit D3D9 apps would fail**

## Canonical AeroGPU driver stack (supported)

The in-tree, supported AeroGPU stack lives under `drivers/aerogpu/` and uses the current PCI IDs
(`PCI\VEN_A3A0&DEV_0001` and `PCI\VEN_1AED&DEV_0001`).

The canonical source of truth for **Windows driver binding** (PCI IDs, service names, INF names) is:

- [`docs/windows-device-contract.md`](../../docs/windows-device-contract.md)
- [`docs/windows-device-contract.json`](../../docs/windows-device-contract.json)

Start with:

- Protocol / ABI: [`drivers/aerogpu/protocol/README.md`](../../drivers/aerogpu/protocol/README.md)
- Win7 packaging (INF/signing/install): [`drivers/aerogpu/packaging/win7/README.md`](../../drivers/aerogpu/packaging/win7/README.md)
  - Staging helper: [`drivers/aerogpu/build/stage_packaging_win7.cmd`](../../drivers/aerogpu/build/stage_packaging_win7.cmd)
- Guest installation media: [`guest-tools/`](../../guest-tools/)
