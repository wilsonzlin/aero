# Guest Tools configuration

`devices.cmd` contains the device identifiers that the Guest Tools installer uses for:

- Boot-critical pre-seeding (`CriticalDeviceDatabase`) for `virtio-blk`
- Storage service name to mark as `BOOT_START`
- Expected PCI hardware IDs for Aero devices (virtio-blk/net/snd/input + Aero GPU)

`devices.cmd` is generated from the canonical machine-readable device contract:

- `docs/windows-device-contract.json`

The `*_HWIDS` values are stored as a list of individually quoted hardware IDs to safely include
`&` characters (e.g. `"PCI\VEN_1AF4&DEV_1042&REV_01"`).
