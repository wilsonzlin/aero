# ADR 0005: AeroGPU PCI IDs and ABI (canonical + deprecation plan)

## Context

AeroGPU spans multiple layers (emulator device model, guest kernel-mode driver, guest user-mode driver, installers/INFs, and docs). Over time, multiple “almost the same” AeroGPU PCI identities and ABIs have accumulated:

- `drivers/aerogpu/protocol/{aerogpu_pci.h,aerogpu_ring.h,aerogpu_cmd.h,aerogpu_escape.h}` defines a **versioned** PCI/MMIO + ring protocol and uses the **A3A0** PCI vendor ID.
- `docs/windows-device-contract.{md,json}` documents the canonical Windows-facing AeroGPU binding contract (**A3A0**) and is checked in CI; older experiments (a retired cmd/completion-ring prototype ABI) used different PCI identities/ABIs and have been retired (see `docs/legacy/experimental-gpu-command-abi.md`).
- Legacy stacks exist with different IDs/ABIs, notably:
  - **1AED**: legacy BAR0 MMIO ABI (and associated INF matching).
  - **1AE0**: archived Win7 prototype stack (see `prototype/legacy-win7-aerogpu-1ae0/`).
  - Retired cmd/completion-ring prototype ABI, used for early host-side experiments and must not be used for the WDDM AeroGPU device.

This drift is costly because **PCI IDs and guest↔host ABIs are API**:

- Windows driver binding depends on stable PCI identity (including subsystem IDs).
- Guest↔host ABI drift causes silent runtime failure modes (device enumerates but driver cannot start, or starts and misprograms registers).
- Documentation drift causes “cargo cult” reintroduction of legacy IDs/ABIs.

We need a single canonical definition of AeroGPU’s PCI identity and ABI, plus an explicit plan to migrate away from legacy IDs/ABIs without accidentally reintroducing them.

Note: this ADR is about the **AeroGPU** device identity/ABI used by the Win7 WDDM driver.
The canonical full-system machine (`aero_machine::Machine`) supports two **mutually-exclusive**
boot display configurations:

- `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`): boot display is provided by the
  standalone `aero_gpu_vga` VGA/VBE device model.
  - When `MachineConfig::enable_pc_platform=false`, the VBE LFB is mapped directly at the configured
    base.
  - When `MachineConfig::enable_pc_platform=true`, the machine exposes a minimal Bochs/QEMU-compatible
    “Standard VGA” PCI function (currently `00:0c.0`, `1234:1111`) and routes the VBE LFB through PCI BAR0 inside
    the ACPI-reported PCI MMIO window / BAR router. The BAR base is assigned by BIOS POST / the PCI
    allocator (and may be relocated when other PCI devices are present). `aero_machine` mirrors the
    chosen BAR base into the BIOS VBE `PhysBasePtr` and the VGA device model so guests observe a
    coherent LFB base.
- `MachineConfig::enable_aerogpu=true` (and `enable_vga=false`): boot display is provided via
  AeroGPU’s BAR1-backed legacy VGA/VBE compatibility path (see
  [`docs/16-aerogpu-vga-vesa-compat.md`](../16-aerogpu-vga-vesa-compat.md)).

In both cases, the Bochs/QEMU “Standard VGA” PCI identity is *not* part of the AeroGPU PCI identity
contract documented here.

## Decision

### Canonical PCI identity

The canonical AeroGPU PCI identity is: `VEN=0xA3A0 DEV=0x0001 SUBSYS=0x0001A3A0`.

Expanded:

- `VEN=0xA3A0`
- `DEV=0x0001`
- `SUBSYS=0x0001A3A0` (i.e. `subsystem_vendor_id=0xA3A0`, `subsystem_device_id=0x0001`)

This is the identity new device models must expose by default, and new Windows driver packages/INFs must bind to.

### Canonical ABI

The canonical AeroGPU guest↔host ABI is the **versioned protocol** defined by
`drivers/aerogpu/protocol/{aerogpu_pci.h,aerogpu_ring.h,aerogpu_cmd.h}`:

- [`drivers/aerogpu/protocol/aerogpu_pci.h`](../../drivers/aerogpu/protocol/aerogpu_pci.h) (PCI identity + MMIO register map + ABI version)
- [`drivers/aerogpu/protocol/aerogpu_ring.h`](../../drivers/aerogpu/protocol/aerogpu_ring.h) (ring layout and synchronization)
- [`drivers/aerogpu/protocol/aerogpu_cmd.h`](../../drivers/aerogpu/protocol/aerogpu_cmd.h) (command stream wire format)
- [`drivers/aerogpu/protocol/aerogpu_escape.h`](../../drivers/aerogpu/protocol/aerogpu_escape.h) (stable Escape packet header + base ops)

ABI compatibility is controlled by the versioning scheme in those headers (major/minor, device-reported version register). Any new ABI work must extend these headers (and bump versions as required) rather than creating alternate “side” ABIs.

### Canonical Windows device contract

The canonical Windows-facing device contract lives in:

- [`docs/windows-device-contract.md`](../windows-device-contract.md)
- [`docs/windows-device-contract.json`](../windows-device-contract.json)

These documents must reflect the canonical AeroGPU PCI identity and the canonical ABI surface (at the level they describe), and are the source of truth for:

- Windows driver binding (INF hardware IDs)
- Guest Tools automation / `CriticalDeviceDatabase` seeding
- CI drift checks

## Alternatives considered

1. **Keep the retired cmd/completion-ring prototype ABI as “the” AeroGPU ABI**
   - Pros: was useful for early host-side experiments.
   - Cons: does not match the WDDM driver protocol headers, does not align with current driver packaging, and encourages a split-brain GPU device story (two different “AeroGPU” devices).

2. **Keep the legacy 1AED MMIO ABI**
   - Pros: preserves compatibility with any existing guests built against that ABI.
   - Cons: unversioned/less extensible than the current A3A0 versioned headers; perpetuates multiple active ABI surfaces; increases emulator/device-model complexity.

3. **Use generic/borrowed PCI IDs (e.g. virtio IDs, QEMU IDs, or random vendor IDs)**
   - Pros: avoids “inventing” IDs.
   - Cons: increases the risk of unintended driver binding collisions and obscures that this is a project-specific contract; does not solve drift unless there is still a single canonical identity.

## Consequences

- **Docs, INFs, and device models must stay in sync.** Any change to AeroGPU’s PCI identity or ABI must update all consumers (protocol headers, emulator/device model, INFs, and `docs/windows-device-contract.{md,json}`) in the same change set.

- **Legacy IDs/ABIs are deprecated and must be explicitly labeled + gated.**
  - **1AE0** (older guest stack / placeholder IDs): archived under `prototype/legacy-win7-aerogpu-1ae0/`; not supported by the current emulator/device models.
  - **1AED** (legacy MMIO ABI): supported only behind an explicit “legacy ABI” compatibility mode; no new features added.
  - Retired cmd/completion-ring prototype ABI: treated as an internal/experimental ABI; must not be presented as the AeroGPU WDDM device.

- **Migration / removal timeline (project policy):**
  1. Immediately: new development targets **A3A0 + versioned protocol headers** only. Legacy IDs/ABIs must not be used by default.
  2. After **one release cycle** with A3A0 as default: remove the retired cmd/completion-ring prototype ABI and 1AE0 from any default configs and docs; keep only in clearly archived prototype locations.
  3. After **two release cycles** with A3A0 as default: drop 1AED compatibility unless there is a documented, actively-used downstream dependency that requires it.

- **CI drift checks are required.** CI must detect mismatches between:
  - `drivers/aerogpu/protocol/aerogpu_pci.h` (canonical constants),
  - `docs/windows-device-contract.json` (canonical Windows contract),
  - driver packaging INFs / installer logic that bind to the device.
