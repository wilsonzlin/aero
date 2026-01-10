# AeroGPU Win7 D3D9Ex UMD (User-Mode Display Driver)

This directory contains the **AeroGPU Direct3D 9Ex user-mode display driver** (UMD) for Windows 7 SP1.

The UMD’s job is to:

1. expose the D3D9 adapter/device entrypoints expected by the Win7 D3D9 runtime, and
2. translate D3D9 DDI calls into the **AeroGPU high-level command stream** (`drivers/aerogpu/protocol/aerogpu_cmd.h`).

The kernel-mode driver (KMD) is responsible for accepting submissions and providing the emulator with the side-band **allocation list** so the command stream can reference resources by allocation-list index (no patch-location list / relocations).

## Build

This project is intended to be built in a Windows/WDK environment as a DLL for both x86 and x64:

- `aerogpu_d3d9.dll` (x86 / Win32)
- `aerogpu_d3d9_x64.dll` (x64)

Build files:

- Visual Studio project: `aerogpu_d3d9_umd.vcxproj`
- Exports: `aerogpu_d3d9_umd.def` (`OpenAdapter`, `OpenAdapter2`)

### Notes

- The code in `include/aerogpu_d3d9_umd.h` includes a tiny “compat” subset of the D3D9 DDI types so the core translation code is self-contained in this repository. When integrating into a real Win7 WDK build, wire the entrypoints to the real WDK D3D9 DDI headers and structures (the exported names are the key ABI contract).
- Logging is done via `OutputDebugStringA` (view with DebugView/WinDbg) and is intentionally lightweight.

## Install / Register (INF)

The D3D9 runtime loads the display driver’s UMD(s) based on registry values written by the display driver INF. The exact INF layout depends on the KMD packaging, but the critical part is that the service installs a WDDM display driver and registers the **user-mode driver DLL name**.

In the Win7 packaging INFs in this repo (`drivers/aerogpu/packaging/win7/aerogpu.inf` and `aerogpu_dx11.inf`), the D3D9 UMD is registered via `InstalledDisplayDrivers` (base name, no extension):

```inf
[AeroGPU_Device_AddReg_x86]
HKR,,InstalledDisplayDrivers,0x00010000,"aerogpu_d3d9"

[AeroGPU_Device_AddReg_amd64]
HKR,,InstalledDisplayDrivers,0x00010000,"aerogpu_d3d9_x64"
HKR,,InstalledDisplayDriversWow,0x00010000,"aerogpu_d3d9"
```

Then ensure the DLLs are copied into the correct system directories during installation:

- x86 Windows: `System32\aerogpu_d3d9.dll`
- x64 Windows:
  - `System32\aerogpu_d3d9_x64.dll` (64-bit)
  - `SysWOW64\aerogpu_d3d9.dll` (32-bit)

After installation, reboot (or restart the display driver) and confirm:

1. DWM starts without falling back to Basic mode.
2. Debug output shows `OpenAdapter2` and subsequent command submissions.

## Supported feature subset (bring-up)

The initial implementation focuses on the minimum D3D9Ex feature set needed for:

- DWM/Aero composition
- a basic triangle app (VB/IB, shaders, textures, alpha blend, present)

Unsupported states are handled defensively; unknown state enums are accepted and forwarded as generic “set render/sampler state” commands so the emulator can decide how to interpret them.
