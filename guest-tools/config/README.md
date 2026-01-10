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
