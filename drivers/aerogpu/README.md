# AeroGPU (Windows 7) driver stack
This directory contains the in-tree **AeroGPU WDDM 1.1** driver stack for **Windows 7 SP1**:

* **KMD** (kernel-mode miniport): `aerogpu.sys`
* **UMDs** (user-mode display drivers):
  * **Required:** D3D9Ex UMD (`aerogpu_d3d9.dll` + `aerogpu_d3d9_x64.dll`)
  * **Optional:** D3D10/11 UMD (`aerogpu_d3d10.dll` + `aerogpu_d3d10_x64.dll`)

## Quickstart (build + install on a Win7 VM)

Build host: **Windows 10/11 x64** (WDK 7.1 + VS2022/MSBuild).

1. Build the drivers (from repo root):

```cmd
drivers\aerogpu\build\build_all.cmd fre
```

2. Stage the Win7 packaging folder (copies binaries next to the `.inf` files):

```cmd
drivers\aerogpu\build\stage_packaging_win7.cmd fre x64
```

3. In the Win7 VM, run as Administrator:

```cmd
cd drivers\aerogpu\packaging\win7
sign_test.cmd
install.cmd
```

If you built and staged the optional D3D10/11 UMDs, install via:

```cmd
install.cmd aerogpu_dx11.inf
```

## Key docs / entrypoints

* Build + toolchain setup: `drivers/aerogpu/build/README.md`
* Win7 packaging/signing/install: `drivers/aerogpu/packaging/win7/README.md`
* Guest-side validation tests: `drivers/aerogpu/tests/win7/README.md`
* Protocol / device ABI: `drivers/aerogpu/protocol/README.md` (see `aerogpu_pci.h`, `aerogpu_ring.h`, `aerogpu_cmd.h`)
* Legacy bring-up ABI header (legacy MMIO + ring; still used by the legacy KMD path): `drivers/aerogpu/protocol/aerogpu_protocol.h`
* Debug control tool (bring-up): `drivers/aerogpu/tools/win7_dbgctl/README.md`
