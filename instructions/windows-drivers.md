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
| `drivers/windows7/virtio/common/` | Shared Win7 virtio glue (WDM/NDIS/StorPort shims + split virtqueue impl) |
| `drivers/windows7/virtio-blk/` | Block device driver |
| `drivers/windows7/virtio-net/` | Network driver |
| `drivers/windows7/virtio-input/` | HID input driver |
| `drivers/windows7/virtio-snd/` | Audio driver |
| `drivers/windows/virtio/` | Portable virtio helpers (shared with Win7 drivers; host-side tests build on Linux) |
| `drivers/windows7/tests/guest-selftest/` | `aero-virtio-selftest.exe` (runs inside Win7 guest; emits serial markers) |
| `drivers/windows7/tests/host-harness/` | QEMU host harness that runs the guest selftest and returns a deterministic PASS/FAIL |
| `drivers/win7/virtio/` | Win7 KMDF virtio scaffolding + capability parser tests (non-shipping test drivers) |
| `drivers/protocol/` | Protocol definitions (shared with emulator) |
| `drivers/protocol/virtio/` | Rust virtio protocol definitions + unit tests (`cargo test`) |
| `guest-tools/` | Guest tools packaging and installer |
| `docs/windows-device-contract.{md,json}` | Machine-readable PCI/INF/service binding contract consumed by CI + Guest Tools |
| `tools/device_contract_validator/` | Rust validator for the device contract (runs in CI) |

---

## Essential Documentation

**Must read:**

- [`docs/windows/README.md`](../docs/windows/README.md) — Windows driver development overview
- [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md) — Virtio device contract
- [`docs/windows-device-contract.md`](../docs/windows-device-contract.md) — Unified device/driver binding contract (virtio + AeroGPU)
- [`docs/windows/virtio-pci-modern-interrupts.md`](../docs/windows/virtio-pci-modern-interrupts.md) — MSI-X/INTx handling
- [`docs/virtio/virtqueue-split-ring-win7.md`](../docs/virtio/virtqueue-split-ring-win7.md) — Virtqueue implementation

**Reference:**

- [`docs/16-windows7-driver-build-and-signing.md`](../docs/16-windows7-driver-build-and-signing.md) — Build toolchain
- [`docs/16-driver-packaging-and-signing.md`](../docs/16-driver-packaging-and-signing.md) — Packaging and catalogs
- [`docs/windows7-driver-troubleshooting.md`](../docs/windows7-driver-troubleshooting.md) — Debugging tips
- [`docs/16-aerogpu-vga-vesa-compat.md`](../docs/16-aerogpu-vga-vesa-compat.md) — AeroGPU VGA compat
- [`drivers/aerogpu/README.md`](../drivers/aerogpu/README.md) — AeroGPU build/CI entrypoint + key docs
- [`drivers/windows7/tests/README.md`](../drivers/windows7/tests/README.md) — Win7 virtio selftest + harness overview (incl. virtio-snd notes)
- [`drivers/windows7/tests/host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md) — Win7 virtio test harness (incl. virtio-snd wav capture)
- [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) — Guest selftest details (incl. virtio-snd playback/capture/duplex)

---

## Device Contracts

**Critical:** The Windows drivers and emulator device models **must stay in sync**. The source of truth is:

- Virtio transport + feature contract: [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md) (`AERO-W7-VIRTIO`)
- Unified device binding manifest: [`docs/windows-device-contract.json`](../docs/windows-device-contract.json) (+ human-readable [`docs/windows-device-contract.md`](../docs/windows-device-contract.md))

Any change to PCI vendor/device IDs, BAR sizes, or feature bits requires:
1. Update the contract document
2. Update the emulator device model
3. Update the driver INF files
4. Coordinate with Graphics (B) and Integration (H) workstreams

### CI guardrails (do not bypass)

These checks exist specifically to prevent “driver installs but doesn’t bind” regressions:

- Win7 driver build + packaging: [`.github/workflows/drivers-win7.yml`](../.github/workflows/drivers-win7.yml)
  - Runs contract drift checks (docs ↔ INFs ↔ emulator PCI profiles)
  - Runs host-side unit tests for virtio common, virtio-snd protocol engines, and AeroGPU command stream encoding
- Windows virtio contract wiring (manifest ↔ INFs ↔ emulator ↔ guest-tools): [`.github/workflows/windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml)
- Device contract validator (JSON schema + invariants): [`.github/workflows/windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml)
- Virtio protocol crate tests: [`.github/workflows/virtio-protocol.yml`](../.github/workflows/virtio-protocol.yml)

Local equivalents for fast iteration (Linux/macOS host):

```bash
# Virtio contract drift checks (docs ↔ INFs ↔ emulator PCI profiles + guest-tools specs)
python3 scripts/ci/check-windows7-virtio-contract-consistency.py
python3 scripts/ci/check-windows-virtio-contract.py --check

# Device contract schema/invariants (same check as windows-device-contract.yml)
cargo run -p device-contract-validator --locked

# Rust virtio protocol unit tests (same as virtio-protocol.yml)
cargo test --locked --manifest-path drivers/protocol/virtio/Cargo.toml

# Host-side C unit tests for virtio helpers and virtio-snd protocol engines
cmake -S . -B build-virtio-host-tests -DAERO_VIRTIO_BUILD_TESTS=ON -DAERO_AEROGPU_BUILD_TESTS=OFF -DCMAKE_BUILD_TYPE=Release
cmake --build build-virtio-host-tests
ctest --test-dir build-virtio-host-tests --output-on-failure
```

---

## Tasks

The tables below are meant to be an **onboarding map**: what already exists in-tree (with CI coverage), and what remains.

Legend:

- **Implemented** = present in-tree and wired into at least one CI workflow/guardrail.
- **Partial** = present but explicitly minimal/stubbed; known follow-ups remain.
- **Remaining** = not implemented yet (or explicitly stubbed with TODO-level behavior).

### Virtio Foundation + Guardrails (Shared)

| ID | Status | Task | Where | CI/Guardrails |
|----|--------|------|-------|---------------|
| VIO-001 | Implemented | Virtio-pci **modern** transport parser/library (cap discovery + BAR0 MMIO layout) | [`drivers/windows/virtio/pci-modern/README.md`](../drivers/windows/virtio/pci-modern/README.md) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (virtio host tests), [`check-windows7-virtio-contract-consistency.py`](../scripts/ci/check-windows7-virtio-contract-consistency.py) |
| VIO-002 | Implemented | Split-ring virtqueue + SG helpers (portable) | [`drivers/windows/virtio/common/README.md`](../drivers/windows/virtio/common/README.md) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (virtio host tests), [`check-win7-virtio-header-collisions.py`](../scripts/ci/check-win7-virtio-header-collisions.py) |
| VIO-003 | Implemented | Win7 virtio common glue (WDM/NDIS/StorPort shims + INTx) | [`drivers/windows7/virtio/common/README.md`](../drivers/windows7/virtio/common/README.md) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (guardrails + host tests) |
| VIO-004 | Implemented | Rust virtio protocol definitions + tests | [`drivers/protocol/virtio/README.md`](../drivers/protocol/virtio/README.md) | [`virtio-protocol.yml`](../.github/workflows/virtio-protocol.yml) |

### Virtio Device Drivers

| ID | Status | Task | Where | Tests / docs to start with |
|----|--------|------|-------|----------------------------|
| VIO-010 | Partial | virtio-input (KMDF HID minidriver; contract-v1 IDs + report descriptor synthesis) | `drivers/windows7/virtio-input/` | README: [`drivers/windows7/virtio-input/README.md`](../drivers/windows7/virtio-input/README.md); guest coverage: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) |
| VIO-011 | Partial | virtio-blk (StorPort miniport; contract-v1 IDs; boot-start capable) | `drivers/windows7/virtio-blk/` | README: [`drivers/windows7/virtio-blk/README.md`](../drivers/windows7/virtio-blk/README.md); guest coverage: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) |
| VIO-012 | Partial | virtio-net (NDIS 6.20 miniport; contract-v1 IDs) | `drivers/windows7/virtio-net/` | README: [`drivers/windows7/virtio-net/README.md`](../drivers/windows7/virtio-net/README.md); guest coverage: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) |
| VIO-016 | Partial | **virtio-snd** (PortCls/WaveRT audio driver; modern-only + optional transitional variant) | `drivers/windows7/virtio-snd/` | README: [`drivers/windows7/virtio-snd/README.md`](../drivers/windows7/virtio-snd/README.md); design notes: [`drivers/windows7/virtio-snd/docs/design.md`](../drivers/windows7/virtio-snd/docs/design.md) |
| VIO-017 | Implemented | virtio-snd host unit tests (control/tx/rx engines; SG/virtqueue behavior) | `drivers/windows7/virtio-snd/tests/host/` | Host test README: [`.../tests/host/README.md`](../drivers/windows7/virtio-snd/tests/host/README.md); CI: [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) |
| VIO-018 | Implemented | Win7 guest selftest coverage for virtio-snd **playback + capture + duplex** markers | `drivers/windows7/tests/guest-selftest/` | Guest tool docs: [`guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md); CI build: [`win7-virtio-selftest.yml`](../.github/workflows/win7-virtio-selftest.yml); harness: [`win7-virtio-harness.yml`](../.github/workflows/win7-virtio-harness.yml) (self-hosted) |
| VIO-019 | Implemented | Host harness: QEMU runner that parses guest selftest markers + optional wav non-silence verification | `drivers/windows7/tests/host-harness/` | Harness README: [`host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md); unit tests: `drivers/windows7/tests/host-harness/tests/` |

### Guest Tools Tasks

| ID | Status | Task | Where | CI/Guardrails |
|----|--------|------|-------|---------------|
| GT-001 | Implemented | Windows device contract docs + JSON manifest | Docs: [`windows-device-contract.md`](../docs/windows-device-contract.md) + [`windows-device-contract.json`](../docs/windows-device-contract.json) | [`windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml) (Rust validator) |
| GT-002 | Implemented | Guest Tools config generation (`guest-tools/config/devices.cmd`) from the contract | Generator: [`scripts/generate-guest-tools-devices-cmd.py`](../scripts/generate-guest-tools-devices-cmd.py); output: [`guest-tools/config/devices.cmd`](../guest-tools/config/devices.cmd) | [`windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml), [`check-windows-virtio-contract.py`](../scripts/ci/check-windows-virtio-contract.py) |
| GT-003 | Implemented | Guest Tools installer stages drivers, manages test-signing policy, and pre-seeds boot-critical virtio-blk (`CriticalDeviceDatabase`) | Installer: [`guest-tools/setup.cmd`](../guest-tools/setup.cmd) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (packages Guest Tools media) |
| GT-004 | Implemented | Enforce INF ↔ contract ↔ emulator consistency (virtio HWIDs, revision gating, service names) | [`check-windows7-virtio-contract-consistency.py`](../scripts/ci/check-windows7-virtio-contract-consistency.py) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (guardrails) |
| GT-005 | Implemented | Enforce virtio contract wiring (contract JSON ↔ INFs ↔ emulator PCI profiles ↔ guest-tools) | [`check-windows-virtio-contract.py`](../scripts/ci/check-windows-virtio-contract.py) | [`windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml) |
| GT-006 | Implemented | Contract versioning + policy surfaced in docs and manifests | [`windows-device-contract.md`](../docs/windows-device-contract.md) | [`windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml) |

### AeroGPU Driver Tasks

| ID | Status | Task | Where | Tests / docs to start with |
|----|--------|------|-------|----------------------------|
| AGPU-001 | Partial | AeroGPU KMD (WDDM 1.1 miniport) + D3D9Ex UMD bring-up (Aero composition + basic rendering) | `drivers/aerogpu/kmd/`, `drivers/aerogpu/umd/d3d9/` | UMD README (stub list): [`drivers/aerogpu/umd/d3d9/README.md`](../drivers/aerogpu/umd/d3d9/README.md); guest suite: [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md); host tests: `drivers/aerogpu/umd/d3d9/tests/` (CI: [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml)) |
| AGPU-002 | Remaining | D3D9Ex “currently stubbed DDIs” → real implementations (fixed-function + state blocks + misc DDIs) | `drivers/aerogpu/umd/d3d9/` | Stub checklist: [`d3d9/README.md#currently-stubbed-ddis`](../drivers/aerogpu/umd/d3d9/README.md#currently-stubbed-ddis); Win7 suite: [`drivers/aerogpu/tests/win7/`](../drivers/aerogpu/tests/win7/) |
| AGPU-003 | Partial | D3D10/11 UMD feature expansion beyond “triangle” bring-up (pipeline/state coverage; Map/Unmap correctness; format support) | `drivers/aerogpu/umd/d3d10_11/` | README: [`drivers/aerogpu/umd/d3d10_11/README.md`](../drivers/aerogpu/umd/d3d10_11/README.md); checklist: [`docs/graphics/win7-d3d11ddi-function-tables.md`](../docs/graphics/win7-d3d11ddi-function-tables.md); host tests: `drivers/aerogpu/umd/d3d10_11/tests/` |
| AGPU-004 | Remaining | DXGI/D3D10/11 shared-resource interop (export/import shared surfaces, share-token plumbing) | `drivers/aerogpu/umd/d3d10_11/` | Design contract: [`docs/graphics/win7-shared-surfaces-share-token.md`](../docs/graphics/win7-shared-surfaces-share-token.md); related D3D9 tests live under `drivers/aerogpu/tests/win7/` |
| AGPU-005 | Implemented | AeroGPU Win7 guest-side validation suite (D3D9/D3D10/D3D11 + vblank/fence/ring probes) | `drivers/aerogpu/tests/win7/` | [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md) |

---

## Driver Architecture

### Virtio Driver Stack

```
┌──────────────────────────────────────────────────────┐
│                    Windows 7 Guest                    │
├──────────────────────────────────────────────────────┤
│  aero_virtio_blk.sys   aero_virtio_net.sys   ...     │  Device-specific drivers
│         │                    │                        │
│         └───────┬────────────┘                        │
│                 ▼                                     │
│   Shared in-driver virtio libs (not a separate .sys): │
│     - drivers/windows7/virtio/common/                 │
│     - drivers/windows/virtio/{common,pci-modern}/     │
│                 │                                     │
├─────────────────┼────────────────────────────────────┤
│                 ▼                                     │
│                 PCI Bus (emulator)                    │
└──────────────────────────────────────────────────────┘
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

# For virtio drivers (example: virtio-blk):
cd drivers\windows7\virtio-blk
msbuild aero_virtio_blk.vcxproj /p:Configuration=Release /p:Platform=x64
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

For virtio end-to-end regression testing (recommended):

- Guest selftest: `drivers/windows7/tests/guest-selftest/` (`aero-virtio-selftest.exe`)
- Host harness: `drivers/windows7/tests/host-harness/` (boots QEMU + parses serial markers)
  - Supports virtio-snd wav capture + non-silence verification when enabled.
  - See: [`drivers/windows7/tests/README.md`](../drivers/windows7/tests/README.md)

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Read [`docs/windows/README.md`](../docs/windows/README.md)
3. ☐ Read [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md)
4. ☐ Read [`docs/windows-device-contract.md`](../docs/windows-device-contract.md)
5. ☐ Read [`drivers/windows7/tests/README.md`](../drivers/windows7/tests/README.md) (selftest + harness)
6. ☐ Run contract checks locally (`python3 scripts/ci/check-windows7-virtio-contract-consistency.py`)
7. ☐ Set up Windows build environment (WDK)
8. ☐ Explore `drivers/aerogpu/` and `drivers/windows7/`
9. ☐ Build existing drivers to verify toolchain
10. ☐ Pick a task from the tables above and begin

---

*These drivers make the emulator fast. Virtio provides 10-100x speedup over full emulation.*
