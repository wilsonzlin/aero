# Aero: Windows 7 Browser Emulator - Coordination Document

> **Project:** Aero
> **Target:** Windows 7 SP1 (32-bit and 64-bit) running performantly in modern web browsers
> **Scope:** Complete x86/x86-64 system emulation with GPU acceleration

---

## 🛡️ Defensive Mindset (Read This First)

**Assume all code and processes can misbehave.** Every subprocess you spawn might:

- Hang forever
- Consume infinite memory
- Spin the CPU endlessly
- Leave zombie children
- Ignore SIGTERM
- Corrupt shared state

**You are not debugging your code. You are operating in a hostile environment where your own code, dependencies, and the system itself may fail in arbitrary ways.**

This mindset applies to:

- Cargo/rustc (can OOM, hang on codegen, deadlock on locks)
- npm/node (can leak memory, hang on network, spawn zombie processes)
- Playwright/Chrome (can hang, crash, leak GPU memory)
- Any I/O operation (can block indefinitely, return garbage, timeout inconsistently)

**Default behaviors:**

1. **Always use timeouts** — no command runs without a deadline
2. **Always use `-k` with timeout** — SIGKILL after grace period (misbehaving code ignores SIGTERM)
3. **Always use memory limits** — no build runs without a ceiling
4. **Always check exit codes** — success is never assumed
5. **Always capture stderr** — silent failures are unacceptable
6. **Kill aggressively** — SIGTERM, wait briefly, SIGKILL

---

## 📀 Windows 7 Test ISO

A Windows 7 Professional x64 ISO is available for integration testing:

```
/state/win7.iso
```

Use this for:

- Boot testing once the emulator reaches that stage
- Driver installation testing
- Integration test fixtures

**Do not redistribute or commit this file.** See `[LEGAL.md](./LEGAL.md)` for licensing.

---

## ⚠️ Memory Limits for Concurrent Execution

**If running many agents concurrently**, enforce **memory limits**. CPU and disk I/O contention are handled fine by the Linux scheduler, but memory exhaustion will OOM-kill the host.

**The one rule:** Use `bash ./scripts/safe-run.sh <command>` for all non-trivial operations. It enforces both timeout (10 min default) and memory limit (12G default).

```bash
bash ./scripts/agent-env-setup.sh               # One-time: validate environment
source ./scripts/agent-env.sh                   # Activate recommended env vars
bash ./scripts/safe-run.sh cargo build --locked  # Build with timeout + memory limit
bash ./scripts/safe-run.sh cargo test --locked   # Test with timeout + memory limit

  # Override defaults if needed:
  #   - AERO_TIMEOUT / AERO_MEM_LIMIT apply to safe-run's timeout + RLIMIT_AS wrapper.
  #   - AERO_CARGO_BUILD_JOBS controls Cargo parallelism (defaults to -j1 for reliability in constrained sandboxes).
  #   - RUST_TEST_THREADS controls Rust's built-in test harness parallelism (libtest). Agent helper scripts default it to CARGO_BUILD_JOBS for reliability under tight thread limits.
  #   - NEXTEST_TEST_THREADS controls cargo-nextest test concurrency (nextest has its own runner parallelism, separate from RUST_TEST_THREADS).
  #   - AERO_TOKIO_WORKER_THREADS controls Tokio runtime worker threads for supported Aero binaries (e.g. aero-l2-proxy, disk-gateway). Agent helper scripts default it to CARGO_BUILD_JOBS for reliability under tight thread limits.
  #   - AERO_ISOLATE_CARGO_HOME=1 isolates Cargo registry/cache state per checkout to avoid "Blocking waiting for file lock on package cache" on shared hosts.
  #     - `scripts/safe-run.sh` will also create `./.cargo-home` automatically when it detects that lock contention, and will auto-use it on future runs as long as you're using the default Cargo home (`$HOME/.cargo`).
  #   - AERO_RUST_CODEGEN_UNITS (alias: AERO_CODEGEN_UNITS) controls rustc per-crate codegen parallelism (`-C codegen-units=<n>`) without manually editing RUSTFLAGS.
  #   - AERO_SAFE_RUN_RUSTC_RETRIES controls how many times safe-run will retry transient rustc thread/process spawn failures (EAGAIN/WouldBlock), including panics like:
  #       - `failed to spawn helper thread (WouldBlock)`
  #       - `called Result::unwrap() on an Err value: Os { code: 11, kind: WouldBlock, message: "Resource temporarily unavailable" }`
AERO_TIMEOUT=1200 AERO_MEM_LIMIT=16G AERO_CARGO_BUILD_JOBS=2 bash ./scripts/safe-run.sh cargo build --release --locked

# Full workspace builds/tests can take longer than safe-run's default 10 minute timeout (especially
# on cold caches), so prefer a larger timeout when running `--workspace`:
AERO_TIMEOUT=3600 bash ./scripts/safe-run.sh cargo test --workspace --locked
```

Troubleshooting (some agent environments lose executable bits and/or tracked fixtures):

- If you get `Permission denied` running `./scripts/*.sh`, run via bash: `bash ./scripts/safe-run.sh …` (same for `agent-env-setup.sh`).
- If `git status` shows lots of changes that are only mode changes, or tracked files appear deleted/empty, restore the checkout:
  - `git checkout -- .` (bigger hammer), or at least:
  - `git checkout -- scripts tools/packaging/aero_packager/testdata tools/disk-streaming-browser-e2e/fixtures`
  - Non-git fallback: `find scripts -name '*.sh' -exec chmod +x {} +`
- If a `git pull`/`git checkout` gets interrupted (e.g. command timeout), you can end up with a partially-updated working tree (lots of modified tracked files) even though you didn't edit anything:
  - If you have no local work to keep: `git fetch origin && git reset --hard origin/main`
  - Otherwise: `git stash`, then reset, then re-apply the stash.
- If Git fails with a stale auto-GC lock (e.g. `fatal: Unable to create '.git/gc.pid.lock': File exists`), remove the lock file and retry:
  - `rm -f .git/gc.pid.lock`
- When scripting `git commit -m ...` from a shell, avoid backticks in the commit message (they trigger command substitution). Prefer single quotes:
  - `git commit -m 'chore: message with `code` formatting'` will *expand* the backticked section in most shells; use `git commit -m 'chore: message with code formatting'` or escape backticks.

Or use `timeout -k` directly (always include `-k` for SIGKILL fallback!):

```bash
# CORRECT — -k 10 sends SIGKILL 10s after SIGTERM if process ignores SIGTERM
timeout -k 10 600 cargo build --release --locked

# WRONG — process can ignore SIGTERM forever:
timeout 600 cargo build --release --locked
```

See [Agent Resource Limits Guide](./docs/agent-resource-limits.md) for details.

---

## 📋 Workstream Instructions

Development is organized into **parallel workstreams**. Each workstream has its own instruction file with specific tasks, key crates, and documentation:

| Workstream | File | Focus |
|------------|------|-------|
| **A: CPU/JIT** | [`instructions/cpu-jit.md`](./instructions/cpu-jit.md) | CPU emulation, decoder, JIT, memory |
| **B: Graphics** | [`instructions/graphics.md`](./instructions/graphics.md) | VGA, DirectX 9/10/11, WebGPU |
| **C: Windows Drivers** | [`instructions/windows-drivers.md`](./instructions/windows-drivers.md) | AeroGPU, virtio drivers |
| **D: Storage** | [`instructions/io-storage.md`](./instructions/io-storage.md) | AHCI, NVMe, OPFS, streaming |
| **E: Network** | [`instructions/network.md`](./instructions/network.md) | E1000, L2 proxy, TCP/UDP |
| **F: USB/Input** | [`instructions/usb-input.md`](./instructions/usb-input.md) | PS/2, USB HID, keyboard/mouse |
| **G: Audio** | [`instructions/audio.md`](./instructions/audio.md) | HD Audio, AudioWorklet |
| **H: Integration** | [`instructions/integration.md`](./instructions/integration.md) | BIOS, ACPI, PCI, boot |

**Start by reading your workstream's instruction file.** Each file contains:
- Specific task lists with priorities and dependencies
- Key crates and directories
- Essential documentation to read
- Interface contracts relevant to that workstream
- Coordination points with other workstreams

See [`instructions/README.md`](./instructions/README.md) for the full index.

---

## Executive Summary

This document coordinates the development of a high-performance Windows 7 emulator that runs entirely in the browser. Unlike existing projects (v86, JSLinux) that target older operating systems, Aero specifically targets Windows 7—a significantly more complex OS requiring:

- **x86-64 CPU emulation** with all modern extensions (SSE4.2, AVX where feasible)
- **2-4GB RAM emulation** (minimum viable for Windows 7)
- **DirectX 9/10/11 → WebGPU translation** for Aero glass and applications
- **ACPI/APIC/HPET** accurate timing and power management
- **AHCI/NVMe storage** with large disk image support (20GB+)
- **Virtio paravirtualized drivers** for performance-critical paths

This is not a "proof of concept" document—it is a comprehensive engineering blueprint for building production-quality emulation.

---

## Table of Contents

1. [Architecture Overview](./docs/01-architecture-overview.md)
2. [CPU Emulation Engine](./docs/02-cpu-emulation.md)
3. [Memory Management Unit](./docs/03-memory-management.md)
4. [Graphics Subsystem (DirectX → WebGPU)](./docs/04-graphics-subsystem.md)
5. [Storage Subsystem](./docs/05-storage-subsystem.md)
6. [Audio Subsystem](./docs/06-audio-subsystem.md)
7. [Networking Stack](./docs/07-networking.md)
8. [Input Device Emulation](./docs/08-input-devices.md)
9. [BIOS/UEFI & Firmware](./docs/09-bios-firmware.md)
10. [Performance Optimization Strategies](./docs/10-performance-optimization.md)
11. [Browser APIs & Web Platform Integration](./docs/11-browser-apis.md)
12. [Testing Strategy & Validation](./docs/12-testing-strategy.md)
13. [Legal & Licensing Considerations](./docs/13-legal-considerations.md)
14. [Project Milestones & Roadmap](./docs/14-project-milestones.md)
15. [Task Breakdown & Work Organization](./docs/15-agent-task-breakdown.md)
16. [Direct3D 10/11 Translation (SM4/SM5 → WebGPU)](./docs/16-d3d10-11-translation.md)
17. [D3D9Ex / DWM Compatibility](./docs/16-d3d9ex-dwm-compatibility.md)
18. [Windows 7 Guest Tools Install Guide](./docs/windows7-guest-tools.md)
19. [Windows 7 Driver Troubleshooting](./docs/windows7-driver-troubleshooting.md)
20. [Backend: Disk Image Streaming Service](./docs/backend/disk-image-streaming-service.md)
21. [Windows 7 Virtio Device Contract](./docs/windows7-virtio-driver-contract.md)
22. [Security Headers & Deployment Hardening](./docs/security-headers.md)
23. [Disk Image Streaming (HTTP Range + Auth + COOP/COEP)](./docs/16-disk-image-streaming-auth.md)
24. [HTTP Range + CDN Behavior](./docs/17-range-cdn-behavior.md)
25. [Windows 7 SP1 Unattended Install (Drivers + Scripting)](./docs/16-win7-unattended-install.md)
26. [Windows 7 Unattended Install Validation & Troubleshooting](./docs/17-win7-unattend-validation.md)
27. [Win7 Install Media Servicing (test-signed virtio drivers)](./docs/16-win7-image-servicing.md)
28. [Win7 BCD offline patching (testsigning / nointegritychecks)](./docs/win7-bcd-offline-patching.md)
29. [Windows Driver Development](./docs/windows/README.md)
30. [Windows 7 Install Media Preparation (Slipstreaming)](./docs/16-windows7-install-media-prep.md)
31. [Performance Tooling (Profiling, HUD, Traces, Benchmarks)](./docs/16-performance-tooling.md)
32. [Guest Tools Packaging](./docs/16-guest-tools-packaging.md)
33. [Virtio Windows Drivers](./docs/virtio-windows-drivers.md)
34. [VM Snapshots (Save-State / Restore-State)](./docs/16-snapshots.md)
35. [AeroGPU Legacy VGA/VBE Compatibility](./docs/16-aerogpu-vga-vesa-compat.md)
36. [Virtio PCI (Modern) Interrupts on Windows 7 (KMDF)](./docs/windows/virtio-pci-modern-interrupts.md)
37. [Virtio PCI: Legacy + Transitional Devices](./docs/16-virtio-pci-legacy-transitional.md)
38. [Driver Install Media (FAT Image)](./docs/16-driver-install-media.md)
39. [Windows 7 Driver Build and Signing](./docs/16-windows7-driver-build-and-signing.md)
40. [Guest CPU Instruction Throughput Benchmarks (PF-008)](./docs/16-guest-cpu-benchmark-suite.md)
41. [Driver Packaging, Catalogs, and WDK Redistributables](./docs/16-driver-packaging-and-signing.md)
42. [Architecture Decision Records (ADRs)](./docs/adr/README.md)
43. [Agent Resource Limits & Concurrency](./docs/agent-resource-limits.md)
44. [IndexedDB storage story (async vs sync)](./docs/19-indexeddb-storage-story.md)
45. [Storage trait consolidation (disk/backing-store traits)](./docs/20-storage-trait-consolidation.md)

---

## Why This Is Hard (And Why We Can Do It Anyway)

### The Challenge Matrix


| Challenge         | Windows 95/2000 (v86) | Windows 7 (Aero)      | Difficulty Multiplier |
| ----------------- | --------------------- | --------------------- | --------------------- |
| CPU Architecture  | i386/i486             | x86-64 + extensions   | 3-5x                  |
| RAM Requirements  | 32-256 MB             | 1-4 GB                | 10-20x                |
| Graphics API      | VGA/SVGA              | DirectX 9/10/11, Aero | 50-100x               |
| Storage Size      | 500MB - 2GB           | 15-40 GB              | 10-20x                |
| Boot Complexity   | Simple BIOS           | ACPI, APIC, HPET      | 5x                    |
| Driver Complexity | Simple                | WDDM, PnP, WDF        | 10x                   |


### Why It's Now Possible

1. **WebAssembly maturity**: WASM now supports SIMD, threads (SharedArrayBuffer), and tail calls
2. **WebGPU availability**: Hardware-accelerated GPU access with compute shaders
3. **Modern storage APIs**: OPFS (Origin Private File System) enables fast, large file access
4. **Improved JIT**: Browser engines have mature JIT compilers we can leverage
5. **Memory**: Modern browsers can allocate multi-GB WASM memories
  - Practically constrained by wasm32 (≤4GiB addressable) and browser/OS allocation caps (often lower).

---

## Core Architecture Decisions

### Decision 1: Hybrid Interpretation + JIT Compilation

We use a **tiered compilation strategy**:

```
┌─────────────────────────────────────────────────────────────────┐
│                      Execution Tiers                            │
├─────────────────────────────────────────────────────────────────┤
│  Tier 0: Interpreter (cold code, debugging)                     │
│     ↓ Hot path detection (execution counters)                   │
│  Tier 1: Baseline JIT (quick compile, moderate speed)           │
│     ↓ Profiling data collection                                 │
│  Tier 2: Optimizing JIT (slow compile, maximum speed)           │
│     ↓ Deoptimization when assumptions break                     │
│  [Loop back to Tier 0/1 as needed]                              │
└─────────────────────────────────────────────────────────────────┘
```

### Decision 2: WASM as the JIT Target

Instead of generating native code (impossible in browser), we generate WASM modules dynamically:

```
x86-64 Instructions → IR (Intermediate Representation) → WASM Bytecode → Browser JIT → Native
```

This gives us:

- Near-native performance through the browser's own JIT
- Portability across platforms
- Security through WASM sandboxing

### Decision 3: Parallel Architecture with Web Workers

```
┌─────────────────────────────────────────────────────────────────┐
│                     Main Thread                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │ UI/Canvas   │  │ Event Loop  │  │ Coordinator │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
└─────────────────────────────────────────────────────────────────┘
         │                │                  │
         │ SharedArrayBuffer / Atomics       │
         ▼                ▼                  ▼
┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ CPU Worker  │  │ GPU Worker  │  │ I/O Worker  │  │ JIT Worker  │
│ (emulation) │  │ (WebGPU)    │  │ (storage)   │  │ (compile)   │
└─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘
```

### Decision 4: Paravirtualization Where Possible

For performance-critical paths, we implement **virtio-style drivers**:

- **virtio-blk**: Block device (storage)
- **virtio-net**: Network interface
- **virtio-gpu**: GPU commands (alongside full emulation)
- **virtio-input**: Keyboard/mouse
- **virtio-snd**: Audio

This requires custom Windows 7 drivers but provides 10-100x performance improvement over full emulation.

---

## Technology Stack

### Core Technologies


| Component     | Technology                      | Rationale                                  |
| ------------- | ------------------------------- | ------------------------------------------ |
| CPU Emulation | Rust → WASM                     | Memory safety, performance, WASM target    |
| JIT Compiler  | Custom (Cranelift-inspired)     | Generate WASM from x86-64                  |
| Graphics      | WebGPU + WGSL shaders           | Hardware acceleration, DirectX translation |
| Audio         | Web Audio API + AudioWorklet    | Low-latency audio processing               |
| Storage       | OPFS (sync) + IndexedDB (async) | Large files (OPFS); host-side caches/tools (IndexedDB) |
| Networking    | WebSocket + WebRTC              | TCP/UDP emulation                          |
| Threading     | Web Workers + SharedArrayBuffer | True parallelism                           |
| UI            | Canvas 2D + OffscreenCanvas     | Rendering pipeline                         |

Note: IndexedDB is async-only in the browser and cannot back the synchronous Rust disk/controller
stack in the same Worker. For the repo-wide storage trait guidance + migration plan, see:

- [`docs/19-indexeddb-storage-story.md`](./docs/19-indexeddb-storage-story.md)
- [`docs/20-storage-trait-consolidation.md`](./docs/20-storage-trait-consolidation.md)


### Build & Toolchain


| Tool         | Purpose                      |
| ------------ | ---------------------------- |
| Rust         | Core emulator implementation |
| wasm-pack    | Rust → WASM compilation      |
| wasm-bindgen | JS ↔ WASM interop            |
| TypeScript   | Host integration, UI         |
| Vite         | Build system, dev server     |
| wasm-opt     | WASM optimization            |


---

## Work Organization (Suggested)

The architecture is modular with well-defined interfaces, enabling parallel development across different areas. One possible way to organize work:

### Suggested Work Areas

```
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Work Areas                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  CORE           │  CPU emulation, memory, interrupts         │
│  │                 │  See: 02-cpu-emulation.md, 03-memory.md    │
│  └─────────────────┘                                            │
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  GRAPHICS       │  DirectX translation, WebGPU, shaders      │
│  │                 │  See: 04-graphics-subsystem.md             │
│  └─────────────────┘                                            │
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  I/O            │  Storage, network, audio, input            │
│  │                 │  See: 05-08 docs                           │
│  └─────────────────┘                                            │
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  FIRMWARE       │  BIOS, ACPI, device models                 │
│  │                 │  See: 09-bios-firmware.md                  │
│  └─────────────────┘                                            │
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  PERFORMANCE    │  Profiling, optimization, benchmarks       │
│  │                 │  See: 10-performance-optimization.md       │
│  └─────────────────┘                                            │
│                                                                  │
│  ┌─────────────────┐                                            │
│  │  INFRASTRUCTURE │  Build, test, CI/CD, browser compat        │
│  │                 │  See: 11-browser-apis.md, 12-testing.md    │
│  └─────────────────┘                                            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Interface Contracts

Each component produces and consumes well-defined interfaces:

```rust
// Example: CPU → memory/IO interface (canonical in `aero_cpu_core`)
//
// See: `crates/aero-cpu-core/src/mem.rs` (`aero_cpu_core::mem::CpuBus`)
//
// Notes:
// - Addresses are *linear* (paging translation is handled by the bus, e.g. `aero_cpu_core::PagingBus`).
// - Operations return `Result` so the CPU can raise architectural faults (e.g. `#PF`, `#GP(0)`).
// - This snippet is intentionally abridged; the real trait also includes:
//   - scalar reads/writes (`read_u16`, `write_u32`, ...)
//   - bulk byte ops (`read_bytes`, `write_bytes`, `bulk_copy`, `bulk_set`)
//   - atomic RMW (`atomic_rmw`, with write-intent semantics)
//   - `preflight_write_bytes` for fault-atomic multi-byte writes (no partial commits)
pub trait CpuBus {
    fn sync(&mut self, state: &aero_cpu_core::state::CpuState) {}
    fn invlpg(&mut self, vaddr: u64) {}

    fn read_u8(&mut self, vaddr: u64) -> Result<u8, aero_cpu_core::Exception>;
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), aero_cpu_core::Exception>;

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], aero_cpu_core::Exception>;
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, aero_cpu_core::Exception>;
    fn io_write(
        &mut self,
        port: u16,
        size: u32,
        val: u64,
    ) -> Result<(), aero_cpu_core::Exception>;
}

// Example: CPU → Graphics interface  
pub trait DisplayAdapter {
    fn write_vga_register(&mut self, port: u16, val: u8);
    fn read_vga_register(&self, port: u16) -> u8;
    fn get_framebuffer(&self) -> &[u8];
    fn submit_command_buffer(&mut self, cmds: &[GpuCommand]);
}
```

---

## Critical Path Analysis

### Phase 1: Bootable System (Months 1-6)

**Goal:** Boot Windows 7 to desktop

1. CPU emulation (protected mode, long mode, basic instructions)
2. Memory management (paging, TLB)
3. Legacy BIOS emulation
4. VGA/SVGA display
5. PS/2 keyboard/mouse
6. IDE/AHCI storage controller
7. Basic interrupt handling (PIC, APIC)

### Phase 2: Usable System (Months 7-12)

**Goal:** Run basic applications, Aero interface

1. Complete x86-64 instruction coverage
2. DirectX 9 → WebGPU translation
3. HD Audio emulation
4. Network adapter emulation
5. USB controller basics
6. Performance optimization pass

### Phase 3: Production System (Months 13-18)

**Goal:** Run complex applications smoothly

1. DirectX 10/11 support
2. Virtio paravirtualized drivers
3. Multi-core CPU emulation
4. Advanced optimization (JIT tuning)
5. Full USB support
6. Comprehensive testing

---

## Success Metrics


| Metric             | Target       | Measurement                |
| ------------------ | ------------ | -------------------------- |
| Boot time          | < 60 seconds | Time from start to desktop |
| Frame rate         | ≥ 30 FPS     | During Aero desktop usage  |
| Application compat | ≥ 80%        | Top 100 Windows 7 apps     |
| Memory overhead    | < 1.5x       | Emulator RAM vs guest RAM  |
| Storage I/O        | ≥ 50 MB/s    | Sequential read/write      |


---

## Getting Started

1. **Read this entire document** — it contains critical operational guidance
2. Read [`LEGAL.md`](./LEGAL.md) and [`CONTRIBUTING.md`](./CONTRIBUTING.md) (clean-room rules, licensing, distribution constraints)
3. **Run environment setup:**
   ```bash
   bash ./scripts/agent-env-setup.sh     # One-time validation
   source ./scripts/agent-env.sh         # Activate env vars
   ```
4. **Read your workstream's instruction file** from [`instructions/`](./instructions/README.md)
5. Read [Architecture Overview](./docs/01-architecture-overview.md) for system design
6. Understand the [Interface Contracts](#interface-contracts) below
7. Check [Project Milestones](./docs/14-project-milestones.md) for timeline
8. **Build and test:**
   ```bash
   bash ./scripts/safe-run.sh cargo build --locked
   bash ./scripts/safe-run.sh cargo test --locked
   ```
9. Begin implementation following test-driven development

---

## Document Index


| Document                                                                                              | Description                                                                                                             | Primary Relevance    |
| ----------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- | -------------------- |
| [01-architecture-overview.md](./docs/01-architecture-overview.md)                                     | System architecture, component diagram                                                                                  | All                  |
| [02-cpu-emulation.md](./docs/02-cpu-emulation.md)                                                     | x86-64 CPU emulation design                                                                                             | Core                 |
| [03-memory-management.md](./docs/03-memory-management.md)                                             | Virtual memory, paging, TLB                                                                                             | Core                 |
| [04-graphics-subsystem.md](./docs/04-graphics-subsystem.md)                                           | DirectX → WebGPU translation                                                                                            | Graphics             |
| [05-storage-subsystem.md](./docs/05-storage-subsystem.md)                                             | Disk emulation, AHCI, virtio                                                                                            | I/O                  |
| [19-indexeddb-storage-story.md](./docs/19-indexeddb-storage-story.md)                                 | IndexedDB async-only constraints and integration options (Option A vs Option C)                                         | I/O / Infra          |
| [20-storage-trait-consolidation.md](./docs/20-storage-trait-consolidation.md)                         | Canonical disk/backend traits + consolidation plan (Rust + TypeScript)                                                  | I/O / Infra          |
| [06-audio-subsystem.md](./docs/06-audio-subsystem.md)                                                 | HD Audio, Web Audio API                                                                                                 | I/O                  |
| [07-networking.md](./docs/07-networking.md)                                                           | Network stack emulation                                                                                                 | I/O                  |
| [08-input-devices.md](./docs/08-input-devices.md)                                                     | Keyboard, mouse, USB HID                                                                                                | I/O                  |
| [usb-hid.md](./docs/usb-hid.md)                                                                       | USB HID usages and report formats                                                                                       | I/O                  |
| [webhid-webusb-passthrough.md](./docs/webhid-webusb-passthrough.md)                                   | WebHID/WebUSB physical device passthrough architecture + security model (aero-usb/aero-wasm bridges + TS broker wiring) | I/O / Infra          |
| [webhid-hid-report-descriptor-synthesis.md](./docs/webhid-hid-report-descriptor-synthesis.md)         | WebHID → HID report descriptor synthesis (Windows 7 contract)                                                           | I/O                  |
| [webusb.md](./docs/webusb.md)                                                                         | WebUSB constraints and troubleshooting                                                                                  | I/O / Infra          |
| [webusb-passthrough.md](./docs/webusb-passthrough.md)                                                 | WebUSB passthrough architecture (UHCI ↔ host actions/completions)                                                       | I/O / Infra          |
| [09-bios-firmware.md](./docs/09-bios-firmware.md)                                                     | BIOS, ACPI, device models                                                                                               | Firmware             |
| [10-performance-optimization.md](./docs/10-performance-optimization.md)                               | JIT, caching, profiling                                                                                                 | Performance          |
| [11-browser-apis.md](./docs/11-browser-apis.md)                                                       | Web platform integration                                                                                                | Infrastructure       |
| [12-testing-strategy.md](./docs/12-testing-strategy.md)                                               | Testing methodology                                                                                                     | All                  |
| [13-legal-considerations.md](./docs/13-legal-considerations.md)                                       | Licensing, IP concerns                                                                                                  | All                  |
| [14-project-milestones.md](./docs/14-project-milestones.md)                                           | Timeline, deliverables                                                                                                  | All                  |
| [15-agent-task-breakdown.md](./docs/15-agent-task-breakdown.md)                                       | Parallelizable work items                                                                                               | All                  |
| [adr/README.md](./docs/adr/README.md)                                                                 | Architecture Decision Records (ADRs)                                                                                    | Infrastructure       |
| [16-debugging-and-introspection.md](./docs/16-debugging-and-introspection.md)                         | Developer debugging surfaces (serial, state, trace)                                                                     | All                  |
| [16-snapshots.md](./docs/16-snapshots.md)                                                             | VM snapshot format + restore flow                                                                                       | All                  |
| [16-performance-tooling.md](./docs/16-performance-tooling.md)                                         | Profiling HUD, traces, benchmarks                                                                                       | Performance          |
| [16-windows7-driver-build-and-signing.md](./docs/16-windows7-driver-build-and-signing.md)             | Driver build & signing toolchain notes                                                                                  | Infrastructure       |
| [16-driver-packaging-and-signing.md](./docs/16-driver-packaging-and-signing.md)                       | Driver packaging, catalogs, WDK redist                                                                                  | Infrastructure       |
| [16-d3d9ex-dwm-compatibility.md](./docs/16-d3d9ex-dwm-compatibility.md)                               | D3D9Ex surface + DWM requirements                                                                                       | Graphics             |
| [16-d3d10-11-translation.md](./docs/16-d3d10-11-translation.md)                                       | Direct3D 10/11 translation details                                                                                      | Graphics             |
| [16-perf-tracing.md](./docs/16-perf-tracing.md)                                                       | Chrome Trace/Perfetto export                                                                                            | Performance          |
| [16-remote-disk-image-delivery.md](./docs/16-remote-disk-image-delivery.md)                           | Production delivery of disk bytes via object store + CDN + HTTP Range                                                   | I/O / Infra          |
| [17-range-cdn-behavior.md](./docs/17-range-cdn-behavior.md)                                           | HTTP Range + CDN caching/limits                                                                                         | Infrastructure       |
| [deployment/cloudfront-disk-streaming.md](./docs/deployment/cloudfront-disk-streaming.md)             | Concrete CloudFront/S3 setup for authenticated disk Range streaming                                                     | I/O / Infra          |
| [17-disk-image-lifecycle-and-access-control.md](./docs/17-disk-image-lifecycle-and-access-control.md) | Hosted disk image uploads, ownership/sharing, leases, and writeback strategies                                          | I/O / Infra          |
| [18-chunked-disk-image-format.md](./docs/18-chunked-disk-image-format.md)                             | Chunked disk image delivery (no HTTP Range)                                                                             | I/O                  |
| [windows7-guest-tools.md](./docs/windows7-guest-tools.md)                                             | End-user guide: install Guest Tools and switch to virtio + Aero GPU                                                     | All                  |
| [windows7-driver-troubleshooting.md](./docs/windows7-driver-troubleshooting.md)                       | End-user guide: Windows 7 driver/signing troubleshooting                                                                | All                  |
| [16-driver-install-media.md](./docs/16-driver-install-media.md)                                       | Driver install media artifacts                                                                                          | Infrastructure       |
| [16-win7-image-servicing.md](./docs/16-win7-image-servicing.md)                                       | Win7 install media servicing for test-signed drivers                                                                    | I/O                  |
| [win7-bcd-offline-patching.md](./docs/win7-bcd-offline-patching.md)                                   | Win7 offline patching of BCD stores for testsigning/nointegritychecks (element IDs + object selection)                  | I/O                  |
| [windows/README.md](./docs/windows/README.md)                                                         | Windows driver development notes                                                                                        | I/O                  |
| [windows7-virtio-driver-contract.md](./docs/windows7-virtio-driver-contract.md)                       | Virtio contract: Win7 drivers ↔ emulator                                                                                | I/O                  |
| [virtio-windows-drivers.md](./docs/virtio-windows-drivers.md)                                         | Windows 7 virtio driver packaging                                                                                       | I/O                  |
| [virtqueue-split-ring-win7.md](./docs/virtio/virtqueue-split-ring-win7.md)                            | Virtio 1.0 split virtqueue implementation guide (Win7 KMDF)                                                             | I/O (virtio drivers) |
| [windows/virtio-pci-modern-interrupts.md](./docs/windows/virtio-pci-modern-interrupts.md)             | Windows 7 KMDF guide for virtio-pci modern MSI-X/INTx interrupts                                                        | I/O (virtio drivers) |
| [backend/disk-image-streaming-service.md](./docs/backend/disk-image-streaming-service.md)             | Disk image streaming (Range/CORS/COEP)                                                                                  | I/O / Infra          |
| [security-headers.md](./docs/security-headers.md)                                                     | COOP/COEP, CSP, security headers                                                                                        | Infrastructure       |
| [16-disk-image-streaming-auth.md](./docs/16-disk-image-streaming-auth.md)                             | Disk byte streaming, auth, CORS/COEP                                                                                    | I/O / Infra          |
| [16-win7-unattended-install.md](./docs/16-win7-unattended-install.md)                                 | Unattended Win7 install (drivers/hooks)                                                                                 | All                  |
| [17-win7-unattend-validation.md](./docs/17-win7-unattend-validation.md)                               | Validate/debug Win7 unattended installs (logs, config-set, SetupComplete)                                               | All                  |
| [16-windows7-install-media-prep.md](./docs/16-windows7-install-media-prep.md)                         | Preparing a Win7 SP1 ISO with Aero drivers/certs                                                                        | All                  |
| [16-guest-tools-packaging.md](./docs/16-guest-tools-packaging.md)                                     | Guest Tools ISO/zip packaging                                                                                           | Infrastructure       |
| [16-guest-cpu-benchmark-suite.md](./docs/16-guest-cpu-benchmark-suite.md)                             | Guest CPU throughput benchmarks (PF-008)                                                                                | Performance          |
| [agent-resource-limits.md](./docs/agent-resource-limits.md)                                           | Resource limits for concurrent agent execution                                                                          | Infrastructure       |


---

## Quick Reference: Key Technical Decisions


| Decision                | Choice                                                          | Rationale                                                                         |
| ----------------------- | --------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| Implementation Language | Rust                                                            | Memory safety, WASM target, performance                                           |
| JIT Strategy            | Tiered (interpreter → baseline → optimizing)                    | Balance startup time vs peak perf                                                 |
| WASM Threading          | SharedArrayBuffer + Atomics                                     | True parallelism required                                                         |
| Graphics API            | WebGPU (fallback: WebGL2)                                       | Hardware acceleration essential                                                   |
| Storage Backend         | OPFS primary; IndexedDB fallback (async-only)                   | Large file support (OPFS); host-side caches/tools (IndexedDB)                     |
| Network Transport       | L2 tunnel over WebSocket (default), optional WebRTC DataChannel | Keep the browser as a frame forwarder; proxy runs the user-space stack (ADR 0013) |
| Audio Processing        | AudioWorklet                                                    | Low latency audio                                                                 |
| USB passthrough stack   | `crates/aero-usb` + `web/src/usb/*` (ADR 0015)                  | Single canonical browser USB stack; deterministic device models in Rust           |
| Browser memory model    | Split buffers (shared `WebAssembly.Memory` + small SABs)        | Avoid >4GiB offsets and single huge SAB allocations                               |


---

## Coordination Notes

- **Architecture questions:** Review docs first
- **Interface changes:** May require cross-functional review
- **Performance concerns:** Relevant to performance work area
- **Browser compatibility:** Relevant to infrastructure work area

---

*This is a living document.*
