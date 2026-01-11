# CPU: CPUID & Feature Policy

Windows 7 boot and many drivers gate behavior based on CPUID leaves and feature bits. If we expose **inconsistent** or **overly optimistic** CPUID information, the guest may:

- take an instruction path we do not implement (e.g. SSE4.2/AVX),
- enable paging features we don’t support (e.g. NX without EFER.NXE behavior),
- or fail early during boot due to missing mandatory capabilities.

This repository models CPUID + MSR behavior in `crates/aero-cpu-core`, exposing a coherent x86-64 feature surface for a Windows 7 guest.

## Implemented CPUID Leaves

The CPUID dispatcher in `crates/aero-cpu-core/src/cpuid.rs` implements common leaves used by Windows 7 and typical drivers:

- `0x0000_0000` – vendor string + max basic leaf
- `0x0000_0001` – signature + baseline feature flags
- `0x0000_0002` – legacy cache/TLB descriptors (QEMU-like constants)
- `0x0000_0004` – deterministic cache parameters (simple L1/L2/L3 model)
- `0x0000_0006` – thermal/power (stubbed as 0)
- `0x0000_0007` – extended features (subleaf 0)
- `0x0000_000A` – perf monitoring (stubbed as 0)
- `0x0000_000B` / `0x0000_001F` – topology enumeration (only exposed when `x2APIC` is enabled)
- `0x8000_0000` – max extended leaf
- `0x8000_0001` – extended feature flags (NX/SYSCALL/LM/LAHF-LM, etc.)
- `0x8000_0002..=0x8000_0004` – brand string
- `0x8000_0006` – extended cache info (L2)
- `0x8000_0007` – invariant TSC
- `0x8000_0008` – physical/virtual address sizes

Unknown leaves return 0 (safe default for bring-up).

## Feature Policy (What We Advertise)

The CPU feature surface is modeled in the same “shape” as CPUID via `CpuFeatureSet`:

- `CPUID.1:ECX` → `CpuFeatureSet::leaf1_ecx`
- `CPUID.1:EDX` → `CpuFeatureSet::leaf1_edx`
- `CPUID.7.0:*` → `CpuFeatureSet::leaf7_*`
- `CPUID.80000001:*` → `CpuFeatureSet::ext1_*`

This keeps it explicit *where* a feature bit is reported, which helps avoid accidental inconsistencies.

### Profiles

`CpuProfile` (see `crates/aero-cpu-core/src/cpuid.rs`) defines two intended configurations:

1. **Win7Minimum** – minimum viable x86-64 CPU for Windows 7 boot:
   - x86-64 / long mode (`LM`)
   - `SSE2`
   - `PAE`
   - `NX`
   - `APIC`
   - `TSC`
   - `SYSCALL/SYSRET`
   - `LAHF/SAHF` in long mode
   - `CMPXCHG16B`

2. **Optimized** – allows additional feature bits (SSE3/SSSE3/SSE4.2/POPCNT, etc.) **only when the emulator implements them**.

### Overrides

`CpuFeatureOverrides` can force-enable/disable specific CPUID bits for debugging.

By default, `force_enable` is still capped by the `implemented_features` set (we don’t advertise what we can’t execute).

Setting `allow_unsafe = true` allows forcing bits that are not implemented, which is useful for bring-up experiments but is expected to break guests.

## CPUID/MSR Coherence

Some CPUID bits imply MSR behavior. The crate enforces the most important ones for Windows boot:

- If `CPUID.80000001:EDX[NX]` is cleared, writes to `EFER.NXE` are masked.
- If `CPUID.80000001:EDX[SYSCALL]` is cleared, writes to `EFER.SCE` are masked.
- If `CPUID.80000001:EDX[LM]` is cleared, writes to `EFER.LME` are masked.

Unit tests in `crates/aero-cpu-core/tests/cpuid_policy.rs` validate these coherency rules.
