# 21 - `crates/emulator` → canonical `aero-machine` stack migration plan

## Context / goal

The repo historically treated `crates/emulator` as “the place where the VM lives” (device models + PCI
wiring + chipset glue). After:

- [ADR 0008: canonical VM core](./adr/0008-canonical-vm-core.md)
- [ADR 0014: canonical machine stack](./adr/0014-canonical-machine-stack.md)

…the **only canonical VM wiring layer** is `crates/aero-machine` (`aero_machine::Machine`). New code
should not build new “machine integration” surfaces on top of `crates/emulator`.

This document is the repo’s source of truth for:

1. Which `crates/emulator/src/*` subsystems are **deprecated** and what their **canonical
   replacement** crate(s) are.
2. The concrete **deletion/extraction targets** inside `crates/emulator`.
3. A small-PR **phased plan** to converge on the canonical stack without a flag day.

Related docs:

- VM wiring map: [`docs/vm-crate-map.md`](./vm-crate-map.md)
- Canonical USB stack: [ADR 0015](./adr/0015-canonical-usb-stack.md)
- Canonical audio stack: [ADR 0010](./adr/0010-canonical-audio-stack.md)
- Storage consolidation plan: [`docs/20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md)

---

## Canonical “machine wiring” story (one answer)

If you are building “a VM that boots” (tests, WASM exports, host runtime), the canonical crate graph
starts from:

- `crates/aero-machine` ([`src/lib.rs`](../crates/aero-machine/src/lib.rs)) — `aero_machine::Machine`

and composes device/platform building blocks from:

- `crates/platform` ([`src/lib.rs`](../crates/platform/src/lib.rs)) — buses + chipset/reset + interrupt routing
- `crates/devices` ([`src/lib.rs`](../crates/devices/src/lib.rs)) — reusable device models + PCI infrastructure
- `crates/aero-pc-platform` ([`src/lib.rs`](../crates/aero-pc-platform/src/lib.rs)) — helper for PC platform composition (expected to fold into `aero-machine` over time)

Subsystem crates plug into that stack (VGA/VBE, USB, storage, networking, …).

Guardrail (policy):

- **Canonical crates must not depend on `crates/emulator`** (except as a *dev-dependency* for
  transitional conformance harnesses).
- `crates/emulator` may depend on canonical crates, but should only host *compatibility shims* and
  *remaining unique implementations* (tracked below).
- CI enforces the “no emulator dependency edges” rule for workspace crates via:
  - `scripts/ci/check-no-emulator-deps.py` (invoked by `scripts/ci/check-repo-layout.sh`)

---

## Subsystem map: `crates/emulator/src/*` → canonical replacement

This section is intentionally explicit. If you touch one of the emulator subsystems listed here, the
expected outcome is either:

- “move the code to the canonical crate”, or
- “delete the emulator code and use the canonical crate”, or
- “keep it temporarily, but only as a compatibility shim”.

### VGA / VBE (legacy display)

**Emulator (deprecated/compat-only)**

- Legacy VGA/VBE implementation: historically lived under `crates/emulator/src/devices/vga/*`.
  That copy has been removed in favour of the canonical `aero-gpu-vga` crate.
- Compatibility re-export + small glue helpers:
  - `crates/emulator/src/devices/vga.rs`
- Compatibility re-export for the shared framebuffer ABI (source of truth lives in `aero-shared`):
  - `crates/emulator/src/display/mod.rs`

**Canonical replacement**

- `crates/aero-gpu-vga` ([`src/lib.rs`](../crates/aero-gpu-vga/src/lib.rs)) — canonical VGA/VBE device model
- `crates/aero-machine` ([`src/lib.rs`](../crates/aero-machine/src/lib.rs)) — canonical port/MMIO routing when `MachineConfig::enable_vga=true`

**Deletion targets (in `crates/emulator`)**

- `src/devices/vga.rs` (compat wrapper), once no callers need the legacy `emulator::devices::vga::*` paths
- `src/display/`
- VGA/VBE-focused emulator tests once equivalents exist in the canonical stack:
  - `tests/vga_smoke.rs`
  - `tests/vbe*`

### PCI / interrupts / “platform wiring”

This is the biggest source of architectural confusion: historically, `crates/emulator` looked like a
top-level “machine wiring” layer because it contained bespoke PCI + APIC + port/MMIO routing.

**Emulator (deprecated)**

- Emulator-local PCI framework / BAR routing (legacy; do not extend):
  - `crates/emulator/src/io/pci.rs`
  - `crates/emulator/src/devices/pci/mod.rs` (excluding the AeroGPU device model; see below)

Note: emulator-local chipset/APIC wiring that previously lived under `crates/emulator/src/*` has been
migrated to the canonical `aero-machine`/`aero-pc-platform`/`aero-interrupts` stack and no longer
exists in the emulator crate. The remaining emulator-local PCI code is still a potential source of
confusion and should not grow.

**Canonical replacement**

- `crates/aero-machine` — owns the top-level machine + port/MMIO routing
- `crates/aero-pc-platform` — PC platform composition helper (PIC/PIT/RTC/PCI/APIC/HPET/ECAM)
- `crates/platform` — canonical I/O bus + interrupt router + reset/chipset state
- `crates/devices` — canonical PCI and device model layer

(Note: the canonical APIC implementation lives in `crates/aero-interrupts`; it is consumed by
`aero-machine` / `aero-pc-platform` and should be treated as part of the canonical platform stack.)

**Deletion targets (in `crates/emulator`)**

- `src/io/pci.rs`
- `src/devices/pci/mod.rs` (after AeroGPU is extracted; see “Unique remaining pieces”)

### Storage traits + formats + controller integration

This is covered in detail by [`docs/20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).
This section exists to make the **emulator deletion targets** explicit.

**Emulator (deprecated)**

- Storage traits + adapters + image formats + controller models:
  - `crates/emulator/src/io/storage/*`

**Canonical replacement**

- `crates/aero-storage` — storage traits + disk image formats
- `crates/aero-storage-adapters` — shared wrapper types around `aero_storage::VirtualDisk`
- `crates/aero-devices-storage` — canonical ATA/ATAPI/IDE/AHCI device/controller models
- `crates/aero-devices-nvme` — canonical NVMe controller model

**Deletion targets (in `crates/emulator`)**

- `src/io/storage/` (entire directory), once all call sites use:
  - `aero_storage::{StorageBackend, VirtualDisk}`
  - `aero_devices_storage::*` and `aero_devices_nvme::*`
- Emulator-only legacy traits once unused (see storage doc Phase 3):
  - `src/io/storage/disk.rs` (legacy `ByteStorage` / `DiskBackend`)

### USB (device models + host controller integration)

USB selection is governed by [ADR 0015](./adr/0015-canonical-usb-stack.md).

**Emulator (deprecated/compat-only)**

- `crates/emulator` currently keeps a compatibility module at `src/io/usb/` that re-exports the
  canonical USB device models and adds legacy integration glue.
  - Note: legacy PCI/MMIO wrappers for EHCI/xHCI are intentionally feature-gated in the emulator
    crate (`legacy-usb-ehci`, `legacy-usb-xhci`) so new code does not accidentally depend on the
    legacy wiring layer.

**Canonical replacement**

- USB device models + host controllers (UHCI/EHCI/xHCI): `crates/aero-usb` ([`src/lib.rs`](../crates/aero-usb/src/lib.rs))
- UHCI PCI device wrapper (canonical PCI stack): `crates/devices/src/usb/uhci.rs` ([`uhci.rs`](../crates/devices/src/usb/uhci.rs))
- EHCI PCI device wrapper (canonical PCI stack): `crates/devices/src/usb/ehci.rs` ([`ehci.rs`](../crates/devices/src/usb/ehci.rs))
- xHCI PCI device wrapper (canonical PCI stack): `crates/devices/src/usb/xhci.rs` ([`xhci.rs`](../crates/devices/src/usb/xhci.rs))

See also controller contracts:

- EHCI (USB 2.0): [`docs/usb-ehci.md`](./usb-ehci.md)
- xHCI (USB 3.x): [`docs/usb-xhci.md`](./usb-xhci.md)

**Deprecation/deletion targets (in `crates/emulator`)**

- Any *standalone* UHCI PCI wrapper / port I/O wiring that duplicates `aero-devices`:
  - `src/io/usb/uhci.rs` (once the emulator is no longer using its bespoke PCI framework)
- Legacy emulator-specific PCI/MMIO wrappers for `aero-usb` controllers:
  - `src/io/usb/ehci.rs` (feature-gated by `legacy-usb-ehci`)
  - `src/io/usb/xhci.rs` (feature-gated by `legacy-usb-xhci`)
- Any USB “wire contracts” that are not owned by `aero-usb` (do not introduce new ones).

### Networking backend (L2 tunnel + pumping)

**Emulator (compat-only)**

- Compatibility re-exports for the host/worker network backend traits and L2 tunnel backends:
  - `crates/emulator/src/io/net/*`

**Canonical replacement**

- Backend traits + L2 tunnel implementations: `crates/aero-net-backend` ([`src/lib.rs`](../crates/aero-net-backend/src/lib.rs))
- Deterministic device “pump” helpers (e.g. ticking NICs against a backend): `crates/aero-net-pump` ([`src/lib.rs`](../crates/aero-net-pump/src/lib.rs))

**Deletion targets (in `crates/emulator`)**

- Any non-trivial networking logic that duplicates `aero-net-*` crates should move out. The intended
  end state for `crates/emulator` is that `src/io/net/*` is *either* deleted outright *or* remains as
  thin re-exports only.

### Audio (HDA/AC97/device bridges)

Audio stack selection is governed by [ADR 0010](./adr/0010-canonical-audio-stack.md).

**Emulator (legacy; feature-gated)**

- Legacy audio device models and host bridges (AC97/HDA/DSP + AudioWorklet glue), gated by
  `--features legacy-audio`:
  - `crates/emulator/src/audio/*`
  - `crates/emulator/src/io/audio/*`

**Canonical replacement**

- HDA device model + PCM helpers: `crates/aero-audio` (`aero_audio::hda`)
- virtio-snd device model: `crates/aero-virtio` (`aero_virtio::devices::snd`)
- AudioWorklet + microphone SharedArrayBuffer bridges: `crates/platform` (`aero_platform::audio::*`)
- Browser/WASM exports for wiring audio devices into the worker runtime: `crates/aero-wasm`

**Deletion targets (in `crates/emulator`)**

- `src/audio/`
- `src/io/audio/`
- Legacy audio benches/tests that require `--features legacy-audio` once equivalent coverage exists
  in the canonical crates.

---

## Unique remaining pieces that should stay (for now)

The following `crates/emulator` subsystems are currently “unique” (not yet represented in the
canonical stack) and should be treated as explicit extraction/integration projects rather than
quietly accreting more responsibilities.

### 1) AeroGPU PCI device model (guest-visible GPU)

**Current owner (in `crates/emulator`)**

- PCI device model:
  - `crates/emulator/src/devices/pci/aerogpu.rs`
- Supporting files (regs/ring/scanout helpers):
  - `crates/emulator/src/devices/aerogpu_*.rs`
  - `crates/emulator/src/devices/aerogpu_scanout.rs`, `aerogpu_ring.rs`, `aerogpu_regs.rs`

**Intended canonical home**

- Target: a first-class device model under the canonical device layer:
  - Preferred: `crates/devices` (as `aero_devices::pci::aerogpu::*`)
  - If it is too large to live in `crates/devices`, create `crates/aero-devices-aerogpu` and have
    `crates/devices` depend on it (matching the existing `aero-devices-nvme` pattern).

**Integration plan**

- `aero_machine::Machine` now exposes an optional AeroGPU PCI function behind
  `MachineConfig::enable_aerogpu` (`A3A0:0001` at `00:07.0`) using the canonical PCI stack:
  - BAR1-backed VRAM + legacy VGA/VBE decode (boot display foundation), and
  - a minimal BAR0 ring/fence transport stub (no-op command execution).

  The remaining integration work is wiring the **full** AeroGPU device model (command execution +
  scanout + vblank pacing), which still lives in `crates/emulator`.
- Keep the driver/ABI contract anchored to:
  - `drivers/aerogpu/protocol/*` (source of truth)
  - `emulator/protocol` (Rust/TS mirror)

### 2) GPU worker + command executor wiring

**Current owner (in `crates/emulator`)**

- `crates/emulator/src/gpu_worker/*`

**Intended canonical home**

- The long-term “GPU worker runtime” should live next to the GPU executor and shared protocols:
  - Candidate: fold into `crates/aero-gpu` / `crates/aero-gpu-wasm` as an explicit worker-oriented
    module, or extract a new `crates/aero-gpu-worker` crate if it needs non-GPU dependencies.

**Integration plan**

- `aero-machine` should expose a stable, explicit boundary for “GPU host actions” (shared-memory ring
  protocol or direct callbacks), so the GPU worker is not implicitly coupled to `crates/emulator`.

### 3) SMP (multi-vCPU) model

**Current owner**

- `crates/aero-smp-model/*` (`SmpMachine`, vCPU scheduler, local APIC IPI delivery)
  - This crate was extracted out of `crates/emulator` to avoid collisions/confusion with the
    canonical `aero_machine::Machine`.
  - `crates/emulator` retains a feature-gated legacy re-export (`emulator::smp` behind
    `--features legacy-smp-model`) for compatibility, but it is intentionally not built/exposed by
    default.

**Intended canonical home**

- Target: `crates/aero-machine` evolves from the current **SMP bring-up scaffolding** (per-vCPU LAPIC
  state/MMIO, INIT/SIPI, cooperative AP execution) into a full multi-vCPU execution environment by
  adopting (or re-implementing) the SMP scheduling and APIC-delivery logic behind a stable API.
  See [`docs/21-smp.md`](./21-smp.md) for the up-to-date gap list.
- If the SMP code needs to be reusable independently of `aero-machine`, `crates/aero-smp-model`
  already provides a minimal deterministic model suitable for unit tests and snapshot validation.

---

## Phased plan (bite-sized PR milestones)

This is intentionally a sequence of small PRs (mirrors the style of the storage consolidation doc).

1. **Phase 0 (this PR): documentation + guardrails**
   - Add this doc.
   - Add/refresh `crates/emulator/README.md` to clearly mark the crate as non-canonical.
   - Update `docs/vm-crate-map.md` / `docs/repo-layout.md` to link here so the repo has exactly one
     canonical VM wiring story.

2. **VGA/VBE: converge on `aero-gpu-vga`**
   - Move/duplicate any missing VGA/VBE tests from `crates/emulator/tests` into `crates/aero-gpu-vga`
     (unit tests) and/or `crates/aero-machine` (integration/boot tests).
   - Delete `crates/emulator/src/devices/vga/` and `crates/emulator/src/display/` once coverage exists.

3. **USB: converge UHCI wiring on the canonical PCI stack**
   - Ensure UHCI PCI wiring lives only in `crates/devices/src/usb/uhci.rs`.
   - Convert any remaining emulator-specific UHCI wrapper/tests to use the canonical device model.
   - Delete the emulator-local UHCI PCI wrapper (`src/io/usb/uhci.rs`) once unused.

4. **Storage: complete the migration described in `docs/20-*`**
   - Replace emulator-only disk traits with `aero_storage::{StorageBackend, VirtualDisk}`.
   - Move remaining controller logic into `aero-devices-storage` / `aero-devices-nvme`.
   - Delete `crates/emulator/src/io/storage/` once no longer provides unique behavior.

5. **Networking: keep `crates/emulator` as re-export-only**
   - Ensure all logic lives in `aero-net-backend` + `aero-net-pump` (and device models in their
     dedicated crates, e.g. `aero-net-e1000`).
   - Reduce `crates/emulator/src/io/net/*` to thin compatibility shims (or delete entirely).

6. **Platform wiring: delete the emulator’s bespoke PCI/interrupt framework**
   - Move remaining PCI/APIC/interrupt glue into `aero-pc-platform` and/or directly into
     `aero-machine`.
   - Delete `crates/emulator/src/chipset.rs`, `src/memory_bus.rs`, `src/io/pci.rs`,
     `src/devices/{ioapic,lapic}.rs`.

7. **AeroGPU extraction**
    - Extract the AeroGPU PCI device model out of `crates/emulator` into the canonical device layer
      (`crates/devices` or a new `crates/aero-devices-aerogpu`).
    - Replace `aero_machine`’s current BAR0 stub with the extracted full device model (command
      execution + scanout + vblank pacing) behind `MachineConfig::enable_aerogpu`.

8. **SMP integration decision**
    - Define a canonical SMP story for `aero-machine` (multi-vCPU API, scheduling model, snapshot
      story).
    - Either integrate/adapt the existing `crates/aero-smp-model` code or replace it with a new
      implementation in the canonical stack.

9. **End state: remove `crates/emulator` (optional)**
   - Once the only remaining code is compatibility glue, either:
     - delete the crate, or
     - rename it to make “legacy/compat” explicit (e.g. `aero-emulator-compat`) and keep it out of
       the canonical dependency graph.
