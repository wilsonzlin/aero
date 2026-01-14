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

Editor note: the JSON manifests include a `$schema` reference to
[`windows-device-contract.schema.json`](./windows-device-contract.schema.json) for improved
editor/CI feedback when editing the contract. In-repo tooling ignores `$schema` and performs its
own validation (see `cargo run -p device-contract-validator --locked`).

For the optional **virtio-win** packaging flow, see: [Virtio-win packaging variant](#virtio-win-packaging-variant-non-normative).

## Virtio-win packaging variant (non-normative)

When building Guest Tools media from **upstream virtio-win** driver bundles (`viostor`, `netkvm`, etc),
use the dedicated contract override:

- [`windows-device-contract-virtio-win.json`](./windows-device-contract-virtio-win.json)

This keeps Aero’s **PCI IDs / HWID patterns** (modern-only virtio IDs + `REV_01`) while changing only the
Windows **service names / INF names** to match virtio-win (`viostor`, `netkvm`, `vioinput`, `viosnd`).

The virtio-win contract file is intended to be passed to the Guest Tools packager wrapper:

- `ci/package-guest-tools.ps1 -WindowsDeviceContractPath docs/windows-device-contract-virtio-win.json`

`windows-device-contract-virtio-win.json` is **generated** from the canonical contract:

- Preferred one-shot regen/check (covers all derived artifacts): `python3 scripts/regen-windows-device-contract-artifacts.py [--check]`
- Generator (standalone): `python3 scripts/generate-windows-device-contract-virtio-win.py`
- CI drift check (standalone): `python3 scripts/generate-windows-device-contract-virtio-win.py --check`

Only `driver_service_name` and `inf_name` should differ for virtio devices; PCI IDs and `hardware_id_patterns`
must remain identical to `windows-device-contract.json`.

## Contract rules (normative)

1. **PCI IDs are API.** If a value in the tables below changes, it is a breaking change.
2. For virtio devices, any PCI ID / transport change MUST be made in `AERO-W7-VIRTIO` first, then reflected here.
3. Any breaking change requires updating:
   - `docs/windows7-virtio-driver-contract.md` (virtio devices)
   - `docs/windows-device-contract.md`
   - `docs/windows-device-contract.json`
4. The Guest Tools installer must consume `windows-device-contract.json` rather than hardcoding IDs in scripts.
    - In this repo, `guest-tools/config/devices.cmd` is generated from the manifest (see `scripts/generate-guest-tools-devices-cmd.py`).
    - Preferred one-shot regen/check (covers all derived artifacts): `python3 scripts/regen-windows-device-contract-artifacts.py [--check]`
    - Drift check (no rewrite): `python3 scripts/ci/gen-guest-tools-devices-cmd.py --check`
    - Full contract drift check (contract + Guest Tools + packaging specs + INFs + emulator IDs):
      `cargo run -p device-contract-validator --locked`
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

- Primary HWID (versioned ABI): `A3A0:0001` (`drivers/aerogpu/protocol/aerogpu_pci.h`)
  - Subsystem vendor/device: `A3A0:0001`
  - Class code: `03/00/00` (display / VGA)
  - Windows hardware IDs:
    - `PCI\VEN_A3A0&DEV_0001`
    - `PCI\VEN_A3A0&DEV_0001&SUBSYS_0001A3A0` (optional; only if the INF matches it)

Current canonical machine note:

- The canonical `aero_machine::Machine` reserves `00:07.0` for AeroGPU (`PCI\VEN_A3A0&DEV_0001`).
- When `MachineConfig::enable_aerogpu=true`, the machine exposes the AeroGPU PCI identity at
  `00:07.0` (`A3A0:0001`) with BAR0 regs + BAR1 VRAM aperture for stable Windows driver binding. In
  `aero_machine` today BAR1 is backed by a dedicated VRAM buffer and the legacy VGA window
  (`0xA0000..0xBFFFF`) is VRAM-backed with permissive legacy VGA port decode (see
  `docs/16-aerogpu-vga-vesa-compat.md`). Note: the in-tree Win7 AeroGPU driver treats the adapter as
  system-memory-backed (no dedicated WDDM VRAM segment); BAR1 exists for VGA/VBE compatibility and
  is outside the WDDM memory model. BAR0 implements a minimal MMIO surface:
  - ring/fence transport (submission decode/capture + fence-page/IRQ plumbing). Default bring-up
    behavior can complete fences without executing the command stream; browser/WASM runtimes can
    enable an out-of-process “submission bridge” (`Machine::aerogpu_drain_submissions` +
    `Machine::aerogpu_complete_fence`) so the GPU worker can execute submissions and report fence
    completion, and native builds can optionally install a feature-gated in-process headless wgpu
    backend (`Machine::aerogpu_set_backend_wgpu`), and
  - scanout0/vblank register storage so the guest can program scanout and the Win7 stack can use
    vblank timing primitives (see `drivers/aerogpu/protocol/vblank.md`).

  Shared device-side building blocks (regs/ring/executor + reusable PCI wrapper) live in
  `crates/aero-devices-gpu`. A legacy sandbox integration surface remains in `crates/emulator`. Real
  **command execution** is provided by host-side executors/backends (GPU worker execution via the
  submission bridge, or optional in-process backends in native/test builds). See:
  [`docs/graphics/status.md`](./graphics/status.md).
- Boot display is provided by `aero_gpu_vga` (VGA + Bochs VBE) when `MachineConfig::enable_vga=true`.
  When the PC platform is enabled, the VBE LFB MMIO aperture is mapped directly at the configured
  LFB base inside the PCI MMIO window (no dedicated PCI VGA stub).

Legacy bring-up ABI note:

- The Win7 KMD still has a compatibility path for the deprecated legacy bring-up AeroGPU ABI (the legacy `"ARGP"` device model; see `docs/abi/aerogpu-pci-identity.md` for the exact PCI identity).
- The canonical in-tree Win7 AeroGPU INFs (`drivers/aerogpu/packaging/win7/{aerogpu.inf,aerogpu_dx11.inf}`) intentionally bind **only** to `PCI\VEN_A3A0&DEV_0001`.
- Installing against the legacy device model requires:
  - building the emulator with the legacy device model enabled (feature `emulator/aerogpu-legacy`), and
  - using the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/`.

> Note: these are virtual-only IDs used inside the guest; they are not required to be PCI-SIG allocated.
>
> Source of truth for AeroGPU PCI IDs:
> - Versioned ABI ID: `drivers/aerogpu/protocol/aerogpu_pci.h`
> - Legacy bring-up ABI ID (deprecated): `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`
> - Guest Tools install/verify config: `guest-tools/config/devices.cmd`
> See also: `docs/abi/aerogpu-pci-identity.md` (context on why two IDs exist, and which emulator device models implement each ABI).
>
> Legacy note: some bring-up builds may still expose the deprecated legacy AeroGPU HWID (the `"ARGP"` device model). This is not
> the default device model, and the shipped Win7 AeroGPU INFs do **not** match it (by design). If you need it, use the legacy
> INFs under `drivers/aerogpu/packaging/win7/legacy/` and enable the legacy device model feature (`emulator/aerogpu-legacy`).
>
> Older prototypes used a different (now deprecated) AeroGPU PCI identity; those stacks are archived and
> must not be treated as the Windows driver binding contract.

## Device table (normative)

All numeric values are shown as hexadecimal.

| Device | PCI Vendor:Device | Subsystem Vendor:Device | Class Code (base/sub/prog) | Windows service | INF name |
|---|---:|---:|---:|---|---|
| virtio-blk | `1AF4:1042` (REV `0x01`) | `1AF4:0002` | `01/00/00` (mass storage / SCSI) | `aero_virtio_blk` | `aero_virtio_blk.inf` |
| virtio-net | `1AF4:1041` (REV `0x01`) | `1AF4:0001` | `02/00/00` (network / ethernet) | `aero_virtio_net` | `aero_virtio_net.inf` |
| virtio-snd | `1AF4:1059` (REV `0x01`) | `1AF4:0019` | `04/01/00` (multimedia / audio) | `aero_virtio_snd` | `aero_virtio_snd.inf` |
| virtio-input (keyboard) | `1AF4:1052` (REV `0x01`) | `1AF4:0010` | `09/80/00` (input / other) | `aero_virtio_input` | `aero_virtio_input.inf` |
| virtio-input (mouse) | `1AF4:1052` (REV `0x01`) | `1AF4:0011` | `09/80/00` (input / other) | `aero_virtio_input` | `aero_virtio_input.inf` |
| Aero GPU | `A3A0:0001` | `A3A0:0001` | `03/00/00` (display / VGA) | `aerogpu` | `aerogpu_dx11.inf` |

Notes:

  - Aero GPU INF path: `drivers/aerogpu/packaging/win7/aerogpu_dx11.inf` (canonical CI-staged variant)
  - `aerogpu.inf` / `aerogpu_dx11.inf` bind to `PCI\VEN_A3A0&DEV_0001` (canonical / current ABI).
  - `aerogpu.inf` is a D3D9-only alternative INF that binds to the same device IDs (useful for bring-up/regression).
  - The deprecated legacy bring-up AeroGPU device model requires the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/` and enabling the legacy device model feature (`emulator/aerogpu-legacy`).
  - Windows service names are case-insensitive. The canonical AeroGPU INFs install the `aerogpu` service (`AddService = aerogpu, ...`).
    The legacy INFs under `drivers/aerogpu/packaging/win7/legacy/` use different casing (for example `AeroGPU`), but this contract normalizes the name to `aerogpu`.
  - `virtio-input` is exposed as a **single multi-function PCI device** (multiple PCI functions on the same slot):
    - keyboard = function 0 and **must** set the multifunction bit (`header_type = 0x80`) so guests enumerate additional functions
    - mouse = function 1
    - (Optional) tablet = function 2 (absolute pointer / `EV_ABS`; subsystem ID `1AF4:0012`; binds via `drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf`)

Compatibility note (transitional virtio PCI Device IDs):

`AERO-W7-VIRTIO` v1 is modern-only (emulator-visible PCI IDs + `REV_01`). The older virtio-pci **transitional** PCI
Device ID space exists for ecosystem compatibility, but is intentionally out of scope for the contract (the emulator is
not required to emit transitional IDs, and Aero’s in-tree contract-v1 driver INFs do not bind to them).

Transitional virtio-pci IDs are out of scope for `AERO-W7-VIRTIO` v1.

For a virtio device type `virtio_device_type` (as recorded in `windows-device-contract.json`), the corresponding
transitional virtio-pci Device ID is:

```text
pci_device_id_transitional = 0x1000 + (virtio_device_type - 1)
```

For reference, this repository records the corresponding transitional IDs as `pci_device_id_transitional` in
`docs/windows-device-contract{,-virtio-win}.json`:

 - virtio-net: `1AF4:1000`
 - virtio-blk: `1AF4:1001`
 - virtio-input: `1AF4:1011`
 - virtio-snd: `1AF4:1018`

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

- Each driver INF must match at least one hardware ID that includes the vendor/device pair: `PCI\VEN_xxxx&DEV_yyyy`
  (potentially with additional qualifiers like `&REV_..` and/or `&SUBSYS_...`).
- For contract version safety, **virtio** driver INFs MUST be revision-gated (`&REV_01`) to avoid binding to non-contract devices.
  Virtio drivers SHOULD also validate the PCI Revision ID at runtime (defense in depth).
- Matching MAY additionally be subsystem-qualified (`&SUBSYS_SSSSVVVV`) for safety, but then the emulator **must** keep those values stable.

Examples (illustrative) INF model entries:

```ini
; aero_virtio_blk.inf
[Manufacturer]
%MfgName% = AeroModels,NTx86,NTamd64

[AeroModels.NTamd64]
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042&REV_01
%AeroVirtioBlk.DeviceDesc% = AeroVirtioBlk_Install, PCI\VEN_1AF4&DEV_1042&SUBSYS_00021AF4&REV_01

; aero_virtio_net.inf
%AeroVirtioNet.DeviceDesc% = AeroVirtioNet_Install, PCI\VEN_1AF4&DEV_1041&REV_01
%AeroVirtioNet.DeviceDesc% = AeroVirtioNet_Install, PCI\VEN_1AF4&DEV_1041&SUBSYS_00011AF4&REV_01

; aero_virtio_snd.inf
%AeroVirtioSnd.DeviceDesc% = AeroVirtioSnd_Install, PCI\VEN_1AF4&DEV_1059&REV_01
  
; aero_virtio_input.inf (virtio-input is a multi-function device: keyboard + mouse)
; Canonical keyboard/mouse INF is SUBSYS-only: it includes only the Aero keyboard/mouse subsystem-qualified contract v1 HWIDs,
; for distinct Device Manager naming:
%AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01
%AeroVirtioMouse.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01

; Legacy filename alias `virtio-input.inf` (checked in disabled-by-default as `virtio-input.inf.disabled`)
; - Exists for compatibility with workflows/tools that still reference `virtio-input.inf` instead of `aero_virtio_input.inf`.
; - Allowed to diverge from the canonical INF only in the models sections (`[Aero.NTx86]` / `[Aero.NTamd64]`) to add an opt-in
;   strict revision-gated generic fallback model line (no `SUBSYS`):
%AeroVirtioInput.DeviceDesc%    = AeroVirtioInput_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&REV_01
; - Outside those models sections, from the first section header (`[Version]`) onward, it is expected to remain byte-for-byte
;   identical to `aero_virtio_input.inf` (banner/comments may differ; see `drivers/windows7/virtio-input/scripts/check-inf-alias.py`).
; - Enabling the alias does change HWID matching behavior (it enables the strict generic fallback binding above).
; - Install only one basename at a time (avoid duplicate overlapping INFs that can cause confusing driver selection).

; aero_virtio_tablet.inf (optional tablet / absolute pointer)
; Note: this SUBSYS-qualified HWID is more specific, so it wins over the generic fallback when both packages are installed.
%AeroVirtioTablet.DeviceDesc%   = AeroVirtioTablet_Install.NTamd64, PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01
```

### Boot-critical storage (`CriticalDeviceDatabase`)

If the boot disk is `virtio-blk`, the Guest Tools installer must ensure the storage driver service is treated as boot-critical by seeding:

`HKLM\SYSTEM\CurrentControlSet\Control\CriticalDeviceDatabase\<hardware-id>`

Where `<hardware-id>` is the hardware ID with backslashes replaced (commonly `PCI#VEN_...&DEV_...`). The exact set of keys written is installer-defined, but must be derived from the manifest.

The required mapping for virtio-blk is:

- `hardware ID` → `Service = aero_virtio_blk`

## Virtio transport contract

This section is intentionally “high level”: it specifies what the Windows drivers can rely on without locking down byte-exact BAR offsets.

The definitive, testable virtio transport/device behavior contract is:

- [`docs/windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md)

### PCI config space

For virtio devices listed in this contract (see `AERO-W7-VIRTIO` for the definitive virtio transport + feature contract):

- `vendor_id = 0x1AF4`
- `device_id` matches the table above
- `revision_id = 0x01` (Aero virtio contract v1; used for revision-gated `REV_01` INF matching)
- `subsystem_vendor_id = 0x1AF4`
- `subsystem_device_id` matches `AERO-W7-VIRTIO`
- `class_code` matches the table above

### BARs / MMIO

Virtio devices MUST implement the **virtio-pci modern** programming interface as specified by `AERO-W7-VIRTIO`.

In particular, `AERO-W7-VIRTIO` v1 is **modern-only** (no legacy/transitional I/O port BAR).

Aero’s Windows drivers must:

- Use the **PCI capability-based MMIO regions** (common config / notify / ISR / device config).
- Not require legacy I/O-port operation for correctness.

> Note: `drivers/windows7/virtio-snd` contains a PortCls (WaveRT) audio driver that targets the
> **contract v1 modern** transport (PCI vendor capabilities + BAR0 MMIO) and supports the contract
> v1 **INTx** baseline, with optional **MSI/MSI-X** when Windows grants message interrupts (INF opt-in).
> Treat `docs/windows7-virtio-driver-contract.md` as authoritative if this document ever disagrees.

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
- `pci_device_id_transitional` (virtio devices only)
- `hardware_id_patterns`
- `driver_service_name`
- `inf_name`
- `virtio_device_type` (only for virtio devices)

Consumers must not assume any particular device ordering and must tolerate new device entries being added over time.

### Manifest field conventions (normative)

- `pci_vendor_id` / `pci_device_id` are hex strings with `0x` prefix (e.g. `"0x1AF4"`).
- `pci_device_id` is the **canonical** emulator-presented Device ID for the device entry (for virtio devices, this is the modern ID space).
- For virtio devices, `pci_device_id_transitional` records the corresponding virtio-pci transitional Device ID (for reference / compatibility).
- `hardware_id_patterns` are Windows PnP PCI hardware ID strings using backslashes (e.g. `PCI\VEN_1AF4&DEV_1042&REV_01`).
  - In `windows-device-contract.json` these are stored as JSON strings, so backslashes are escaped (the literal form is
    `"PCI\\VEN_1AF4&DEV_1042&REV_01"`).
  - They are intended to be **directly usable** in INF matching and transformable into registry key names for `CriticalDeviceDatabase`.
  - Tools must treat them as case-insensitive.
  - Each entry MUST include the canonical `VEN_xxxx&DEV_yyyy` pair for the device as declared by `pci_vendor_id` / `pci_device_id`
    (do not mix multiple Vendor/Device families inside a single device entry).
  - Consumers MUST treat this list as an **unordered set** (do not assume ordering); tools like Guest Tools may sort/deduplicate patterns before use.
  - For virtio devices, this list is **modern-only** under `AERO-W7-VIRTIO` v1 (it must not contain transitional `DEV_10xx` IDs).
    Transitional IDs are tracked separately in `pci_device_id_transitional`.

### Hardware ID pattern policy (normative)

`windows-device-contract.json` intentionally carries **both** strict and convenience Windows HWID patterns.

- **Strict patterns (automation / contract major version):**
  - Patterns that include the contract major version as a PCI Revision ID suffix: `&REV_RR`.
  - For `AERO-W7-VIRTIO` contract v1, this is `&REV_01`.
  - These patterns are intended for **automation** (Guest Tools generation, conformance checks), because they avoid accidentally matching non-contract devices (for example, QEMU’s default virtio PCI `REV_00`).
  - The manifest SHOULD include at least the canonical Vendor/Device + Revision form: `PCI\VEN_VVVV&DEV_DDDD&REV_RR` (some in-tree INFs, e.g. virtio-snd, are revision-gated on Vendor/Device only).
- **Preferred strict patterns (device identity / avoiding false positives):**
  - When available, automation SHOULD prefer the most specific form: `&SUBSYS_SSSSVVVV&REV_RR`.
  - This is especially important for devices with multiple contract instances under the same Vendor/Device ID (notably `virtio-input` keyboard vs mouse).
- **Convenience patterns (non-binding / compatibility):**
  - Patterns that omit `&REV_` and/or `&SUBSYS_`.
  - These are useful for broad matching (manual driver install, defensive `CriticalDeviceDatabase` seeding), but they may match non-Aero devices or future contract versions.

If there is any disagreement between `windows-device-contract.{md,json}` and the definitive virtio contract (`AERO-W7-VIRTIO`), **`AERO-W7-VIRTIO` is authoritative**.
