# 01 - Architecture Overview

## System Architecture Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              BROWSER ENVIRONMENT                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         MAIN THREAD                                     │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                  │ │
│  │  │  UI Manager  │  │ Event Router │  │  Coordinator │                  │ │
│  │  │  (Canvas)    │  │  (kbd/mouse) │  │  (IPC hub)   │                  │ │
│  │  └──────────────┘  └──────────────┘  └──────────────┘                  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                              │                                               │
│        Shared WebAssembly.Memory + SharedArrayBuffer + Atomics               │
│                              │                                               │
│  ┌───────────────────────────┼───────────────────────────────────────────┐  │
│  │                    WORKER THREADS                                      │  │
│  │                           │                                            │  │
│  │  ┌────────────────────────┴─────────────────────────────────────────┐ │  │
│  │  │                    CPU EMULATION WORKER                           │ │  │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │ │  │
│  │  │  │ Interpreter │  │  JIT Engine │  │ Instruction │              │ │  │
│  │  │  │   (Tier 0)  │  │ (Tier 1/2)  │  │   Decoder   │              │ │  │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘              │ │  │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │ │  │
│  │  │  │  Registers  │  │    MMU      │  │  Interrupt  │              │ │  │
│  │  │  │   (x86-64)  │  │  (paging)   │  │  Controller │              │ │  │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘              │ │  │
│  │  └──────────────────────────────────────────────────────────────────┘ │  │
│  │                                                                        │  │
│  │  ┌────────────────────────────────────────────────────────────────┐   │  │
│  │  │                    GPU EMULATION WORKER                         │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │   │  │
│  │  │  │   DirectX   │  │    WGSL     │  │   WebGPU    │             │   │  │
│  │  │  │ Translator  │  │   Shaders   │  │  Commands   │             │   │  │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘             │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐                              │   │  │
│  │  │  │ Framebuffer │  │   Texture   │                              │   │  │
│  │  │  │   Manager   │  │    Cache    │                              │   │  │
│  │  │  └─────────────┘  └─────────────┘                              │   │  │
│  │  └────────────────────────────────────────────────────────────────┘   │  │
│  │                                                                        │  │
│  │  ┌────────────────────────────────────────────────────────────────┐   │  │
│  │  │                    I/O WORKER                                   │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │   │  │
│  │  │  │   Storage   │  │   Network   │  │    Audio    │             │   │  │
│  │  │  │  (AHCI/NVMe)│  │  (E1000)    │  │  (HD Audio) │             │   │  │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘             │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐                              │   │  │
│  │  │  │    USB      │  │   Serial    │                              │   │  │
│  │  │  │ Controller  │  │    Ports    │                              │   │  │
│  │  │  └─────────────┘  └─────────────┘                              │   │  │
│  │  └────────────────────────────────────────────────────────────────┘   │  │
│  │                                                                        │  │
│  │  ┌────────────────────────────────────────────────────────────────┐   │  │
│  │  │                    JIT COMPILATION WORKER                       │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │   │  │
│  │  │  │    x86-64   │  │     IR      │  │    WASM     │             │   │  │
│  │  │  │   Parser    │  │  Optimizer  │  │  Generator  │             │   │  │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘             │   │  │
│  │  │  ┌─────────────┐  ┌─────────────┐                              │   │  │
│  │  │  │    Code     │  │   Profile   │                              │   │  │
│  │  │  │    Cache    │  │    Data     │                              │   │  │
│  │  │  └─────────────┘  └─────────────┘                              │   │  │
│  │  └────────────────────────────────────────────────────────────────┘   │  │
│  └────────────────────────────────────────────────────────────────────────┘  │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         SHARED MEMORY REGION                            │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐   │ │
│  │  │            Guest Physical Memory (512MiB-3GiB, configurable)     │   │ │
│  │  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐   │   │ │
│  │  │  │ 0-640KB │ │ Video   │ │ ROM     │ │ Extended│ │ High    │   │   │ │
│  │  │  │ Conv.   │ │ Memory  │ │ Area    │ │ Memory  │ │ Memory  │   │   │ │
│  │  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘   │   │ │
│  │  └─────────────────────────────────────────────────────────────────┘   │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐   │ │
│  │  │              Device MMIO Regions                                 │   │ │
│  │  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐               │   │ │
│  │  │  │ VGA FB  │ │ PCI CFG │ │ APIC    │ │ HPET    │               │   │ │
│  │  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘               │   │ │
│  │  └─────────────────────────────────────────────────────────────────┘   │ │
│  │  ┌─────────────────────────────────────────────────────────────────┐   │ │
│  │  │              Inter-Worker Communication Buffers                  │   │ │
│  │  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐               │   │ │
│  │  │  │ Command │ │ Event   │ │ Status  │ │ Debug   │               │   │ │
│  │  │  │ Queues  │ │ Queues  │ │ Flags   │ │ Buffers │               │   │ │
│  │  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘               │   │ │
│  │  └─────────────────────────────────────────────────────────────────┘   │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         BROWSER APIS                                    │ │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐          │ │
│  │  │ WebGPU  │ │ OPFS    │ │WebSocket│ │ Web     │ │ Pointer │          │ │
│  │  │         │ │         │ │         │ │ Audio   │ │ Lock    │          │ │
│  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘          │ │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐          │ │
│  │  │IndexedDB│ │ WebRTC  │ │WebCodecs│ │Offscreen│ │ Gamepad │          │ │
│  │  │         │ │         │ │         │ │ Canvas  │ │   API   │          │ │
│  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘          │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Component Responsibilities

### Main Thread Components

| Component | Responsibility | Key Interfaces |
|-----------|----------------|----------------|
| **UI Manager** | Canvas rendering, fullscreen, resize | `CanvasRenderingContext2D`, `requestAnimationFrame` |
| **Event Router** | Keyboard/mouse capture, event translation | `KeyboardEvent`, `MouseEvent`, `PointerLock` |
| **Coordinator** | Worker lifecycle, IPC routing, state sync | `Worker`, `WebAssembly.Memory`, `SharedArrayBuffer`, `Atomics` |

### CPU Emulation Worker

| Component | Responsibility | Key Interfaces |
|-----------|----------------|----------------|
| **Interpreter (Tier-0)** | Slow but accurate instruction execution | `aero_cpu_core::interp::tier0` (`exec::step`) |
| **JIT Engine (Tier-1/2)** | Fast compiled code execution | `aero_cpu_core::exec::ExecDispatcher` + `aero_cpu_core::jit` |
| **Instruction Decoder** | x86-64 instruction parsing | `aero_x86::decode` (iced-x86 wrapper) |
| **Registers / architectural state** | CPU register file + control state | `aero_cpu_core::state::CpuState` (JIT ABI) |
| **MMU / paging** | Linear → physical address translation | `aero_cpu_core::PagingBus` (wraps `aero_mmu`) |
| **Interrupts / exceptions** | Architectural delivery + bookkeeping | `aero_cpu_core::{CpuCore, interrupts::PendingEventState}` |

#### Multi-vCPU execution

> **Current status:** `aero_machine::Machine` supports `cpu_count > 1` and includes basic SMP bring-up
> plumbing (per-vCPU LAPIC state/MMIO, INIT/SIPI, and bounded cooperative AP execution inside
> `Machine::run_slice`). This is sufficient for SMP contract/bring-up tests, but it is **not** a full
> SMP scheduler or parallel vCPU execution environment yet. Other integrations (e.g. `PcMachine`)
> still execute only the BSP. For the up-to-date bring-up plan and gap list see
> [`docs/21-smp.md`](./21-smp.md) (and [`instructions/integration.md`](../instructions/integration.md)).

To support SMP guests, the CPU emulation worker hosts **2+ vCPUs**:

- Each vCPU has its own `aero_cpu_core::CpuCore` (owns `CpuState`, `interrupts::PendingEventState`, and `time::TimeSource`).
- Guest physical memory and device models are shared.
- Scheduling is either:
  - **Parallel vCPU workers** (one Web Worker per vCPU), or
  - A **deterministic time-sliced scheduler** inside a single CPU worker (baseline).

### GPU Emulation Worker

| Component | Responsibility | Key Interfaces |
|-----------|----------------|----------------|
| **DirectX Translator** | D3D9/10/11 → WebGPU command translation | `TranslateCommand(d3d_cmd)` |
| **WGSL Shaders** | Shader compilation and caching | `CompileShader(hlsl)` |
| **WebGPU Commands** | GPU command buffer building | `SubmitCommands(cmds)` |
| **Framebuffer Manager** | Display output, VGA compatibility | `Present()`, `GetPixels()` |
| **Texture Cache** | GPU texture management | `LoadTexture(id, data)` |

### I/O Worker

| Component | Responsibility | Key Interfaces |
|-----------|----------------|----------------|
| **Storage** | AHCI/NVMe/virtio-blk emulation | `Read(lba, sectors)`, `Write(lba, data)` |
| **Network** | E1000/virtio-net emulation | `Send(packet)`, `Receive() -> packet` |
| **Audio** | Guest audio device models (HDA/virtio-snd) + Web Audio bridging (AC'97 is legacy-only) | PCI/MMIO + virtio queues, `SharedArrayBuffer` rings ↔ `AudioWorklet` |
| **USB** | USB host controller emulation (UHCI/EHCI/xHCI) + USB device models (optional WebHID/WebUSB passthrough) | `crates/aero-usb` (`uhci::UhciController`, `ehci::EhciController`, `xhci::XhciController`), `UsbHostAction` bridge (see [ADR 0015](./adr/0015-canonical-usb-stack.md); controller docs: [`usb-ehci.md`](./usb-ehci.md), [`usb-xhci.md`](./usb-xhci.md)) |

> Runtime note: the table above describes the **legacy** browser runtime architecture (`vmRuntime=legacy`), where guest-visible
> device models live in the I/O worker.
>
> In the canonical machine runtime (`vmRuntime=machine`), the CPU worker runs `api.Machine` (backed by `aero_machine::Machine`)
> and owns guest devices (storage/network/input/etc). In that mode, the I/O worker runs in a **host-only stub** configuration and
> does not initialize guest device models.

### JIT Compilation Worker

| Component | Responsibility | Key Interfaces |
|-----------|----------------|----------------|
| **x86-64 Parser** | Instruction stream analysis | `ParseBlock(address)` |
| **IR Optimizer** | Intermediate representation optimization | `Optimize(ir_block)` |
| **WASM Generator** | WASM bytecode generation | `GenerateWasm(ir_block)` |
| **Code Cache** | Compiled code storage | `Lookup(address)`, `Store(address, code)` |
| **Profile Data** | Execution hotspot tracking | `RecordExecution(address, count)` |

---

## Memory Layout

> **Important:** WebAssembly (MVP) linear memory is limited to **4 GiB**. That limit applies to the *entire* WASM address space (guest RAM + any in-WASM control/state). Avoid designs that assume a monolithic “5 GiB+” shared buffer; instead, **split guest memory from control/IPC buffers** using multiple `SharedArrayBuffer` instances.  
> See [ADR 0003](./adr/0003-shared-memory-layout.md).

### Guest Physical Memory Map

```
┌─────────────────────────────────────────────────────────────────┐
│ Address Range        │ Size     │ Description                   │
├─────────────────────────────────────────────────────────────────┤
│ 0x0000_0000 - 0x0009_EFFF │ 636 KiB  │ Conventional memory (RAM) │
│ 0x0009_F000 - 0x0009_FFFF │ 4 KiB    │ EBDA (reserved)           │
│ 0x000A_0000 - 0x000F_FFFF │ 384 KiB  │ VGA/BIOS/option ROM (res) │
│ 0x0010_0000 - 0xAFFF_FFFF │ ~2.75 GiB│ Low RAM (usable; ≤ ECAM)  │
│ 0xB000_0000 - 0xBFFF_FFFF │ 256 MiB  │ PCIe ECAM (MMCONFIG/MCFG) │
│ 0xC000_0000 - 0xFFFF_FFFF │ 1 GiB    │ PCI/MMIO hole (reserved)  │
│   0xFEC0_0000 - 0xFEC0_0FFF │ 4 KiB  │ I/O APIC MMIO (within hole)│
│   0xFED0_0000 - 0xFED0_03FF │ 1 KiB  │ HPET MMIO (within hole)   │
│   0xFEE0_0000 - 0xFEE0_0FFF │ 4 KiB  │ Local APIC (within hole)  │
│   0xFFFF_0000 - 0xFFFF_FFFF │ 64 KiB │ BIOS reset-vector alias   │
│ 0x1_0000_0000 - ...        │ ...     │ High RAM remap (>4 GiB)   │
└─────────────────────────────────────────────────────────────────┘
```

Note: Aero’s baseline browser build uses wasm32 and is therefore constrained to **< 4 GiB** of
contiguous guest RAM. With an ECAM window at `0xB000_0000`, the largest contiguous low-RAM window is
**< 2.75 GiB**; larger RAM configurations require remapping some RAM above 4 GiB, which in turn
requires a segmented/sparse host backing model (or wasm `memory64`).

### I/O Port Map (Selected)

```
┌─────────────────────────────────────────────────────────────────┐
│ Port Range     │ Device                                         │
├─────────────────────────────────────────────────────────────────┤
│ 0x0000-0x001F  │ DMA Controller 1                               │
│ 0x0020-0x0021  │ PIC 1 (Master)                                 │
│ 0x0040-0x0043  │ PIT (Programmable Interval Timer)              │
│ 0x0060, 0x0064 │ PS/2 Controller (Keyboard/Mouse)               │
│ 0x0070-0x0071  │ CMOS / RTC                                     │
│ 0x0080-0x008F  │ DMA Page Registers                             │
 │ 0x00A0-0x00A1  │ PIC 2 (Slave)                                  │
 │ 0x00C0-0x00DF  │ DMA Controller 2                               │
 │ 0x0170-0x0177  │ IDE Secondary                                  │
 │ 0x01CE-0x01CF  │ VBE (Bochs VBE_DISPI) index/data               │
 │ 0x01F0-0x01F7  │ IDE Primary                                    │
 │ 0x0278-0x027A  │ Parallel Port (LPT)                            │
 │ 0x02F8-0x02FF  │ COM2 Serial                                    │
│ 0x03B0-0x03DF  │ VGA registers (legacy display)                  │
 │ 0x03F0-0x03F7  │ Floppy Controller                              │
 │ 0x03F8-0x03FF  │ COM1 Serial                                    │
 │ 0x0CF8-0x0CFF  │ PCI Configuration                              │
 │ 0x0CF9         │ Reset Control (ACPI FADT reset register)        │
 │ 0x0400-0x0427  │ ACPI PM I/O (PM1a_EVT/CNT, PM_TMR, GPE0, SCI)   │
 └─────────────────────────────────────────────────────────────────┘
```

Note on boot display vs AeroGPU:

- With `MachineConfig::enable_vga=true`, the canonical `aero_machine::Machine` implements these
  VGA/VBE legacy ports using the standalone `aero_gpu_vga` device model (boot display).
  - When `enable_pc_platform=false`, the VBE LFB MMIO aperture is mapped directly at the configured
    LFB base.
  - When `enable_pc_platform=true`, the machine exposes a minimal Bochs/QEMU-compatible “Standard VGA”
    PCI function (currently `00:0c.0`, `1234:1111`) so the VBE LFB is reachable via PCI BAR0 inside the PCI MMIO
    window / BAR router. BAR0 is assigned by BIOS POST / the PCI allocator (and may be relocated when
    other PCI devices are present); the machine mirrors the assigned BAR base into the BIOS VBE
    `PhysBasePtr` and the VGA device model so guests observe a coherent LFB base.
- With `MachineConfig::enable_aerogpu=true`, the canonical machine exposes the AeroGPU PCI identity
  (`A3A0:0001`) at `00:07.0` with the canonical BAR layout (BAR0 regs + BAR1 VRAM aperture) for
  stable Windows driver binding. In `aero_machine` today BAR1 is backed by a dedicated VRAM buffer
  and implements permissive legacy VGA decode (VGA port I/O + VRAM-backed `0xA0000..0xBFFFF`
  window; see `docs/16-aerogpu-vga-vesa-compat.md`). Note: the in-tree Win7 AeroGPU driver treats
  the adapter as system-memory-backed (no dedicated WDDM VRAM segment); BAR1 exists for VGA/VBE
  compatibility and is outside the WDDM memory model. BAR0 implements a minimal MMIO surface
  (ring/fence transport + scanout0/vblank register storage/pacing and host-facing scanout
  presentation). Default bring-up behavior can complete fences without executing the command
  stream.

  Command execution is pluggable:
  - browser/WASM runtimes can enable an out-of-process “submission bridge”
    (`Machine::aerogpu_drain_submissions` + `Machine::aerogpu_complete_fence`) so the GPU worker can
    execute submissions and report fence completion, and
  - native builds can optionally install an in-process headless wgpu backend (feature-gated;
    `Machine::aerogpu_set_backend_wgpu`).

  Shared device-side building blocks (regs/ring/executor + reusable PCI wrapper) live in
  `crates/aero-devices-gpu`. A legacy sandbox integration surface remains in `crates/emulator`
  (see also: [`21-emulator-crate-migration.md`](./21-emulator-crate-migration.md)).
- Long-term direction: the AeroGPU WDDM device (`PCI\\VEN_A3A0&DEV_0001`) should be the sole display
  adapter and also own VGA/VBE compatibility. This is already implemented in `aero_machine` when
  `MachineConfig::enable_aerogpu=true`; see:
  - [`abi/aerogpu-pci-identity.md`](./abi/aerogpu-pci-identity.md)
  - [`16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md)

---

## Data Flow Diagrams

### Instruction Execution Flow

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│   Fetch     │───▶│   Decode    │───▶│   Execute   │───▶│  Writeback  │
│  (memory)   │    │  (parser)   │    │ (ALU/JIT)   │    │ (registers) │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
       │                  │                  │                  │
       ▼                  ▼                  ▼                  ▼
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  MMU/TLB    │    │ Instruction │    │   Memory    │    │   Flags     │
│ Translation │    │   Cache     │    │   Access    │    │   Update    │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
```

### Graphics Pipeline Flow

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Guest App  │───▶│   DirectX   │───▶│   Command   │───▶│   WebGPU    │
│  (D3D call) │    │ Translator  │    │   Buffer    │    │   Submit    │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
                                                               │
                          ┌────────────────────────────────────┘
                          ▼
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  GPU Exec   │───▶│ Framebuffer │───▶│  Composite  │───▶│   Canvas    │
│  (shaders)  │    │   Output    │    │  (layers)   │    │  (display)  │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
```

### Storage I/O Flow

```
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Guest OS   │───▶│    AHCI     │───▶│   Request   │───▶│    OPFS     │
│  (driver)   │    │ Controller  │    │   Queue     │    │   Access    │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
       │                  │                  │                  │
       │                  │                  │                  │
       ▼                  ▼                  ▼                  ▼
┌─────────────┐    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐
│  Interrupt  │◀───│   Status    │◀───│  Complete   │◀───│   File      │
│   (IRQ)     │    │   Update    │    │   Notify    │    │   Read      │
└─────────────┘    └─────────────┘    └─────────────┘    └─────────────┘
```

> Note: in the Rust codebase, the canonical synchronous disk traits are
> `aero_storage::{StorageBackend, VirtualDisk}`. Browser IndexedDB is async-only and cannot back the
> synchronous Rust disk/controller path in the same Worker; see
> [`19-indexeddb-storage-story.md`](./19-indexeddb-storage-story.md) and
> [`20-storage-trait-consolidation.md`](./20-storage-trait-consolidation.md).

---

## Inter-Worker Communication

### Message Types

The production IPC layer uses a **binary protocol** carried over **lock-free ring
buffers** in a `SharedArrayBuffer` (rather than `postMessage` JSON).

The protocol and shared-memory layout are defined in:

- [`docs/ipc-protocol.md`](./ipc-protocol.md)

In code:

- Rust: `crates/aero-ipc/`
- TypeScript: `web/src/ipc/`
 
### Snapshot Protocol (Save/Restore)
 
Snapshots enable fast resume, crash recovery, and deterministic testing.
 
**Coordinator-level flow:**

1. Coordinator broadcasts `STATE_SAVE` to workers.
2. I/O worker forces storage flush (AHCI/NVMe/virtio-blk) and snapshots device + host-side metadata.
3. Workers respond with a versioned, deterministic byte blob per subsystem.
4. Coordinator stores the combined snapshot (IndexedDB / OPFS / download).
5. On resume, Coordinator broadcasts `STATE_RESTORE` with the saved bytes.
6. Workers restore device state; I/O worker reopens host handles (OPFS/IDB/WebSocket/WebRTC) as needed.

**Deterministic serialization requirements:**

- Use an explicit, versioned format (e.g. TLV).
- Avoid nondeterministic iteration order (no `HashMap` without sorting).
- Treat active host-side resources as *reconnectable* rather than bit-restorable (see networking limitations).
 
Power-management requests (guest S5 shutdown and reset) should be surfaced as
explicit protocol events so the coordinator can stop/reset workers, flush
storage, and update UI state.
 
### Shared Memory Protocol

Browsers cannot reliably allocate a single 5+GiB `SharedArrayBuffer`, and wasm32 linear memory is fundamentally **≤ 4GiB** addressable. To keep the architecture implementable today, Aero uses **Option B: split buffers** (see [ADR 0003](./adr/0003-shared-memory-layout.md)).

| Buffer | Type | Typical Size | Contents | Access Pattern |
|--------|------|--------------|----------|----------------|
| `guestMemory` | `WebAssembly.Memory` (`shared: true`) | **512MiB / 1GiB / 2GiB / 3GiB** (best-effort) | Guest physical RAM | Shared read/write; the only region directly accessible from wasm today |
| `controlSab` | `SharedArrayBuffer` | ~KiB–MiB | Small status block + per-worker command/event rings (start/stop/config) | Atomics-driven ring buffer; `Atomics.wait` in workers, `Atomics.waitAsync`/polling on main |
| `ioIpcSab` | `SharedArrayBuffer` | ~KiB–MiB | High-frequency device IPC rings (CPU/WASM ↔ IO worker, network TX/RX, etc) | AIPC header + Atomics-driven ring buffers (see [`docs/ipc-protocol.md`](./ipc-protocol.md)) |
| `vram` | `SharedArrayBuffer` | **64MiB** (default) | AeroGPU BAR1/VRAM aperture backing (MMIO window; legacy VGA/VBE and optional WDDM surfaces) | I/O worker maps it as an MMIO range; GPU worker reads it for scanout/cursor when `base_paddr` points into the VRAM aperture (see [`docs/16-aerogpu-vga-vesa-compat.md`](./16-aerogpu-vga-vesa-compat.md#vram-bar1-backing-as-a-sharedarraybuffer)) |
| `sharedFramebuffer` | `SharedArrayBuffer` | ~MiB | CPU→GPU demo/legacy RGBA framebuffer | CPU worker writes frames; GPU worker presents |
| `scanoutState` | `SharedArrayBuffer` | tiny | Shared scanout descriptor (`LEGACY_TEXT`/`LEGACY_VBE_LFB`/`WDDM`, `base_paddr`, width/height/pitch/format) | Lock-free seqlock-style publish/snapshot between device models and the presenter |
| `cursorState` | `SharedArrayBuffer` | tiny | Shared hardware cursor descriptor (cursor regs + surface pointer) | Device models publish updates; GPU worker reads/presents cursor overlay |

This avoids >4GiB offsets entirely: each buffer is independently addressable and can be sized/failed independently.

#### Ring buffer layout (IPC queues)

The IPC queues use a bounded, variable-length ring buffer that supports **SPSC**
(per-worker cmd/evt) and can be extended to **MPSC** (e.g. a global log queue).

The full definition is in [`docs/ipc-protocol.md`](./ipc-protocol.md). Summary:

**Ring header (`Int32Array[4]`, 16 bytes):**

| Index | Name | Description |
|---:|---|---|
| 0 | `head` | Consumer cursor (wrapping `u32` byte offset) |
| 1 | `tail_reserve` | Producer reservation cursor (wrapping `u32` byte offset) |
| 2 | `tail_commit` | Producer commit cursor (wrapping `u32` byte offset) |
| 3 | `capacity` | Data region size in bytes (written once at init) |

**Data region:** `Uint8Array(capacity)`

**Record format (variable length):**

- `u32 payload_len`
- `payload_len` bytes of payload
- padding to 4-byte alignment

If there is insufficient contiguous space near the end of the buffer, producers
write a **wrap marker** (`payload_len = 0xFFFF_FFFF`) and advance to the next
segment start.

### Synchronization Primitives
Queues are implemented as a **bounded variable-length MPSC/SPSC ring buffer**
with:

- `head`: consumer cursor
- `tail_reserve`: producer reservation cursor
- `tail_commit`: producer commit cursor (published/visible tail)

This makes the queue safe for **multi-producer single-consumer** use (global log
queue), while still being efficient for **SPSC** use (per-worker cmd/evt).

---

## Initialization Sequence

```
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Boot Sequence                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  1. Page Load                                                    │
│     └─▶ Load main.js bundle                                      │
│         └─▶ Initialize Coordinator                               │
│                                                                  │
│  2. Resource Allocation                                          │
│     └─▶ Allocate shared `WebAssembly.Memory` (guest RAM, ≤ 4 GiB, configurable) │
│     └─▶ Allocate `controlSab` (status + per-worker cmd/evt rings) + `ioIpcSab` (high-frequency AIPC) │
│     └─▶ Allocate display-related shared buffers (`vram` BAR1 backing, `scanoutState`, `cursorState`, legacy `sharedFramebuffer`) │
│         └─▶ Request storage access (OPFS)                        │
│             └─▶ Load/create disk image                           │
│                                                                  │
│  3. Worker Initialization                                        │
│     └─▶ Spawn CPU Worker                                         │
│         └─▶ Initialize CPU state (real mode)                     │
│     └─▶ Spawn GPU Worker                                         │
│         └─▶ Initialize WebGPU device                             │
│     └─▶ Spawn I/O Worker                                         │
│         └─▶ Initialize device models                             │
│     └─▶ Spawn JIT Worker                                         │
│         └─▶ Initialize code cache                                │
│                                                                  │
│  4. BIOS Load                                                    │
│     └─▶ Copy BIOS to 0xF0000-0xFFFFF                            │
│         └─▶ Initialize interrupt vectors                         │
│             └─▶ Set up ACPI tables                               │
│                                                                  │
│  5. Reset Vector Execution                                       │
│     └─▶ Set CS:IP to 0xF000:0xFFF0                              │
│         └─▶ Begin CPU emulation loop                             │
│                                                                  │
│  6. BIOS POST                                                    │
│     └─▶ Memory detection                                         │
│         └─▶ Device enumeration                                   │
│             └─▶ Boot device selection                            │
│                                                                  │
│  7. OS Boot                                                      │
│     └─▶ Load boot sector                                         │
│         └─▶ Windows Boot Manager                                 │
│             └─▶ Windows 7 kernel load                            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Error Handling Strategy

### Error Categories

| Category | Examples | Handling |
|----------|----------|----------|
| **Guest Faults** | Page fault, GPF, divide by zero | Inject exception to guest |
| **Emulation Errors** | Unimplemented instruction, invalid state | Log, attempt recovery, or halt |
| **Resource Errors** | OOM, storage full | Graceful degradation or user notification |
| **Browser Errors** | API unavailable, permission denied | Feature detection, fallback |

### Recovery Mechanisms

1. **State Snapshots**: Periodic saves for crash recovery
2. **Watchdog Timer**: Detect hung CPU loops
3. **Memory Limits**: Prevent runaway allocation
4. **Error Boundaries**: Isolate component failures

---

## Security Model

### Threat Model

| Threat | Mitigation |
|--------|------------|
| Guest escape | WASM sandbox, no direct system access |
| Code injection | JIT output validated, CSP headers |
| Data exfiltration | Same-origin policy, no network by default |
| DoS (resource) | CPU/memory limits, throttling |
| Malicious images | Signature verification, sandbox isolation |

### Security Boundaries

```
┌─────────────────────────────────────────────────────────────────┐
│                    Security Boundaries                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │              BROWSER SANDBOX (enforced by browser)       │    │
│  │  ┌───────────────────────────────────────────────────┐  │    │
│  │  │           WASM SANDBOX (linear memory only)        │  │    │
│  │  │  ┌─────────────────────────────────────────────┐  │  │    │
│  │  │  │        GUEST OS (emulated environment)      │  │  │    │
│  │  │  │                                             │  │  │    │
│  │  │  │  - No access to host filesystem             │  │  │    │
│  │  │  │  - No access to host network (directly)     │  │  │    │
│  │  │  │  - No access to other browser tabs          │  │  │    │
│  │  │  │  - Memory isolated in SharedArrayBuffer     │  │  │    │
│  │  │  │                                             │  │  │    │
│  │  │  └─────────────────────────────────────────────┘  │  │    │
│  │  └───────────────────────────────────────────────────┘  │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| IPS (Instructions/sec) | ≥ 500 MIPS | Instruction counter |
| Boot time | < 60s | Time to desktop |
| Frame rate | ≥ 30 FPS | `requestAnimationFrame` timing |
| Input latency | < 50ms | Event timestamp delta |
| Storage throughput | ≥ 50 MB/s | Timed read/write |
| Memory overhead | < 1.5x | Actual vs guest RAM |
| JIT compile time | < 10ms/block | Compilation timing |

---

## Next Steps

1. **Read [VM crate map](./vm-crate-map.md)** for the canonical Rust VM core crate graph (browser + host) and layering rules.
1. **Read [CPU Emulation](./02-cpu-emulation.md)** for x86-64 implementation details
2. **Read [Memory Management](./03-memory-management.md)** for MMU/paging design
3. **Read [Graphics Subsystem](./04-graphics-subsystem.md)** for DirectX translation
4. **Review [Task Breakdown](./15-agent-task-breakdown.md)** for detailed work items
