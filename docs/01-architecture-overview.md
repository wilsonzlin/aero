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
│                    SharedArrayBuffer + Atomics                               │
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
│  │  │              Guest Physical Memory (1-4 GB)                      │   │ │
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
| **Coordinator** | Worker lifecycle, IPC routing, state sync | `Worker`, `SharedArrayBuffer`, `Atomics` |

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

```typescript
// Worker message protocol
interface WorkerMessage {
  type: MessageType;
  id: number;        // For request/response correlation
  payload: unknown;
}

enum MessageType {
  // CPU → Coordinator
  CPU_HALTED = 'cpu_halted',
  CPU_TRIPLE_FAULT = 'cpu_triple_fault',
  MMIO_READ = 'mmio_read',
  MMIO_WRITE = 'mmio_write',
  PORT_READ = 'port_read',
  PORT_WRITE = 'port_write',
  
  // GPU → Coordinator
  FRAME_READY = 'frame_ready',
  GPU_COMMAND_COMPLETE = 'gpu_cmd_complete',
  
  // I/O → Coordinator
  IRQ_RAISE = 'irq_raise',
  IRQ_LOWER = 'irq_lower',
  DMA_REQUEST = 'dma_request',
  
  // Coordinator → Workers
  START = 'start',
  STOP = 'stop',
  RESET = 'reset',
  STATE_SAVE = 'state_save',
  STATE_RESTORE = 'state_restore',
}
```

### Shared Memory Protocol

Browsers cannot reliably allocate a single 5+GiB `SharedArrayBuffer`, and wasm32 linear memory is fundamentally **≤ 4GiB** addressable. To keep the architecture implementable today, Aero uses **Option B: split buffers**.

| Buffer | Type | Typical Size | Contents | Access Pattern |
|--------|------|--------------|----------|----------------|
| `guestMemory` | `WebAssembly.Memory` (`shared: true`) | **512MiB / 1GiB / 2GiB / 3GiB** (best-effort) | Guest physical RAM | Shared read/write; the only region directly accessible from wasm today |
| `stateSab` | `SharedArrayBuffer` | 64KiB | Small global state (run/pause/stop flags, liveness, stats) | Mostly atomic loads/stores |
| `cmdSab` | `SharedArrayBuffer` | 64KiB+ | Command ring buffers (SPSC per producer/consumer pair) | Atomic head/tail + `Atomics.wait/notify` |
| `eventSab` | `SharedArrayBuffer` | 64KiB+ | Event ring buffers (responses, interrupts, input events) | Atomic head/tail + `Atomics.wait/notify` |

This avoids >4GiB offsets entirely: each buffer is independently addressable and can be sized/failed independently.

### Synchronization Primitives

```typescript
// Lock-free ring buffer for command passing
class RingBuffer {
  private buffer: SharedArrayBuffer;
  private head: Int32Array;  // Write position (atomic)
  private tail: Int32Array;  // Read position (atomic)
  
  push(data: Uint8Array): boolean {
    // Atomic compare-and-swap for thread-safe push
    // Returns false if buffer full
  }
  
  pop(): Uint8Array | null {
    // Atomic load for thread-safe pop
    // Returns null if buffer empty
  }
  
  waitForData(): void {
    // Atomics.wait() for efficient blocking
  }
  
  notifyData(): void {
    // Atomics.notify() to wake waiters
  }
}
```

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
│     └─▶ Allocate small `SharedArrayBuffer`s (state/cmd/event)     │
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
