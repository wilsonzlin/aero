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
| **Interpreter** | Slow but accurate instruction execution | `ExecuteInstruction(opcode)` |
| **JIT Engine** | Fast compiled code execution | `ExecuteBlock(address)` |
| **Instruction Decoder** | x86-64 instruction parsing | `Decode(bytes) -> Instruction` |
| **Registers** | CPU register file (RAX-R15, etc.) | `GetReg(id)`, `SetReg(id, val)` |
| **MMU** | Virtual → Physical address translation | `Translate(vaddr) -> paddr` |
| **Interrupt Controller** | IRQ handling, APIC emulation | `RaiseIRQ(num)`, `CheckPending()` |

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
| **Audio** | HD Audio/AC97 emulation | `PlaySamples(buf)`, `GetMixerState()` |
| **USB** | UHCI/EHCI/xHCI emulation | `AttachDevice(dev)`, `ProcessURB(urb)` |

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

### Guest Physical Memory Map

```
┌─────────────────────────────────────────────────────────────────┐
│ Address Range        │ Size     │ Description                   │
├─────────────────────────────────────────────────────────────────┤
│ 0x0000_0000 - 0x0009_FFFF │ 640 KB   │ Conventional Memory       │
│ 0x000A_0000 - 0x000B_FFFF │ 128 KB   │ VGA Video Memory          │
│ 0x000C_0000 - 0x000C_7FFF │ 32 KB    │ Video BIOS                │
│ 0x000C_8000 - 0x000E_FFFF │ 160 KB   │ Adapter ROM Area          │
│ 0x000F_0000 - 0x000F_FFFF │ 64 KB    │ System BIOS               │
│ 0x0010_0000 - 0x00EF_FFFF │ 14 MB    │ Extended Memory (ISA hole)│
│ 0x00F0_0000 - 0x00FF_FFFF │ 1 MB     │ ISA Memory Hole           │
│ 0x0100_0000 - 0xBFFF_FFFF │ ~3 GB    │ Extended Memory (config-dependent)│
│ 0xC000_0000 - 0xFEBF_FFFF │ ~1 GB    │ PCI MMIO Space            │
│ 0xFEC0_0000 - 0xFEC0_0FFF │ 4 KB     │ I/O APIC                  │
│ 0xFED0_0000 - 0xFED0_03FF │ 1 KB     │ HPET                      │
│ 0xFEE0_0000 - 0xFEE0_0FFF │ 4 KB     │ Local APIC               │
│ 0xFFFF_0000 - 0xFFFF_FFFF │ 64 KB    │ BIOS Shadow / Reset Vec   │
└─────────────────────────────────────────────────────────────────┘
```

Note: Aero’s baseline browser build uses wasm32 and is therefore constrained to **< 4GiB** of contiguous guest RAM. Supporting guest RAM above 4GiB would require either wasm `memory64` (not assumed) or a segmented/sparse host backing model.

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
│ 0x01F0-0x01F7  │ IDE Primary                                    │
│ 0x0278-0x027A  │ Parallel Port (LPT)                            │
│ 0x02F8-0x02FF  │ COM2 Serial                                    │
│ 0x03C0-0x03DF  │ VGA Registers                                  │
│ 0x03F0-0x03F7  │ Floppy Controller                              │
│ 0x03F8-0x03FF  │ COM1 Serial                                    │
│ 0x0CF8-0x0CFF  │ PCI Configuration                              │
└─────────────────────────────────────────────────────────────────┘
```

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

### Shared Memory Protocol

Browsers cannot reliably allocate a single 5+GiB `SharedArrayBuffer`, and wasm32 linear memory is fundamentally **≤ 4GiB** addressable. To keep the architecture implementable today, Aero uses **Option B: split buffers**.

| Buffer | Type | Typical Size | Contents | Access Pattern |
|--------|------|--------------|----------|----------------|
| `guestMemory` | `WebAssembly.Memory` (`shared: true`) | **512MiB / 1GiB / 2GiB / 3GiB** (best-effort) | Guest physical RAM | Shared read/write; the only region directly accessible from wasm today |
| `ipcSab` | `SharedArrayBuffer` | ~MiB (configurable) | IPC control block + ring buffers (cmd/evt per worker) | Atomics-driven ring buffer; `Atomics.wait` in workers, `Atomics.waitAsync`/polling on main |

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
│     └─▶ Allocate shared `WebAssembly.Memory` (guest RAM, configurable) │
│     └─▶ Allocate `SharedArrayBuffer` IPC region(s) (cmd/evt queues, state) │
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

1. **Read [CPU Emulation](./02-cpu-emulation.md)** for x86-64 implementation details
2. **Read [Memory Management](./03-memory-management.md)** for MMU/paging design
3. **Read [Graphics Subsystem](./04-graphics-subsystem.md)** for DirectX translation
4. **Review [Task Breakdown](./15-agent-task-breakdown.md)** for detailed work items
