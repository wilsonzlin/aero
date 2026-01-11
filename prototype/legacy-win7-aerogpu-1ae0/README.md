# Legacy AeroGPU 1AE0 prototype (archived)

This directory contains a **deprecated** AeroGPU prototype stack that used PCI vendor ID **1AE0**.
It predates (and does **not** match) the supported AeroGPU device models/protocols in this
repository.

On Windows 7 x64, the archived 1AE0 Windows driver package is also **not WOW64-complete**
(it does not ship/install an x86 UMD), so 32-bit D3D9 apps will fail.

Supported AeroGPU ABIs in this repo:

- **Legacy bring-up ABI (1AED)**: `drivers/aerogpu/protocol/aerogpu_protocol.h` and the emulator
  device `crates/emulator/src/devices/pci/aerogpu_legacy.rs`.
- **Current versioned ABI (A3A0)**: `drivers/aerogpu/protocol/aerogpu_{pci,ring,cmd}.h` and the
  emulator device `crates/emulator/src/devices/pci/aerogpu.rs`.

Contents:

- `prototype/legacy-win7-aerogpu-1ae0/guest/windows/`: archived Windows 7 WDDM 1.1 + D3D9 driver stack targeting the 1AE0 prototype.
- The matching host-side 1AE0 device model is **not** part of the current emulator codebase.
  (Only the 1AED legacy and A3A0 versioned AeroGPU devices are supported.) If you need the 1AE0
  host-side prototype for archaeology, retrieve it from git history.

Do not use this prototype for new development.

For the supported Win7 driver package + install workflow, start at:
`drivers/aerogpu/packaging/win7/README.md` (and `drivers/aerogpu/build/stage_packaging_win7.cmd`).
