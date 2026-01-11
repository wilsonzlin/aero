# Guest Tools configuration

`devices.cmd` contains the device identifiers that the Guest Tools installer uses for:

- Boot-critical pre-seeding (`CriticalDeviceDatabase`) for `virtio-blk`
- Storage service name to mark as `BOOT_START`
- Expected PCI `VEN/DEV` pairs for Aero devices (virtio-blk/net/snd/input + Aero GPU)

This file is intended to be kept in sync with:

- The emulator's presented PCI IDs
- The `.inf` hardware ID matches
- The storage driver's service name (INF `AddService` name)

The `*_HWIDS` values are stored as a list of individually quoted hardware IDs to safely include `&` characters (e.g. `"PCI\VEN_1AF4&DEV_1001"`).

## AeroGPU PCI IDs

The AeroGPU Windows driver currently matches two `VEN/DEV` pairs:

- `PCI\VEN_A3A0&DEV_0001` — current versioned ABI
- `PCI\VEN_1AED&DEV_0001` — legacy bring-up ABI (still supported by the INF)

`devices.cmd` includes both in `AERO_GPU_HWIDS` so Guest Tools verification can recognize AeroGPU devices enumerated with either ID. The older 1AE0-family vendor ID is deprecated/stale and should not be used.
