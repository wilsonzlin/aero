# 21 - SMP / Multi-vCPU Bring-up

## Status (today)

The canonical machine integrations:

- `aero_machine::Machine` (`crates/aero-machine`)
- `aero_machine::PcMachine` (`crates/aero-machine`)
- `aero_pc_platform::PcPlatform` (`crates/aero-pc-platform`)

are currently **BSP-only execution**: they execute only vCPU0 and do not yet schedule application
processors (APs) end-to-end.

`cpu_count` is allowed to be `>= 1` so firmware can publish SMP-capable CPU topology (ACPI/SMBIOS)
and platform code can size per-vCPU state (for example LAPIC instances), but a multi-vCPU guest will
not actually run correctly yet.

Attempting to construct a canonical machine with `cpu_count == 0` fails with
`MachineError::InvalidCpuCount`.

## Why SMP is disabled in the canonical machine/platform

The project has building blocks for multi-vCPU guests (ACPI table generation can emit multiple CPU
entries, and there is a prototype SMP model in `crates/aero-smp-model/`), but the end-to-end
full-system wiring is not complete yet.

Key missing pieces include:

1. **Multiple vCPU execution + scheduling**
   - The canonical machine owns a single `aero_cpu_core::CpuCore`.
   - SMP requires `Vec<CpuCore>` plus a scheduler (deterministic time-slicing baseline; optional
     multi-worker execution later).

2. **AP startup (BSP + AP bring-up)**
   - APs must start in a wait-for-SIPI state.
   - BSP must be able to deliver INIT/SIPI via the Local APIC ICR.
   - Requires a low-memory trampoline region and the associated memory/firmware contract.

3. **LAPIC/IPI plumbing**
   - `PcPlatform` currently models one Local APIC for interrupt delivery to the single BSP.
   - SMP requires per-vCPU LAPIC state and IPI delivery between vCPUs.

4. **Firmware topology and OS discovery**
   - ACPI MADT must enumerate all CPUs and their APIC IDs.
   - DSDT must include `_PR_` processor objects for all CPUs.
   - SMBIOS should report the correct CPU/core count.
   - These pieces exist in `aero-acpi`/`firmware` and can be generated for `cpu_count > 1`, but
     multi-vCPU execution is not wired end-to-end yet.

5. **Snapshot/restore**
   - `aero-snapshot` supports multi-vCPU `CPUS` state, but the canonical machine currently snapshots
     a single CPU.
   - SMP bring-up must define the per-vCPU snapshot contract and deterministic restore ordering.

## Where to track progress

- This doc (`docs/21-smp.md`) is the canonical “what’s missing / what’s next” reference.
- Prototype SMP plumbing lives in `crates/aero-smp-model/` (APIC IPI + AP startup state machine).
  - For backwards compatibility it can also be accessed via `emulator::smp` when
    `--features legacy-smp-model` is enabled, but it is intentionally **not** part of the default
    `crates/emulator` API surface.
- The higher-level roadmap mentions multi-core as a Windows 7 compatibility milestone:
  - `docs/14-project-milestones.md` → “Phase 4 … Multi-core / SMP emulation”
- Firmware-side conceptual background (not necessarily implemented end-to-end yet):
  - `docs/09-bios-firmware.md` → “SMP Boot (BSP + APs)”

## Recommended workarounds (until SMP lands)

- **For real guest boots today, use `cpu_count = 1`** for canonical machines/platforms.
- For faster “boot to desktop” workflows, use snapshots (`docs/16-snapshots.md`) rather than relying
  on multi-core to speed up boot.
- If you specifically want to experiment with AP startup/IPI logic (without full PCI/BIOS/Windows),
  look at the unit-test-oriented SMP prototype in `crates/aero-smp-model/`.
