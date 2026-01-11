# Windows 7 Paravirtual PCI Device / Driver Contract (Aero)

This document is the **single source of truth** for the device-model ↔ Windows-driver contract for Aero’s Windows 7 paravirtual devices:

- virtio-blk (boot/storage)
- virtio-net
- virtio-snd
- virtio-input
- Aero GPU (WDDM)

It exists to prevent “it boots on my machine” failures caused by silent PCI ID drift between:

- the emulator’s PCI device models,
- the Windows drivers/INFs that bind to them, and
- the Guest Tools installer logic (notably `CriticalDeviceDatabase` seeding for boot-critical storage).

The machine-readable companion manifest is: **[`windows-device-contract.json`](./windows-device-contract.json)**.

## Contract rules (normative)

1. **PCI IDs are API.** If a value in the tables below changes, it is a breaking change.
2. Any breaking change requires **updating both**:
   - `docs/windows-device-contract.md`
   - `docs/windows-device-contract.json`
3. The Guest Tools installer must **consume** `windows-device-contract.json` (planned at minimum; implemented ideally) rather than hardcoding IDs in scripts.
4. Emulator device models must emit the IDs exactly as specified here (including subsystem IDs and class codes), or Windows driver binding may fail.

## PCI ID allocations

### Virtio (paravirtual I/O devices)

Virtio devices use the virtio PCI vendor ID:

- `VIRTIO_PCI_VENDOR_ID = 0x1AF4`

Device IDs follow the virtio 1.0+ “modern” virtio-pci ID range:

```
pci_device_id = 0x1040 + virtio_device_type
```

For compatibility with older stacks/driver packages, Aero drivers/INFs MAY also match the
“transitional” virtio ID range commonly used by hypervisors:

```
pci_device_id = 0x1000 + (virtio_device_type - 1)
```

The emulator emits the modern IDs by default.

Subsystem IDs are used to provide a stable secondary identifier:

- `subsystem_vendor_id = 0x1AF4`
- `subsystem_device_id = virtio_device_type` (e.g. 0x0002 for virtio-blk)

### Aero GPU (WDDM)

Aero GPU is a custom PCI device (not virtio). It uses project-specific virtual PCI IDs:

- Primary HWID (new versioned ABI): `A3A0:0001` (`drivers/aerogpu/protocol/aerogpu_pci.h`)
- Secondary/legacy HWID (legacy bring-up ABI): `1AED:0001` (`drivers/aerogpu/protocol/aerogpu_protocol.h`)

> Note: these are virtual-only IDs used inside the guest; they are not required to be PCI-SIG allocated.
>
> Source of truth for AeroGPU PCI IDs: `drivers/aerogpu/protocol/aerogpu_pci.h` and `guest-tools/config/devices.cmd`.
> See also: `docs/abi/aerogpu-pci-identity.md` (context on why two IDs exist, and which emulator device models implement each ABI).

## Device table (normative)

All numeric values are shown as hexadecimal.

| Device | PCI Vendor:Device | Subsystem Vendor:Device | Class Code (base/sub/prog) | Windows service | INF name |
|---|---:|---:|---:|---|---|
| virtio-blk | `1AF4:1042` | `1AF4:0002` | `01/00/00` (mass storage / SCSI) | `aerovioblk` | `aero-virtio-blk.inf` |
| virtio-net | `1AF4:1041` | `1AF4:0001` | `02/00/00` (network / ethernet) | `aerovionet` | `aero-virtio-net.inf` |
| virtio-snd | `1AF4:1059` | `1AF4:0019` | `04/01/00` (multimedia / audio) | `aeroviosnd` | `aero-virtio-snd.inf` |
| virtio-input | `1AF4:1052` | `1AF4:0012` | `09/80/00` (input / other) | `aerovioinput` | `aero-virtio-input.inf` |
| Aero GPU | `A3A0:0001` | `A3A0:0001` | `03/00/00` (display / VGA) | `AeroGPU` | `aerogpu.inf` |

Notes:

- Aero GPU INF path: `drivers/aerogpu/packaging/win7/aerogpu.inf`
- `aerogpu.inf` also matches the legacy AeroGPU HWID `1AED:0001`.
- `aerogpu_dx11.inf` is an optional alternative INF if shipping D3D10/11 user-mode components.

## Windows hardware IDs and driver binding

Windows PnP hardware IDs for PCI devices are formatted like:

- `PCI\VEN_VVVV&DEV_DDDD&SUBSYS_SSSSVVVV&REV_RR`
- `PCI\VEN_VVVV&DEV_DDDD&SUBSYS_SSSSVVVV`
- `PCI\VEN_VVVV&DEV_DDDD&REV_RR`
- `PCI\VEN_VVVV&DEV_DDDD`

Where:

- `VVVV` = vendor ID (4 hex digits)
- `DDDD` = device ID (4 hex digits)
- `SSSS` = subsystem device ID (4 hex digits)
- `RR` = revision ID (2 hex digits)

### Binding requirements (normative)

- Each driver INF must match **at least** the vendor/device pair: `PCI\VEN_xxxx&DEV_yyyy`.
- Matching MAY additionally be revision-gated (`&REV_RR`) and/or subsystem-qualified (`&SUBSYS_SSSSVVVV`) for safety, but then the emulator **must** keep those values stable.

Example (illustrative) INF model entries:

```ini
; aero-virtio-blk.inf
[Manufacturer]
%MfgName% = AeroModels,NTx86,NTamd64

[AeroModels.NTamd64]
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4
```

### Boot-critical storage (`CriticalDeviceDatabase`)

If the boot disk is `virtio-blk`, the Guest Tools installer must ensure the storage driver service is treated as boot-critical by seeding:

`HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\<hardware-id>`

Where `<hardware-id>` is the hardware ID with backslashes replaced (commonly `PCI#VEN_...&DEV_...`). The exact set of keys written is installer-defined, but must be derived from the manifest.

The required mapping for virtio-blk is:

- `hardware ID` → `Service = aerovioblk`

## Virtio transport contract

This section is intentionally “high level”: it specifies what the Windows drivers can rely on without locking down byte-exact BAR offsets.

### PCI config space

For virtio devices listed in this contract:

- `vendor_id = 0x1AF4`
- `device_id` matches the table above
- `subsystem_vendor_id = 0x1AF4`
- `subsystem_device_id = virtio_device_type`
- `revision_id = 0x01` (Aero virtio contract v1; used for optional `REV_01` INF matching)
- `class_code` matches the table above

### BARs / MMIO vs I/O ports

Virtio devices must present a **virtio-pci modern** programming interface to the guest driver.

Implementation options (emulator-side):

1. **Transitional virtio-pci** (recommended for interoperability):
   - Provide the legacy I/O-port BAR (for legacy drivers) **and**
   - Provide the virtio 1.x PCI capability-based MMIO regions (for modern drivers).
2. **Modern-only virtio-pci**:
   - No legacy I/O-port BAR.
   - Only the virtio PCI capabilities and their MMIO regions.

Whichever option is chosen, Aero’s Windows drivers must:

- Use the **PCI capability-based MMIO regions** (common config / notify / ISR / device config).
- Not require legacy I/O-port operation for correctness.

### Interrupts

- MSI-X is recommended.
- INTx must work as a fallback (at least during early bring-up), unless the platform explicitly disables it.

## Feature negotiation / compatibility checks

### Virtio (all virtio-* devices)

Drivers must treat feature negotiation as the primary compatibility mechanism.

Minimum required virtio feature bit:

- `VIRTIO_F_VERSION_1` (bit 32) **must** be offered by the device and accepted by the driver.

Virtqueue format:

- Split virtqueues are required.
- Packed virtqueues must be treated as unsupported unless/until this contract is revised to require `VIRTIO_F_RING_PACKED` (bit 34).

Additional features may be used for performance (device- and ring-level), but must be treated as optional unless this contract is updated to require them. In particular, drivers should negotiate these opportunistically:

- `VIRTIO_RING_F_INDIRECT_DESC` (bit 28)
- `VIRTIO_RING_F_EVENT_IDX` (bit 29)

### Aero GPU

Aero GPU exposes an MMIO register block (see BAR contract below) containing a version field.

- Driver must read a `VERSION` register and refuse to start if the **major** version is unsupported.
- Minor/patch versions may add optional capabilities gated by a `CAPS` bitmask.

## Aero GPU BAR contract (high level)

- **BAR0 (MMIO):** required. Contains:
  - identification + version registers
  - command submission doorbells
  - interrupt/status registers
  - optional shared-memory window descriptors (if used)
- Additional BARs are optional and must be discoverable via version/caps.

## Manifest (`windows-device-contract.json`)

The JSON manifest is the canonical interface for automation (Guest Tools installer, CI checks, etc.).

At minimum, each device entry contains:

- `pci_vendor_id`
- `pci_device_id`
- `hardware_id_patterns`
- `driver_service_name`
- `inf_name`
- `virtio_device_type` (only for virtio devices)

Consumers must not assume any particular device ordering and must tolerate new device entries being added over time.

### Manifest field conventions (normative)

- `pci_vendor_id` / `pci_device_id` are hex strings with `0x` prefix (e.g. `"0x1AF4"`).
- `hardware_id_patterns` are Windows PnP PCI hardware ID strings using backslashes (e.g. `"PCI\\VEN_1AF4&DEV_1042"`).
  - They are intended to be **directly usable** in INF matching and transformable into registry key names for `CriticalDeviceDatabase`.
  - Tools must treat them as case-insensitive.
