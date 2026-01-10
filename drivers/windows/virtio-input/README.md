# virtio-input (Windows 7 SP1) KMDF HID minidriver skeleton

This directory contains a minimal **KMDF** driver that registers itself as a **HID minidriver** using `HidRegisterMinidriver`, intended to bind to the PCI virtio-input device:

- Modern-only virtio-input: `PCI\VEN_1AF4&DEV_1052`
- Transitional virtio-input: `PCI\VEN_1AF4&DEV_1011`

The current implementation is a **skeleton**:

- Implements WDF boilerplate (`EvtDriverDeviceAdd`, power/PnP callbacks, default queue).
- Implements the HIDCLASS-facing IOCTL surface (`IOCTL_HID_GET_*`) and exposes keyboard + mouse collections (Report IDs 1 and 2).
- Implements `EvtIoInternalDeviceControl` for the core report IOCTLs:
  - `IOCTL_HID_READ_REPORT` is routed by Report ID / collection handle context so keyboard and mouse reports do not get mixed.
  - `IOCTL_HID_WRITE_REPORT` validates keyboard LED output reports (ReportID=1) and ignores unknown IDs.
- Does **not** yet parse the virtio transport, negotiate features, or deliver real input reports.

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
