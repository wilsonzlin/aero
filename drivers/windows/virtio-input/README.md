# virtio-input (Windows 7 SP1) KMDF HID minidriver

This directory contains a **KMDF** driver that registers itself as a **HID minidriver** using `HidRegisterMinidriver`, intended to bind to the Aero contract v1 virtio-input PCI device:

- Modern-only virtio-input: `PCI\VEN_1AF4&DEV_1052`

The driver implements the Aero Windows 7 virtio contract (see `docs/windows7-virtio-driver-contract.md`):

- virtio-pci **modern** transport (PCI capabilities + MMIO) with Revision ID `0x01`
- Feature negotiation includes `VIRTIO_F_VERSION_1` and `VIRTIO_F_RING_INDIRECT_DESC`
- Two split virtqueues (size 64 each):
  - queue 0: `eventq` (device → driver)
  - queue 1: `statusq` (driver → device, keyboard LEDs)
- Interrupts:
  - INTx (required)
  - MSI(-X) optional if available

In Aero contract v1, virtio-input is exposed as **two PCI functions** (keyboard and mouse). Each driver instance exposes only the matching HID report descriptor:

- keyboard function: ReportID `1` only
- mouse function: ReportID `2` only

## Building (WDK 7.1 recommended)

This project is set up for the classic **WDK 7.1 `build`** system, targeting the in-box Windows 7 KMDF runtime (**KMDF 1.9**).

1. Install the Windows Driver Kit that targets Windows 7 (WDK 7.1 / 7600.x).
2. Open the WDK build environment for:
   - `Win7 x86 Free Build Environment`
   - `Win7 x64 Free Build Environment`
3. From the build environment prompt:

   ```bat
   cd \path\to\repo\drivers\windows\virtio-input
   build -cZ
   ```

The output `virtioinput.sys` will be placed under the WDK `objfre_*` output directories.

## Installing on Windows 7 SP1

1. Ensure the virtio-input device is present. In QEMU, the device is commonly exposed as `virtio-input-pci` and shows the PCI hardware ID:
   - `PCI\VEN_1AF4&DEV_1052`
2. Build and **test-sign** the driver package (or enable test signing):
   - `bcdedit /set testsigning on`
   - Reboot
3. In Device Manager, locate the matching PCI device and use **Update Driver → Have Disk**, pointing at `virtio-input.inf`.

After installation, Device Manager should show the device using `virtioinput.sys`.

## Quick user-mode validation (hidtest)

For quick sanity checks of HID enumeration and the report IOCTL surface, a small Win32 console test tool lives under:

`drivers/windows/virtio-input/tools/hidtest/`

It can enumerate HID interfaces, print VID/PID + report descriptor length, read input reports via `ReadFile`, and optionally write the keyboard LED output report (ReportID=1) to exercise the `IOCTL_HID_WRITE_REPORT` path.

## Notes on KMDF versioning

The INF pins `KmdfLibraryVersion=1.9`, which is the in-box KMDF version for Windows 7. If you build with a newer WDK and target a newer KMDF version, you must ship the matching **KMDF coinstaller** in the driver package and update the INF accordingly.
