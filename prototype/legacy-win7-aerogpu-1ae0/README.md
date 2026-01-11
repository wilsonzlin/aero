# Legacy AeroGPU 1AE0 prototype (archived)

This directory contains a **deprecated** AeroGPU prototype stack that used PCI vendor ID **1AE0**.
It predates (and does **not** match) the supported AeroGPU device models/protocols in this
repository.

Supported AeroGPU ABIs in this repo:

- **Legacy bring-up ABI (1AED)**: `drivers/aerogpu/protocol/aerogpu_protocol.h` and the emulator
  device `crates/emulator/src/devices/pci/aerogpu_legacy.rs`.
- **Current versioned ABI (A3A0)**: `drivers/aerogpu/protocol/aerogpu_{pci,ring,cmd}.h` and the
  emulator device `crates/emulator/src/devices/pci/aerogpu.rs`.

Contents:

- `guest/windows/`: archived Windows 7 WDDM 1.1 + D3D9 driver stack targeting the 1AE0 prototype.
- Host-side toy device model (not wired into the current emulator):  
  `crates/aero-emulator/src/devices/aerogpu_1ae0_prototype/` (gated behind the
  `aerogpu-1ae0-prototype` crate feature).

Do not use this prototype for new development.
