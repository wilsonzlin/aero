# Windows 7 Paravirtual PCI Device / Driver Contract (Aero)

This document is a consolidated reference for Aero’s Windows 7 paravirtual PCI devices:

- virtio-blk (boot/storage)
- virtio-net
- virtio-snd
- virtio-input
- Aero GPU (WDDM)

For **virtio** devices, the definitive, binding interoperability contract is:

- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) (**Contract ID:** `AERO-W7-VIRTIO`)

`windows-device-contract.{md,json}` MUST remain consistent with `AERO-W7-VIRTIO`. If they ever disagree, **`AERO-W7-VIRTIO` wins**.

It exists to prevent “it boots on my machine” failures caused by silent PCI ID drift between:

- the emulator’s PCI device models,
- the Windows drivers/INFs that bind to them, and
- the Guest Tools installer logic (notably `CriticalDeviceDatabase` seeding for boot-critical storage).

The machine-readable companion manifest (for automation like Guest Tools) is:
**[`windows-device-contract.json`](./windows-device-contract.json)**.

## Contract rules (normative)

1. **PCI IDs are API.** If a value in the tables below changes, it is a breaking change.
2. For virtio devices, any PCI ID / transport change MUST be made in `AERO-W7-VIRTIO` first, then reflected here.
3. Any breaking change requires updating:
   - `docs/windows7-virtio-driver-contract.md` (virtio devices)
   - `docs/windows-device-contract.md`
   - `docs/windows-device-contract.json`
4. The Guest Tools installer must consume `windows-device-contract.json` (planned at minimum; implemented ideally) rather than hardcoding IDs in scripts.
5. Emulator device models must emit the IDs exactly as specified by the relevant contract, or Windows driver binding may fail.

## PCI ID allocations

### Virtio (paravirtual I/O devices)

Virtio devices use the virtio PCI vendor ID:

- `VIRTIO_PCI_VENDOR_ID = 0x1AF4`

Device IDs follow the virtio 1.0+ “modern” virtio-pci Device ID space:

```
pci_device_id = 0x1040 + virtio_device_id
```

Where `virtio_device_id` is the virtio device type ID (e.g. 1 = virtio-net, 2 = virtio-blk).

`AERO-W7-VIRTIO` v1 uses the modern ID space and a modern-only transport. Aero virtio devices MUST expose PCI Revision ID `0x01`; transitional virtio-pci IDs are out of scope for the contract.

The emulator emits the modern IDs by default.

Subsystem IDs are Aero-specific and are used as stable secondary identifiers:

- `subsystem_vendor_id = 0x1AF4`
- `subsystem_device_id` is defined by `AERO-W7-VIRTIO` (e.g. 0x0002 for virtio-blk, 0x0019 for virtio-snd).

### Aero GPU (WDDM)

Aero GPU is a custom PCI device (not virtio). It uses project-specific virtual PCI IDs:

- Primary HWID (new versioned ABI): `A3A0:0001` (`drivers/aerogpu/protocol/aerogpu_pci.h`)
  - Subsystem vendor/device: `A3A0:0001`
  - Class code: `03/00/00` (display / VGA)
  - Windows hardware IDs:
    - `PCI\VEN_A3A0&DEV_0001`
    - `PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0` (optional; only if the INF matches it)
Legacy bring-up ABI note:

- The Win7 KMD supports a legacy bring-up AeroGPU ABI (`PCI\VEN_1AED&DEV_0001`, protocol header `drivers/aerogpu/protocol/aerogpu_protocol.h`).
- The canonical in-tree Win7 AeroGPU INFs (`drivers/aerogpu/packaging/win7/{aerogpu.inf,aerogpu_dx11.inf}`) intentionally bind **only** to `PCI\VEN_A3A0&DEV_0001`.
- Installing against the legacy device model requires:
  - building the emulator with the legacy device model enabled (feature `emulator/aerogpu-legacy`), and
  - using a custom INF that matches `PCI\VEN_1AED&DEV_0001`.

> Note: these are virtual-only IDs used inside the guest; they are not required to be PCI-SIG allocated.
>
> Source of truth for AeroGPU PCI IDs:
> - Versioned ABI ID: `drivers/aerogpu/protocol/aerogpu_pci.h`
> - Legacy bring-up ABI ID: `drivers/aerogpu/protocol/aerogpu_protocol.h`
> - Guest Tools install/verify config: `guest-tools/config/devices.cmd`
> See also: `docs/abi/aerogpu-pci-identity.md` (context on why two IDs exist, and which emulator device models implement each ABI).
>
> Legacy note: some bring-up builds may still expose the legacy AeroGPU HWID `PCI\VEN_1AED&DEV_0001`.
> This is not the default device model, and the shipped Win7 AeroGPU INFs do **not** match it (by design).
> If you need it, use a custom INF and enable the legacy device model feature (`emulator/aerogpu-legacy`).

## Device table (normative)

All numeric values are shown as hexadecimal.

| Device | PCI Vendor:Device | Subsystem Vendor:Device | Class Code (base/sub/prog) | Windows service | INF name |
|---|---:|---:|---:|---|---|
| virtio-blk | `1AF4:1042` (REV `0x01`) | `1AF4:0002` | `01/00/00` (mass storage / SCSI) | `aerovblk` | `aerovblk.inf` |
| virtio-net | `1AF4:1041` (REV `0x01`) | `1AF4:0001` | `02/00/00` (network / ethernet) | `aerovnet` | `aerovnet.inf` |
| virtio-snd | `1AF4:1059` (REV `0x01`) | `1AF4:0019` | `04/01/00` (multimedia / audio) | `aeroviosnd` | `aero-virtio-snd.inf` |
| virtio-input (keyboard) | `1AF4:1052` (REV `0x01`) | `1AF4:0010` | `09/80/00` (input / other) | `aero_virtio_input` | `virtio-input.inf` |
| virtio-input (mouse) | `1AF4:1052` (REV `0x01`) | `1AF4:0011` | `09/80/00` (input / other) | `aero_virtio_input` | `virtio-input.inf` |
| Aero GPU | `A3A0:0001` | `A3A0:0001` | `03/00/00` (display / VGA) | `aerogpu` | `aerogpu.inf` |

Notes:

- Aero GPU INF path: `drivers/aerogpu/packaging/win7/aerogpu.inf`
- `aerogpu.inf` / `aerogpu_dx11.inf` bind to `PCI\VEN_A3A0&DEV_0001` (canonical / current ABI).
  - The Win7 KMD also supports a legacy `PCI\VEN_1AED&DEV_0001` bring-up device model, but installing against it requires a custom INF and enabling the legacy device model feature (`emulator/aerogpu-legacy`).
- `aerogpu_dx11.inf` is an optional alternative INF that binds to the same device IDs and additionally installs D3D10/11 user-mode components.

Compatibility note (non-canonical virtio PCI Device IDs):

Transitional virtio-pci IDs (e.g. `1AF4:1000`, `1AF4:1018`) are intentionally out of scope for `AERO-W7-VIRTIO` v1 and are not part of the Aero Win7 virtio contract.

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
- For contract version safety, matching SHOULD be revision-gated (`&REV_01`) and/or the driver should validate the PCI Revision ID at runtime.
- Matching MAY additionally be subsystem-qualified (`&SUBSYS_SSSSVVVV`) for safety, but then the emulator **must** keep those values stable.

Examples (illustrative) INF model entries:

```ini
[Manufacturer]
%MfgName% = AeroModels,NTx86,NTamd64

[AeroModels.NTamd64]
; aerovblk.inf
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042&REV_01
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01

; aerovnet.inf
%AeroVirtioNet.DeviceDesc% = AeroVirtioNet_Install, PCI\VEN_1AF4&DEV_1041&REV_01
%AeroVirtioNet.DeviceDesc% = AeroVirtioNet_Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01
```

### Boot-critical storage (`CriticalDeviceDatabase`)

If the boot disk is `virtio-blk`, the Guest Tools installer must ensure the storage driver service is treated as boot-critical by seeding:

`HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\<hardware-id>`

Where `<hardware-id>` is the hardware ID with backslashes replaced (commonly `PCI#VEN_...&DEV_...`). The exact set of keys written is installer-defined, but must be derived from the manifest.

The required mapping for virtio-blk is:

- `hardware ID` → `Service = aerovblk`

## Virtio transport contract

This section is intentionally “high level”: it specifies what the Windows drivers can rely on without locking down byte-exact BAR offsets.

The definitive, testable virtio transport/device behavior contract is:

- [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)

### PCI config space

For virtio devices listed in this contract (see `AERO-W7-VIRTIO` for the definitive virtio transport + feature contract):

- `vendor_id = 0x1AF4`
- `device_id` matches the table above
- `revision_id = 0x01` (Aero virtio contract v1; used for optional `REV_01` INF matching)
- `subsystem_vendor_id = 0x1AF4`
- `subsystem_device_id` matches `AERO-W7-VIRTIO`
- `class_code` matches the table above

### BARs / MMIO

Virtio devices MUST implement the **virtio-pci modern** programming interface as specified by `AERO-W7-VIRTIO`.

In particular, `AERO-W7-VIRTIO` v1 is **modern-only** (no legacy/transitional I/O port BAR).

Aero’s Windows drivers must:

- Use the **PCI capability-based MMIO regions** (common config / notify / ISR / device config).
- Not require legacy I/O-port operation for correctness.

> Note: `drivers/windows7/virtio-snd` contains a PortCls (WaveRT) audio driver skeleton that targets
> the **contract v1 modern** transport (PCI vendor capabilities + BAR0 MMIO) and uses **INTx** for
> interrupts. Treat `docs/windows7-virtio-driver-contract.md` as authoritative if this document ever
> disagrees.

### Interrupts

- Contract v1 requires **INTx** and the virtio ISR status register **read-to-ack** semantics.
- MSI-X is permitted but not required in contract v1.

## Feature negotiation / compatibility checks

### Virtio (all virtio-* devices)

Drivers must treat feature negotiation as the primary compatibility mechanism.

Minimum required virtio feature bit:

- `VIRTIO_F_VERSION_1` (bit 32) **must** be offered by the device and accepted by the driver.

Virtqueue format:

- Split virtqueues are required.
- Packed virtqueues must be treated as unsupported unless/until this contract is revised to require `VIRTIO_F_RING_PACKED` (bit 34).

Additional features may be used for performance, but must be treated as optional unless the relevant contract is updated to require them.

> Note: The legacy virtio-pci I/O-port `GuestFeatures` register only negotiates the low 32 bits. Drivers using the legacy interface must not depend on `VIRTIO_F_VERSION_1` (bit 32) being negotiated.

For `AERO-W7-VIRTIO` v1 specifically:

- `VIRTIO_F_RING_INDIRECT_DESC` (bit 28) is required and MUST be offered.
- `VIRTIO_F_RING_EVENT_IDX` (bit 29) is not offered.

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
- `pci_device_id` is the **canonical** emulator-presented Device ID for the device entry. If a driver/tool also supports additional compatibility IDs (for example, virtio transitional IDs), those MUST be represented in `hardware_id_patterns`.
- `hardware_id_patterns` are Windows PnP PCI hardware ID strings using backslashes (e.g. `"PCI\\VEN_1AF4&DEV_1042&REV_01"`).
  - They are intended to be **directly usable** in INF matching and transformable into registry key names for `CriticalDeviceDatabase`.
  - Tools must treat them as case-insensitive.

### Hardware ID pattern policy (normative)

`windows-device-contract.json` intentionally carries **both** strict and convenience Windows HWID patterns.

- **Strict patterns (automation / contract major version):**
  - Patterns that include the contract major version as a PCI Revision ID suffix: `&REV_RR`.
  - For `AERO-W7-VIRTIO` contract v1, this is `&REV_01`.
  - These patterns are intended for **automation** (Guest Tools generation, conformance checks), because they avoid accidentally matching non-contract devices (for example, QEMU’s default virtio PCI `REV_00`).
- **Convenience patterns (non-binding / compatibility):**
  - Patterns that omit `&REV_` and/or `&SUBSYS_`.
  - These are useful for broad matching (manual driver install, defensive `CriticalDeviceDatabase` seeding), but they may match non-Aero devices or future contract versions.

If there is any disagreement between `windows-device-contract.{md,json}` and the definitive virtio contract (`AERO-W7-VIRTIO`), **`AERO-W7-VIRTIO` is authoritative**.
