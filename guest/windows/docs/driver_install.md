# AeroGPU Windows 7 Driver Install (WDDM 1.1 + D3D9)

This directory contains a minimal Windows 7 WDDM 1.1 driver stack for the AeroGPU paravirtual PCI device:

- Kernel-mode display miniport (KMD): `guest/windows/wddm_kmd/aerogpu_kmd.sys`
- Direct3D 9 user-mode driver (UMD): `guest/windows/d3d9_umd/aerogpu_d3d9umd.dll`
- INF package: `guest/windows/inf/aerogpu.inf`

## Prerequisites

- Windows 7 SP1 (x86 or x64) VM/image.
- A host/emulator build that exposes the AeroGPU PCI device and implements the MMIO + command ring ABI described in `guest/windows/common/aerogpu_protocol.h`.
- Driver signing for test builds:
  - Either enable test signing in the guest (`bcdedit /set testsigning on` + reboot), or
  - Sign with a certificate trusted by the guest.

## Build (developer machine)

This repository includes Visual Studio/WDK MSBuild projects:

- `guest/windows/wddm_kmd/aerogpu_kmd.vcxproj`
- `guest/windows/d3d9_umd/aerogpu_d3d9umd.vcxproj`
- `guest/windows/aerogpu_driver.sln`

Build with an installed WDK toolchain (WDK 10 is sufficient to target Windows 7):

```powershell
msbuild .\guest\windows\aerogpu_driver.sln /m /p:Configuration=Release /p:Platform=x64
```

Outputs:

- `guest/windows/wddm_kmd/x64/Release/aerogpu_kmd.sys`
- `guest/windows/d3d9_umd/x64/Release/aerogpu_d3d9umd.dll`

## Package for install

Copy the following files into a single folder on the Windows 7 guest (or into the disk image):

- `aerogpu.inf` (from `guest/windows/inf/`)
- `aerogpu_kmd.sys`
- `aerogpu_d3d9umd.dll`

If you generate a catalog (`aerogpu.cat`) via `inf2cat`, include it as well.

## Install on Windows 7

1. Ensure the AeroGPU device is present in Device Manager under **Display adapters** (it may show as *Standard VGA Graphics Adapter* initially).
2. Right click → **Update Driver Software...**
3. **Browse my computer for driver software**
4. Point it at the folder containing `aerogpu.inf`
5. Accept the unsigned/test-signed warning (depending on guest configuration).

## Notes

- PCI IDs: the default INF matches `PCI\\VEN_1AE0&DEV_0001` and also matches by display class code (`PCI\\CC_030000`) to simplify bringup. If the emulator uses different IDs, update both `guest/windows/common/aerogpu_protocol.h` and `guest/windows/inf/aerogpu.inf` together.
- This is a minimal bring-up stack. The UMD currently provides the escape submission plumbing and is expected to grow into a functional D3D9 driver that serializes D3D9 state + draws into the AeroGPU command stream.

