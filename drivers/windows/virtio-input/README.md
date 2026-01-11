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

## Building

### CI / modern WDK (recommended)

CI builds this driver using **MSBuild + the Windows 10 WDK**, while still targeting **Windows 7** and the in-box **KMDF 1.9** runtime.

On a Windows machine:

```powershell
pwsh -File ci/install-wdk.ps1
pwsh -File ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers windows/virtio-input
```

Build outputs are staged under:

`out/drivers/windows/virtio-input/<arch>/`

### Legacy WDK 7.1 build.exe (optional)

If you still have WDK 7.1 installed, the classic `build.exe` flow is preserved via `sources`/`makefile`:

```bat
cd \path\to\repo\drivers\windows\virtio-input
build -cZ
```

The output `virtioinput.sys` will be placed under the WDK `objfre_*` output directories.

## Installing on Windows 7 SP1

1. Ensure the virtio-input PCI device is present.
   - In QEMU, virtio-input is typically exposed via:
     - `virtio-keyboard-pci`
     - `virtio-mouse-pci`
     - `virtio-tablet-pci`
   - For Aero contract v1 testing, pass `disable-legacy=on,x-pci-revision=0x01` so the device matches `AERO-W7-VIRTIO` v1
     (modern transport + `REV_01`), e.g.:
     - `-device virtio-keyboard-pci,disable-legacy=on,x-pci-revision=0x01`
     - `-device virtio-mouse-pci,disable-legacy=on,x-pci-revision=0x01`
   - In Device Manager, the device’s Hardware Ids should include at least:
     - `PCI\VEN_1AF4&DEV_1052`
     - (and more-specific forms like `...&SUBSYS_...&REV_01` depending on the device model)
2. Build and **test-sign** the driver package (or enable test signing):
   - `bcdedit /set testsigning on`
   - Reboot
3. In Device Manager, locate the matching PCI device and use **Update Driver → Have Disk**, pointing at `virtio-input.inf`.

After installation:

- Device Manager should show the device using `virtioinput.sys`.
- The installed driver service name should be `virtioinput` (check with `sc query virtioinput`).

## Quick user-mode validation (hidtest)

For quick sanity checks of HID enumeration and the report IOCTL surface, a small Win32 console test tool lives under:

`drivers/windows/virtio-input/tools/hidtest/`

It can enumerate HID interfaces, print VID/PID + report descriptor length, read input reports via `ReadFile`, and optionally write the keyboard LED output report (ReportID=1) to exercise the `IOCTL_HID_WRITE_REPORT` path.

## Notes on KMDF versioning

The INF pins `KmdfLibraryVersion=1.9`, which is the in-box KMDF version for Windows 7. If you build with a newer WDK and target a newer KMDF version, you must ship the matching **KMDF coinstaller** in the driver package and update the INF accordingly.
