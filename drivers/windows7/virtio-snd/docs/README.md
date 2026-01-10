# virtio-snd (Windows 7) driver skeleton

This directory contains an initial, clean-room **WDM kernel-mode function driver** for a PCI virtio-snd device.

The driver currently:

- Loads on Windows 7 SP1 (x86/x64)
- Attaches to the PCI PDO
- Handles basic PnP (START/STOP/REMOVE) and maps BAR resources (best-effort)

It **does not** yet implement virtio-pci transport, virtqueues, or any PortCls miniports, so it will not expose audio endpoints yet.

## Building (WDK 7600 / WDK 7.1)

1. Open a WDK build environment:
   - **Windows 7 x86 Free Build Environment** (x86)
   - **Windows 7 x64 Free Build Environment** (x64)
   - Checked builds work as well (DBG output is enabled only in checked builds).

2. Build from the driver root:

```
cd drivers\windows7\virtio-snd
build -cZ
```

The output will be under `objfre_win7_*` (or `objchk_win7_*` for checked builds).

## Installing (development/testing)

1. Copy `virtiosnd.sys` next to `inf\virtio-snd.inf`.
2. Use Device Manager → Update Driver → "Have Disk..." and point to the `inf` directory.

### PCI Hardware IDs

`inf/virtio-snd.inf` currently matches:

- `PCI\VEN_1AF4&DEV_1059` (assumed virtio 1.x sound; `0x1040 + VIRTIO_ID_SOUND`)
- `PCI\VEN_1AF4&DEV_1018` (assumed transitional/legacy sound; `0x1000 + (VIRTIO_ID_SOUND - 1)`)

These IDs are included as a starting point; confirm against the virtio specification / device configuration used by the emulator.
