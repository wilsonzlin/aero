# Aero Guest Tools (Windows 7)

Guest Tools is the Windows-side bundle that makes Aero usable and fast:

- Paravirtual device drivers (virtio-blk/net/snd/input + Aero GPU)
- Installer logic (PnP driver install + boot-critical storage seeding)
- Optional userland utilities (time sync, clipboard, etc.)

## Source of truth: device contracts

Do **not** hardcode PCI IDs, subsystem IDs, or service names in installer scripts.

For **virtio** devices, the definitive contract is:

- [`docs/windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO`)

For automation (Guest Tools installer), the machine-readable manifest is:

- [`docs/windows-device-contract.json`](../windows-device-contract.json)

`windows-device-contract.json` MUST remain consistent with `AERO-W7-VIRTIO` for virtio devices.

The Guest Tools installer should consume `windows-device-contract.json` to:

1. Install the correct INF for each device.
2. Pre-seed `HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase` for boot-critical storage
   (virtio-blk) using the `driver_service_name` and `hardware_id_patterns` in the manifest.

If emulator-side PCI IDs change without updating the contract, Windows may fail to bind drivers or
may bluescreen on boot due to missing boot-critical storage drivers.
