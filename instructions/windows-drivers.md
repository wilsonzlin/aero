# Workstream C: Windows Drivers

> **⚠️ MANDATORY: Read and follow [`AGENTS.md`](../AGENTS.md) in its entirety before starting any work.**
>
> AGENTS.md contains critical operational guidance including:
> - Defensive mindset (assume hostile/misbehaving code)
> - Resource limits and `safe-run.sh` usage
> - Windows 7 test ISO location (`/state/win7.iso`)
> - Interface contracts
> - Technology stack decisions
>
> **Failure to follow AGENTS.md will result in broken builds, OOM kills, and wasted effort.**

---

## Overview

This workstream owns **Windows 7 guest drivers**: the AeroGPU display driver (WDDM KMD + UMD) and virtio paravirtualized drivers (virtio-blk, virtio-net, virtio-input, virtio-snd).

These drivers run **inside the guest Windows 7** and communicate with the emulator's device models via PCI MMIO/IO ports and shared memory.

---

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `drivers/aerogpu/` | AeroGPU WDDM driver (KMD + UMD) |
| `drivers/aerogpu/kmd/` | Kernel-mode driver |
| `drivers/aerogpu/umd/` | User-mode driver (D3D9/D3D10/D3D11 DDI) |
| `drivers/windows7/` | Virtio drivers for Windows 7 |
| `drivers/windows7/virtio-blk/` | Block device driver |
| `drivers/windows7/virtio-net/` | Network driver |
| `drivers/windows7/virtio-input/` | HID input driver |
| `drivers/windows7/virtio-snd/` | Audio driver |
| `drivers/protocol/` | Protocol definitions (shared with emulator) |
| `guest-tools/` | Guest tools packaging and installer |

---

## Essential Documentation

**Must read:**

- [`docs/windows/README.md`](../docs/windows/README.md) — Windows driver development overview
- [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md) — Virtio device contract
- [`docs/windows/virtio-pci-modern-interrupts.md`](../docs/windows/virtio-pci-modern-interrupts.md) — MSI-X/INTx handling
- [`docs/virtio/virtqueue-split-ring-win7.md`](../docs/virtio/virtqueue-split-ring-win7.md) — Virtqueue implementation

**Reference:**

- [`docs/16-windows7-driver-build-and-signing.md`](../docs/16-windows7-driver-build-and-signing.md) — Build toolchain
- [`docs/16-driver-packaging-and-signing.md`](../docs/16-driver-packaging-and-signing.md) — Packaging and catalogs
- [`docs/windows7-driver-troubleshooting.md`](../docs/windows7-driver-troubleshooting.md) — Debugging tips
- [`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md) — AeroGPU VGA compat

---

## Device Contracts

**Critical:** The Windows drivers and emulator device models **must stay in sync**. The source of truth is:

- [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md) — PCI IDs, BAR layouts, feature bits

Any change to PCI vendor/device IDs, BAR sizes, or feature bits requires:
1. Update the contract document
2. Update the emulator device model
3. Update the driver INF files
4. Coordinate with Graphics (B) and Integration (H) workstreams

---

## Tasks

### Virtio Foundation Tasks (Shared)

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| VIO-001 | Virtio-pci modern transport library (shared) | P0 | None | High |
| VIO-002 | Virtqueue split-ring implementation (shared) | P0 | VIO-001 | Very High |
| VIO-003 | MSI-X + legacy interrupt plumbing (shared) | P0 | VIO-001 | High |

### Virtio Device Drivers

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| VIO-010 | Virtio-input KMDF HID minidriver | P1 | VIO-001..VIO-003 | Very High |
| VIO-011 | Virtio-blk driver | P1 | VIO-001..VIO-003 | High |
| VIO-012 | Virtio-net driver | P1 | VIO-001..VIO-003 | High |
| VIO-013 | Virtio-input HID report descriptor | P1 | VIO-010 | High |
| VIO-014 | Virtio-input packaging/signing | P1 | VIO-010, VIO-013 | Medium |
| VIO-015 | Virtio-input functional test plan | P1 | VIO-010..VIO-014 | Medium |

### Guest Tools Tasks

| ID | Task | Priority | Dependencies | Complexity |
|----|------|----------|--------------|------------|
| GT-001 | Define/maintain Windows PCI device contract | P0 | None | Medium |
| GT-002 | Guest Tools installer consumes contract | P0 | GT-001 | Medium |
| GT-003 | Seed CriticalDeviceDatabase for boot (virtio-blk) | P0 | GT-002 | High |
| GT-004 | Ensure INF hardware IDs match contract | P0 | GT-001 | Medium |
| GT-005 | Add emulator CI check for PCI ID match | P1 | GT-001 | Medium |
| GT-006 | Versioning policy for contract changes | P1 | GT-001 | Low |

---

## Driver Architecture

### Virtio Driver Stack

```
┌─────────────────────────────────────────────┐
│            Windows 7 Guest                   │
├─────────────────────────────────────────────┤
│  virtio-blk.sys    virtio-net.sys    ...    │  Device-specific drivers
│         │               │                    │
│         └───────┬───────┘                    │
│                 ▼                            │
│          virtio-pci.sys                      │  Shared transport layer
│                 │                            │
├─────────────────┼───────────────────────────┤
│                 ▼                            │
│           PCI Bus (emulator)                 │
└─────────────────────────────────────────────┘
```

### AeroGPU Driver Stack

```
┌─────────────────────────────────────────────┐
│            Windows 7 Guest                   │
├─────────────────────────────────────────────┤
│        D3D9/D3D10/D3D11 Runtime             │
│                 │                            │
│                 ▼                            │
│        aerogpu_umd.dll (UMD)                │  User-mode DDI implementation
│                 │                            │
│                 ▼                            │
│        aerogpu_kmd.sys (KMD)                │  WDDM miniport driver
│                 │                            │
├─────────────────┼───────────────────────────┤
│                 ▼                            │
│         AeroGPU PCI Device (emulator)        │
└─────────────────────────────────────────────┘
```

---

## Build Environment

**Requires Windows + WDK.** If developing on Linux/macOS, you'll need:
- Cross-compilation setup, OR
- Windows VM for driver builds, OR
- CI that builds on Windows

See [`docs/16-windows7-driver-build-and-signing.md`](../docs/16-windows7-driver-build-and-signing.md) for toolchain setup.

```powershell
# On Windows with WDK installed:
cd drivers\aerogpu
msbuild aerogpu.sln /p:Configuration=Release /p:Platform=x64

# For virtio drivers:
cd drivers\windows7\virtio-blk
msbuild virtio-blk.vcxproj /p:Configuration=Release /p:Platform=x64
```

---

## Test Signing

Windows 7 requires signed drivers. For development:

1. Enable test signing: `bcdedit /set testsigning on`
2. Sign drivers with test certificate
3. Install test certificate in guest

See [`docs/win7-bcd-offline-patching.md`](../docs/win7-bcd-offline-patching.md) for offline BCD patching.

---

## Coordination Points

### Dependencies on Other Workstreams

- **Graphics (B)**: AeroGPU driver must match emulator device model
- **Integration (H)**: Device models must be wired into platform

### What Other Workstreams Need From You

- Stable driver binaries for integration testing
- Updated INF files when device model changes
- Guest Tools installer for easy deployment

### Cross-Workstream Contract Changes

**Any change to these requires coordination:**
- PCI Vendor/Device IDs
- BAR sizes or layouts
- Feature bits
- Command/status register formats

---

## Testing

Driver testing requires a running Windows 7 guest:

1. Boot Windows 7 in the emulator
2. Install test-signed drivers
3. Verify device appears in Device Manager
4. Run functional tests

For AeroGPU:
- Check display resolution changes
- Verify DWM composition (Aero glass)
- Run D3D9/D3D10/D3D11 test apps

For virtio-blk:
- Verify disk appears in Disk Management
- Read/write test files
- Check performance (should be faster than emulated AHCI)

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Read [`docs/windows/README.md`](../docs/windows/README.md)
3. ☐ Read [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md)
4. ☐ Set up Windows build environment (WDK)
5. ☐ Explore `drivers/aerogpu/` and `drivers/windows7/`
6. ☐ Build existing drivers to verify toolchain
7. ☐ Pick a task from the tables above and begin

---

*These drivers make the emulator fast. Virtio provides 10-100x speedup over full emulation.*
