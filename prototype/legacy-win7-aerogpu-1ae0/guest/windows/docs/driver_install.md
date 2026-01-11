# AeroGPU Windows 7 Driver Install (deprecated 1AE0 prototype)

This directory contains a minimal Windows 7 WDDM 1.1 driver stack for the **deprecated**
AeroGPU prototype PCI device (vendor 1AE0).

It does **not** match the supported AeroGPU ABIs in this repository:

- Legacy bring-up ABI: vendor 1AED
- Current versioned ABI: vendor A3A0

- Kernel-mode display miniport (KMD): `prototype/legacy-win7-aerogpu-1ae0/guest/windows/wddm_kmd/aerogpu_kmd.sys`
- Direct3D 9 user-mode driver (UMD): `prototype/legacy-win7-aerogpu-1ae0/guest/windows/d3d9_umd/aerogpu_d3d9umd.dll`
- INF package: `prototype/legacy-win7-aerogpu-1ae0/guest/windows/inf/aerogpu.inf`

## Prerequisites

- Windows 7 SP1 (x86 or x64) VM/image.
- A host/emulator build that exposes the deprecated AeroGPU 1AE0 PCI device and implements the
  MMIO + command ring ABI described in
  `prototype/legacy-win7-aerogpu-1ae0/guest/windows/common/aerogpu_protocol.h`.
- Driver signing for test builds:
  - Either enable test signing in the guest (`bcdedit /set testsigning on` + reboot), or
  - Sign with a certificate trusted by the guest.

## Build (developer machine)

This repository includes Visual Studio/WDK MSBuild projects:

- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/wddm_kmd/aerogpu_kmd.vcxproj`
- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/d3d9_umd/aerogpu_d3d9umd.vcxproj`
- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/aerogpu_driver.sln`

Build with an installed WDK toolchain (WDK 10 is sufficient to target Windows 7):

```powershell
msbuild .\prototype\legacy-win7-aerogpu-1ae0\guest\windows\aerogpu_driver.sln /m /p:Configuration=Release /p:Platform=x64
```

Outputs:

- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/wddm_kmd/x64/Release/aerogpu_kmd.sys`
- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/d3d9_umd/x64/Release/aerogpu_d3d9umd.dll`

## Package for install

Copy the following files into a single folder on the Windows 7 guest (or into the disk image):

- `aerogpu.inf` (from `prototype/legacy-win7-aerogpu-1ae0/guest/windows/inf/`)
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

- PCI IDs: the default INF matches `PCI\\VEN_1AE0&DEV_0001` and also matches by display class code (`PCI\\CC_030000`) to simplify bringup. If the emulator uses different IDs, update both `prototype/legacy-win7-aerogpu-1ae0/guest/windows/common/aerogpu_protocol.h` and `prototype/legacy-win7-aerogpu-1ae0/guest/windows/inf/aerogpu.inf` together.
- This is a minimal bring-up stack. The UMD currently provides the escape submission plumbing and is expected to grow into a functional D3D9 driver that serializes D3D9 state + draws into the AeroGPU command stream.
