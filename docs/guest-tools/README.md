# Aero Guest Tools (Windows 7)

Guest Tools is the Windows-side bundle that makes Aero usable and fast:

- Paravirtual device drivers (virtio-blk/net/snd/input + Aero GPU)
- Installer logic (PnP driver install + boot-critical storage seeding)
- Optional userland utilities (time sync, clipboard, etc.)

## Source of truth: Windows device contract

Do **not** hardcode PCI IDs, subsystem IDs, or service names in installer scripts.

All device/driver binding information is specified in:

- [`docs/windows-device-contract.md`](../windows-device-contract.md) (human-readable contract)
- [`docs/windows-device-contract.json`](../windows-device-contract.json) (machine-readable manifest)

The Guest Tools installer should consume `windows-device-contract.json` to:

1. Install the correct INF for each device.
2. Pre-seed `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase` for boot-critical storage
   (virtio-blk) using the `driver_service_name` and `hardware_id_patterns` in the manifest.

If emulator-side PCI IDs change without updating the contract, Windows may fail to bind drivers or
may bluescreen on boot due to missing boot-critical storage drivers.

