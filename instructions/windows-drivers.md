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

It also owns the **binding/packaging surface** for those drivers (HWID/service-name contracts + Guest Tools media) so the emulator, driver INFs, and installer scripts stay consistent.

These drivers run **inside the guest Windows 7** and communicate with the emulator's device models via PCI MMIO/IO ports and shared memory.

---

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `drivers/aerogpu/` | AeroGPU WDDM driver (KMD + UMD) |
| `drivers/aerogpu/kmd/` | Kernel-mode driver |
| `drivers/aerogpu/umd/` | User-mode driver (D3D9/D3D10/D3D11 DDI) |
| `drivers/aerogpu/tests/win7/` | Guest-side AeroGPU validation suite (D3D9/D3D10/D3D11) |
| `drivers/windows7/` | Virtio drivers for Windows 7 |
| `drivers/windows7/virtio/common/` | Shared Win7 virtio glue (WDM/NDIS/StorPort shims + split virtqueue impl) |
| `drivers/windows7/virtio-blk/` | Block device driver |
| `drivers/windows7/virtio-net/` | Network driver |
| `drivers/windows7/virtio-input/` | HID input driver |
| `drivers/windows7/virtio-snd/` | Audio driver |
| `drivers/virtio/` | Virtio driver-pack/ISO layout surface (virtio-win compatibility tooling; `sample/` placeholders) |
| `drivers/scripts/` | Driver-pack + Guest Tools build/install scripts (`make-guest-tools-from-ci.ps1`, `make-driver-pack.ps1`, etc.) |
| `drivers/windows/virtio/` | Portable virtio helpers (shared with Win7 drivers; host-side tests build on Linux) |
| `drivers/windows7/tests/guest-selftest/` | `aero-virtio-selftest.exe` (runs inside Win7 guest; emits serial markers) |
| `drivers/windows7/tests/host-harness/` | QEMU host harness that runs the guest selftest and returns a deterministic PASS/FAIL |
| `drivers/win7/virtio/` | Win7 KMDF virtio scaffolding + capability parser tests (non-shipping test drivers) |
| `drivers/protocol/` | Protocol definitions (shared with emulator) |
| `drivers/protocol/virtio/` | Rust virtio protocol definitions + unit tests (`cargo test`) |
| `guest-tools/` | Guest tools packaging and installer |
| `docs/windows-device-contract.{md,json}` | Machine-readable PCI/INF/service binding contract consumed by CI + Guest Tools |
| `docs/windows-device-contract-virtio-win.json` | Optional virtio-win compatibility contract (used by virtio-win packaging flows) |
| `tools/device_contract_validator/` | Rust validator for the device contract (runs in CI) |
| `tools/packaging/` | Guest Tools packager + packaging specs (ISO/zip builder) |
| `tools/packaging/aero_packager/` | Deterministic Rust packager implementation + tests |
| `tools/guest-tools/` | Guest Tools config validation + linters (runs in CI) |
| `ci/` | CI scripts for Win7 driver builds/signing/packaging (WDK provisioning, catalogs, signing) |

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
- [`docs/16-guest-tools-packaging.md`](../docs/16-guest-tools-packaging.md) — Guest Tools packager specs/inputs/outputs (ISO/zip)
- [`docs/16-virtio-drivers-win7.md`](../docs/16-virtio-drivers-win7.md) — Virtio driver plumbing notes (transport/virtqueues)
- [`docs/16-virtio-pci-legacy-transitional.md`](../docs/16-virtio-pci-legacy-transitional.md) — Legacy/transitional virtio-pci notes (compatibility)
- [`docs/adr/0016-win7-virtio-driver-naming.md`](../docs/adr/0016-win7-virtio-driver-naming.md) — Canonical in-tree Win7 virtio naming scheme (`aero_virtio_*`)
- [`drivers/README.md`](../drivers/README.md) — What CI actually ships (artifact names, release workflow, Guest Tools media)
- [`docs/virtio-windows-drivers.md`](../docs/virtio-windows-drivers.md) — Virtio driver packaging options (in-tree vs virtio-win)
- [`docs/virtio-input.md`](../docs/virtio-input.md) — virtio-input device model notes (keyboard/mouse) + contract mapping
- [`docs/virtio-input-test-plan.md`](../docs/virtio-input-test-plan.md) — virtio-input end-to-end test plan (device model ↔ driver ↔ harness ↔ web runtime)
- [`docs/virtio-snd.md`](../docs/virtio-snd.md) — virtio-snd device model notes + contract mapping (incl. transitional ID notes)
- [`docs/graphics/win7-aerogpu-validation.md`](../docs/graphics/win7-aerogpu-validation.md) — AeroGPU stability checklist (TDR/vblank/perf debug playbook)
- [`docs/windows7-guest-tools.md`](../docs/windows7-guest-tools.md) — End-to-end Win7 install + switch to virtio + AeroGPU
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
  - Optional virtio-win compatibility manifest (used only for virtio-win-based packaging/scripts): [`docs/windows-device-contract-virtio-win.json`](../docs/windows-device-contract-virtio-win.json)

Any change to PCI vendor/device IDs, BAR sizes, or feature bits requires:
1. Update the contract document
2. Update the emulator device model
3. Update the driver INF files
4. Update the unified binding manifest (`docs/windows-device-contract.json`) and regenerate Guest Tools `devices.cmd`
5. Coordinate with Graphics (B) and Integration (H) workstreams

### CI guardrails (do not bypass)

These checks exist specifically to prevent “driver installs but doesn’t bind” regressions:

- Win7 driver build + packaging: [`.github/workflows/drivers-win7.yml`](../.github/workflows/drivers-win7.yml)
  - Runs contract drift checks (docs ↔ INFs ↔ emulator PCI profiles)
  - Runs host-side unit tests for virtio common, virtio-snd protocol engines, and AeroGPU command stream encoding
- Repo-wide CI also runs some driver/Guest Tools guardrails (WDK macro guards, Guest Tools `devices.cmd` regeneration, D3D11 guest-memory import invariants): [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)
- Windows virtio contract wiring (manifest ↔ INFs ↔ emulator ↔ guest-tools): [`.github/workflows/windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml)
- Device contract validator (JSON schema + invariants): [`.github/workflows/windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml)
- Virtio protocol crate tests: [`.github/workflows/virtio-protocol.yml`](../.github/workflows/virtio-protocol.yml)
- Win7 virtio guest selftest build (x86 + x64 EXEs): [`.github/workflows/win7-virtio-selftest.yml`](../.github/workflows/win7-virtio-selftest.yml)
- Win7 virtio QEMU harness (self-hosted; end-to-end guest run, incl. virtio-snd wav capture): [`.github/workflows/win7-virtio-harness.yml`](../.github/workflows/win7-virtio-harness.yml)
- Guest Tools packager + spec/config validation: [`.github/workflows/guest-tools-packager.yml`](../.github/workflows/guest-tools-packager.yml)
- Guest Tools `devices.cmd` regeneration check (must match contract JSON): [`.github/workflows/guest-tools-devices-cmd.yml`](../.github/workflows/guest-tools-devices-cmd.yml)
- Win7 toolchain smoke (WDK provisioning + `Inf2Cat /os:7_X86,7_X64`): [`.github/workflows/toolchain-win7-smoke.yml`](../.github/workflows/toolchain-win7-smoke.yml)
- virtio-win packaging smoke tests (optional flow, upstream virtio-win bundles): [`.github/workflows/virtio-win-packaging-smoke.yml`](../.github/workflows/virtio-win-packaging-smoke.yml)
- Sample virtio driver ISO build + smoke tests: [`.github/workflows/virtio-driver-iso.yml`](../.github/workflows/virtio-driver-iso.yml)
- Tagged release pipeline (publishes signed driver bundles + Guest Tools as GitHub Release assets): [`.github/workflows/release-drivers-win7.yml`](../.github/workflows/release-drivers-win7.yml)
- Docs lint + contract/link checks (includes virtio contract consistency + share-token checks): [`.github/workflows/docs.yml`](../.github/workflows/docs.yml)

Local equivalents for fast iteration:

```bash
# Virtio contract drift checks (docs ↔ INFs ↔ emulator PCI profiles + guest-tools specs)
python3 scripts/ci/check-windows7-virtio-contract-consistency.py
python3 scripts/ci/check-windows-virtio-contract.py --check

# Optional: regenerate derived artifacts (currently only guest-tools/config/devices.cmd), then re-check
python3 scripts/ci/check-windows-virtio-contract.py --fix

# Ensure `guest-tools/config/devices.cmd` matches the contract JSON + generator
python3 scripts/ci/gen-guest-tools-devices-cmd.py --check

# Additional Win7 driver guardrails (fast, no VM required)
python3 scripts/ci/check-virtio-snd-vcxproj-sources.py
python3 scripts/ci/check-win7-virtqueue-split-headers.py
python3 scripts/ci/check-virtqueue-split-driver-builds.py
python3 scripts/ci/check-win7-virtio-header-collisions.py
python3 scripts/ci/check-win7-virtio-net-pci-config-access.py
python3 scripts/ci/check-win7-virtio-blk-no-duplicate-freeresources.py

# AeroGPU guardrails (fast, no VM required)
python3 scripts/ci/check-aerogpu-d3d9-def-stdcall.py
python3 scripts/ci/check-aerogpu-d3d10-def-stdcall.py
python3 scripts/ci/check-aerogpu-wdk-guards.py
python3 scripts/ci/check-aero-d3d11-guest-memory-imports.py
python3 scripts/ci/check-aerogpu-share-token-contract.py

# Repo layout guardrails (includes AeroGPU Win7 test-suite manifest/doc/fallback-list invariants)
bash scripts/ci/check-repo-layout.sh

# Ensure no duplicate virtio INFs/projects bind the same HWIDs (requires pwsh)
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/check-virtio-driver-uniqueness.ps1

# Device contract schema/invariants (same check as windows-device-contract.yml)
cargo run -p device-contract-validator --locked

# Rust virtio protocol unit tests (same as virtio-protocol.yml)
cargo test --locked --manifest-path drivers/protocol/virtio/Cargo.toml

# Guest Tools packager tests + spec/config validation
cargo test --locked --manifest-path tools/packaging/aero_packager/Cargo.toml
python3 tools/guest-tools/validate_config.py --spec tools/packaging/specs/win7-signed.json

# Ensure virtio-win Guest Tools docs + wrapper defaults stay in sync
python3 scripts/ci/check-virtio-win-guest-tools-docs.py

# Host-harness Python unit tests (wav verification + QEMU arg quoting)
python3 -m unittest discover -s drivers/windows7/tests/host-harness/tests -p 'test_*.py'

# Host-side C unit tests for virtio helpers and virtio-snd protocol engines
cmake -S . -B build-virtio-host-tests -DAERO_VIRTIO_BUILD_TESTS=ON -DAERO_AEROGPU_BUILD_TESTS=OFF -DCMAKE_BUILD_TYPE=Release
cmake --build build-virtio-host-tests
ctest --test-dir build-virtio-host-tests --output-on-failure

# Portable virtio PCI capability parser tests (virtio-core; no Windows/WDK required)
bash ./drivers/win7/virtio/tests/build_and_run.sh

# Host-side C++ unit tests for AeroGPU UMD helpers (command stream writer, submit buffer utils)
cmake -S . -B build-aerogpu-host-tests -DAERO_AEROGPU_BUILD_TESTS=ON -DAERO_VIRTIO_BUILD_TESTS=OFF -DCMAKE_BUILD_TYPE=Release
cmake --build build-aerogpu-host-tests
ctest --test-dir build-aerogpu-host-tests --output-on-failure --no-tests=error

# Windows-only: validate the installed WDK toolchain can generate Win7 catalogs
pwsh -NoProfile -ExecutionPolicy Bypass -File ci/validate-toolchain.ps1
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
| VIO-005 | Implemented | Win7 KMDF `virtio-core` transport (portable virtio PCI cap parser + optional Aero MMIO layout enforcement) | [`drivers/win7/virtio/virtio-core/README.md`](../drivers/win7/virtio/virtio-core/README.md), [`drivers/win7/virtio/tests/README.md`](../drivers/win7/virtio/tests/README.md) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (virtio host tests) |

### Virtio Device Drivers

| ID | Status | Task | Where | Tests / docs to start with |
|----|--------|------|-------|----------------------------|
| VIO-010 | Implemented | virtio-input (KMDF HID minidriver; contract-v1 IDs; keyboard+mouse separation; report descriptor synthesis) | `drivers/windows7/virtio-input/` | README: [`drivers/windows7/virtio-input/README.md`](../drivers/windows7/virtio-input/README.md); unit tests: [`drivers/windows7/virtio-input/tests/README.md`](../drivers/windows7/virtio-input/tests/README.md); guest selftest: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md); harness event injection: [`drivers/windows7/tests/host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md) |
| VIO-011 | Implemented | virtio-blk (StorPort miniport; contract-v1 IDs; boot-start capable; minimal feature set) | `drivers/windows7/virtio-blk/` | README: [`drivers/windows7/virtio-blk/README.md`](../drivers/windows7/virtio-blk/README.md); guest coverage: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) |
| VIO-012 | Implemented | virtio-net (NDIS 6.20 miniport; contract-v1 IDs; minimal feature set) | `drivers/windows7/virtio-net/` | README: [`drivers/windows7/virtio-net/README.md`](../drivers/windows7/virtio-net/README.md); guest coverage: [`drivers/windows7/tests/guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md) |
| VIO-016 | Implemented | **virtio-snd** (PortCls/WaveRT audio driver; contract v1 + optional transitional/QEMU package) | `drivers/windows7/virtio-snd/` | README: [`drivers/windows7/virtio-snd/README.md`](../drivers/windows7/virtio-snd/README.md); design notes: [`drivers/windows7/virtio-snd/docs/design.md`](../drivers/windows7/virtio-snd/docs/design.md) |
| VIO-017 | Implemented | virtio-snd host unit tests (control/tx/rx engines; SG/virtqueue behavior) | `drivers/windows7/virtio-snd/tests/host/` | Host test README: [`drivers/windows7/virtio-snd/tests/host/README.md`](../drivers/windows7/virtio-snd/tests/host/README.md); CI: [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) |
| VIO-018 | Implemented | Win7 guest selftest coverage for virtio-snd **playback + capture + duplex** markers | `drivers/windows7/tests/guest-selftest/` | Guest tool docs: [`guest-selftest/README.md`](../drivers/windows7/tests/guest-selftest/README.md); CI build: [`win7-virtio-selftest.yml`](../.github/workflows/win7-virtio-selftest.yml); harness: [`win7-virtio-harness.yml`](../.github/workflows/win7-virtio-harness.yml) (self-hosted) |
| VIO-019 | Implemented | Host harness: QEMU runner that parses guest selftest markers + optional wav non-silence verification | `drivers/windows7/tests/host-harness/` | Harness README: [`host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md); unit tests: [`host-harness/tests/README.md`](../drivers/windows7/tests/host-harness/tests/README.md) |
| VIO-020 | Remaining | Feature expansion for virtio devices (non-contract, optional): MSI/MSI-X, virtio-net offloads/TSO, virtio-snd **eventq message support** (contract v1 reserves the queue but defines no events) + multi-format negotiation, virtio-input additional HID coverage | N/A | Start from device-specific READMEs + the contract v1 doc: [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md) |

### Guest Tools Tasks

| ID | Status | Task | Where | CI/Guardrails |
|----|--------|------|-------|---------------|
| GT-001 | Implemented | Windows device contract docs + JSON manifest | Docs: [`windows-device-contract.md`](../docs/windows-device-contract.md) + [`windows-device-contract.json`](../docs/windows-device-contract.json) | [`windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml) (Rust validator) |
| GT-002 | Implemented | Guest Tools config generation (`guest-tools/config/devices.cmd`) from the contract | Generator: [`scripts/generate-guest-tools-devices-cmd.py`](../scripts/generate-guest-tools-devices-cmd.py); output: [`guest-tools/config/devices.cmd`](../guest-tools/config/devices.cmd) | [`.github/workflows/guest-tools-devices-cmd.yml`](../.github/workflows/guest-tools-devices-cmd.yml), [`gen-guest-tools-devices-cmd.py`](../scripts/ci/gen-guest-tools-devices-cmd.py), [`.github/workflows/windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml), [`check-windows-virtio-contract.py`](../scripts/ci/check-windows-virtio-contract.py) |
| GT-003 | Implemented | Guest Tools installer stages drivers, manages test-signing policy, and pre-seeds boot-critical virtio-blk (`CriticalDeviceDatabase`) | Installer: [`guest-tools/setup.cmd`](../guest-tools/setup.cmd) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (packages Guest Tools media) |
| GT-004 | Implemented | Enforce INF ↔ contract ↔ emulator consistency (virtio HWIDs, revision gating, service names) | [`check-windows7-virtio-contract-consistency.py`](../scripts/ci/check-windows7-virtio-contract-consistency.py) | [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml) (guardrails) |
| GT-005 | Implemented | Enforce virtio contract wiring (contract JSON ↔ INFs ↔ emulator PCI profiles ↔ guest-tools) | [`check-windows-virtio-contract.py`](../scripts/ci/check-windows-virtio-contract.py) | [`windows-virtio-contract.yml`](../.github/workflows/windows-virtio-contract.yml) |
| GT-006 | Implemented | Contract versioning + policy surfaced in docs and manifests | [`windows-device-contract.md`](../docs/windows-device-contract.md) | [`windows-device-contract.yml`](../.github/workflows/windows-device-contract.yml) |
| GT-007 | Implemented | Guest Tools packager (ISO/zip) + spec/config validation | `tools/packaging/aero_packager/`, `tools/packaging/specs/`, `tools/guest-tools/` | [`guest-tools-packager.yml`](../.github/workflows/guest-tools-packager.yml) |

### AeroGPU Driver Tasks

| ID | Status | Task | Where | Tests / docs to start with |
|----|--------|------|-------|----------------------------|
| AGPU-001 | Partial | AeroGPU KMD (WDDM 1.1 miniport) + D3D9Ex UMD bring-up (Aero composition + basic rendering) | `drivers/aerogpu/kmd/`, `drivers/aerogpu/umd/d3d9/` | UMD README (stub list): [`drivers/aerogpu/umd/d3d9/README.md`](../drivers/aerogpu/umd/d3d9/README.md); guest suite: [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md); host tests: `drivers/aerogpu/umd/d3d9/tests/` (CI: [`drivers-win7.yml`](../.github/workflows/drivers-win7.yml)) |
| AGPU-002 | Remaining | D3D9Ex “currently stubbed DDIs” → real implementations (fixed-function + state blocks + misc DDIs) | `drivers/aerogpu/umd/d3d9/` | Stub checklist: [`d3d9/README.md#currently-stubbed-ddis`](../drivers/aerogpu/umd/d3d9/README.md#currently-stubbed-ddis); Win7 suite: [`drivers/aerogpu/tests/win7/`](../drivers/aerogpu/tests/win7/) |
| AGPU-003 | Partial | D3D10/11 UMD feature expansion beyond “triangle” bring-up (pipeline/state coverage; Map/Unmap correctness; format support) | `drivers/aerogpu/umd/d3d10_11/` | README: [`drivers/aerogpu/umd/d3d10_11/README.md`](../drivers/aerogpu/umd/d3d10_11/README.md); checklist: [`docs/graphics/win7-d3d11ddi-function-tables.md`](../docs/graphics/win7-d3d11ddi-function-tables.md); Map/Unmap notes: [`docs/graphics/win7-d3d11-map-unmap.md`](../docs/graphics/win7-d3d11-map-unmap.md); host tests: `drivers/aerogpu/umd/d3d10_11/tests/` |
| AGPU-004 | Implemented | DXGI/D3D10/11 shared-resource interop (export/import shared surfaces, share-token plumbing) | `drivers/aerogpu/umd/d3d10_11/` | Design contract: [`docs/graphics/win7-shared-surfaces-share-token.md`](../docs/graphics/win7-shared-surfaces-share-token.md); Win7 IPC tests: `d3d10_shared_surface_ipc`, `d3d10_1_shared_surface_ipc`, `d3d11_shared_surface_ipc` |
| AGPU-005 | Implemented | AeroGPU Win7 guest-side validation suite (D3D9/D3D10/D3D11 + vblank/fence/ring probes) | `drivers/aerogpu/tests/win7/` | [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md) |
| AGPU-006 | Implemented | DX11-capable AeroGPU package is staged by CI (`aerogpu_dx11.inf` + WOW64 `aerogpu_d3d10.dll`) | `drivers/aerogpu/ci-package.json`, `drivers/aerogpu/packaging/win7/` | Packaging notes: [`drivers/aerogpu/packaging/win7/README.md`](../drivers/aerogpu/packaging/win7/README.md) (see “0) CI packages vs manual packaging”) |

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
│     - drivers/win7/virtio/virtio-core/                │
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
│  aerogpu_d3d9*.dll (+ optional aerogpu_d3d10*.dll) │  User-mode display drivers (UMDs)
│                 │                            │
│                 ▼                            │
│        aerogpu.sys (KMD)                    │  WDDM miniport driver
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
# Recommended (CI-like; builds + stages drivers under out/)
pwsh ci/install-wdk.ps1
pwsh ci/build-drivers.ps1 -ToolchainJson out/toolchain.json -Drivers aerogpu windows7/virtio-blk windows7/virtio-net windows7/virtio-input windows7/virtio-snd

# Note: CI only builds/packages drivers that explicitly opt in via `ci-package.json`
# under the driver directory (and have at least one `.inf`), to avoid accidentally
# shipping scaffolding/test drivers.

# Optional: generate catalogs + test-sign + bundle artifacts (Guest Tools ISO/zip, etc.)
pwsh ci/make-catalogs.ps1 -ToolchainJson out/toolchain.json
pwsh ci/sign-drivers.ps1 -ToolchainJson out/toolchain.json
pwsh ci/package-drivers.ps1
pwsh ci/package-guest-tools.ps1 -SpecPath tools/packaging/specs/win7-signed.json

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
- Run the in-tree Win7 validation suite (`drivers/aerogpu/tests/win7/`), preferably via `bin\\aerogpu_test_runner.exe` (see: [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md))

For virtio-blk:
- Verify disk appears in Disk Management
- Read/write test files
- Check performance (should be faster than emulated AHCI)

For virtio end-to-end regression testing (recommended):

- Guest selftest: `drivers/windows7/tests/guest-selftest/` (`aero-virtio-selftest.exe`)
- Host harness: `drivers/windows7/tests/host-harness/` (boots QEMU + parses serial markers)
  - Supports virtio-snd wav capture + non-silence verification when enabled.
  - See: [`drivers/windows7/tests/README.md`](../drivers/windows7/tests/README.md)

Example (Linux/macOS/Windows host; Python harness):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --timeout-seconds 600
```

Example: require end-to-end virtio-input **event delivery** (host QMP injects a deterministic keyboard/mouse sequence):

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --with-input-events \
  --timeout-seconds 600
```

Note: `--with-input-events` requires a guest image provisioned with virtio-input event testing enabled (so the guest selftest runs with `--test-input-events` / env var; otherwise the guest will emit `virtio-input-events|SKIP|flag_not_set` and the harness will fail). If the guest selftest is too old/misconfigured and does not emit any `virtio-input-events` marker at all after completing `virtio-input`, the harness will also fail early. See: [`drivers/windows7/tests/host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md).

Example: attach virtio-snd and capture deterministic wav output + verify non-silence:

```bash
python3 drivers/windows7/tests/host-harness/invoke_aero_virtio_win7_tests.py \
  --qemu-system qemu-system-x86_64 \
  --disk-image ./win7-aero-tests.qcow2 \
  --snapshot \
  --with-virtio-snd \
  --virtio-snd-audio-backend wav \
  --virtio-snd-wav-path ./out/virtio-snd.wav \
  --virtio-snd-verify-wav \
  --timeout-seconds 600
```

Note: `--with-virtio-snd` requires a guest image provisioned with virtio-snd capture/duplex selftests enabled (otherwise the harness will fail on `virtio-snd-duplex|SKIP|flag_not_set`). See: [`drivers/windows7/tests/host-harness/README.md`](../drivers/windows7/tests/host-harness/README.md).

---

## Quick Start Checklist

1. ☐ Read [`AGENTS.md`](../AGENTS.md) completely
2. ☐ Read [`docs/windows/README.md`](../docs/windows/README.md)
3. ☐ Read [`docs/windows7-virtio-driver-contract.md`](../docs/windows7-virtio-driver-contract.md)
4. ☐ Read [`docs/windows-device-contract.md`](../docs/windows-device-contract.md)
5. ☐ Read [`docs/windows7-guest-tools.md`](../docs/windows7-guest-tools.md) (how the drivers are installed/switch-over order)
6. ☐ Read [`drivers/windows7/tests/README.md`](../drivers/windows7/tests/README.md) (selftest + harness)
7. ☐ Read [`drivers/aerogpu/tests/win7/README.md`](../drivers/aerogpu/tests/win7/README.md) (AeroGPU guest validation suite)
8. ☐ Run contract checks locally (`python3 scripts/ci/check-windows7-virtio-contract-consistency.py`, `python3 scripts/ci/check-windows-virtio-contract.py --check`, `python3 scripts/ci/gen-guest-tools-devices-cmd.py --check`)
9. ☐ Set up Windows build environment (WDK)
10. ☐ Explore `drivers/aerogpu/` and `drivers/windows7/`
11. ☐ Build existing drivers to verify toolchain
12. ☐ Pick a task from the tables above and begin

---

*These drivers make the emulator fast. Virtio provides 10-100x speedup over full emulation.*
