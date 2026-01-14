# 21 - SMP / Multi-vCPU Bring-up

## Status (today)

The canonical machine integrations:

- `aero_machine::Machine` (`crates/aero-machine`)
- `aero_machine::PcMachine` (`crates/aero-machine`)
- `aero_pc_platform::PcPlatform` (`crates/aero-pc-platform`)

are still **SMP bring-up only**, but progress has landed:

- `aero_machine::Machine` can be configured with `cpu_count > 1` and includes basic SMP plumbing:
  per-vCPU LAPIC instances/MMIO routing, AP wait-for-SIPI state, INIT+SIPI delivery via the LAPIC
  ICR, and a bounded cooperative AP execution loop inside `Machine::run_slice`.
  This is sufficient for SMP contract/bring-up tests, but it is **not** a full SMP scheduler or
  parallel vCPU execution environment yet.
  - Tests/coverage: `crates/aero-machine/tests/ap_tsc_sipi_sync.rs`, `lapic_mmio_per_vcpu.rs`,
    `ioapic_routes_to_apic1.rs`, `smp_lapic_timer_wakes_ap.rs`, `smp_timer_irq_routed_to_ap.rs`.
- `aero_machine::PcMachine` and `aero_pc_platform::PcPlatform` still execute only the BSP today;
  `cpu_count > 1` there is primarily for firmware-table enumeration tests.
- Lower-level interrupt-fabric SMP semantics are covered in `crates/platform-compat/tests/smp_*`
  (INIT deassert IPI behavior, IOAPIC destination routing, and MSI destination/broadcast routing).

`cpu_count` is allowed to be `>= 1` so firmware can publish SMP-capable CPU topology (ACPI/SMBIOS)
and platform code can size per-vCPU state (for example LAPIC instances). Multi-vCPU guests are not
expected to run robustly yet (especially OS SMP), but the building blocks are now testable.

Attempting to construct a canonical machine with `cpu_count == 0` fails with
`MachineError::InvalidCpuCount`.

## Why SMP is disabled in the canonical machine/platform

The project has building blocks for multi-vCPU guests (ACPI table generation can emit multiple CPU
entries, and there is a prototype SMP model in `crates/aero-smp/`), but the end-to-end
full-system wiring is not complete yet.

Key missing pieces include (what’s still left even with the current bring-up support):

1. **Robust multi-vCPU execution + scheduling**
   - `aero_machine::Machine` now owns a BSP `CpuCore` plus AP `CpuCore`s and runs APs cooperatively,
     but this is still a minimal bring-up scheduler.
   - Remaining work includes fairness, guest-driven AP execution (not just host-driven bring-up),
     and eventually parallel execution.

2. **AP startup (BSP + AP bring-up)**
   - APs must start in a wait-for-SIPI state.
   - BSP must be able to deliver INIT/SIPI via the Local APIC ICR.
   - Basic INIT/SIPI bring-up is now implemented in `aero_machine::Machine`, but full guest OS AP
     bring-up sequences still need hardening and coverage.

3. **LAPIC/IPI plumbing**
    - Per-vCPU LAPIC state exists and IOAPIC destination routing works for non-BSP LAPICs.
    - Remaining work includes full IPI delivery semantics (AP→BSP/AP, broadcast modes), per-vCPU
      interrupt polling/injection hardening (beyond bring-up), and safety/determinism under nested
      interrupt activity.

4. **Firmware topology and OS discovery**
   - ACPI MADT must enumerate all CPUs and their APIC IDs.
   - DSDT must include `_PR_` processor objects for all CPUs.
   - SMBIOS should report the correct CPU/core count.
   - These pieces exist in `aero-acpi`/`firmware` and can be generated for `cpu_count > 1`, but
     multi-vCPU execution is not wired end-to-end yet.

5. **Snapshot/restore**
   - `aero-snapshot` supports multi-vCPU `CPUS` state, but `aero_machine::Machine` snapshots/restores
     only the BSP CPU state today. AP CPU state + LAPIC/IPI state must be included in a stable,
     deterministic multi-vCPU snapshot contract.
   - SMP bring-up must define the per-vCPU snapshot contract and deterministic restore ordering.

## Where to track progress

- This doc (`docs/21-smp.md`) is the canonical “what’s missing / what’s next” reference.
- Prototype SMP plumbing lives in `crates/aero-smp/` (APIC IPI + AP startup state machine).
  - For backwards compatibility it can also be accessed via `emulator::smp` (a pure re-export shim).
- The higher-level roadmap mentions multi-core as a Windows 7 compatibility milestone:
  - `docs/14-project-milestones.md` → “Phase 4 … Multi-core / SMP emulation”
- Firmware-side conceptual background (not necessarily implemented end-to-end yet):
  - `docs/09-bios-firmware.md` → “SMP Boot (BSP + APs)”

## Recommended workarounds (until SMP lands)

- **For real guest boots today, use `cpu_count = 1`** for canonical machines/platforms.
- For faster “boot to desktop” workflows, use snapshots (`docs/16-snapshots.md`) rather than relying
  on multi-core to speed up boot.
- If you specifically want to experiment with AP startup/IPI logic (without full PCI/BIOS/Windows),
  look at the unit-test-oriented SMP prototype in `crates/aero-smp/`.
