/// <reference lib="webworker" />

import { IoClient } from "../io/ipc/io_client.ts";
import { SharedRingBuffer } from "../io/ipc/ring_buffer.ts";
import { unrefBestEffort } from "../unrefSafe";

type DebugCommand =
  | { type: "Pause" }
  | { type: "Resume" }
  | { type: "Step" }
  | { type: "SetBreakpoint"; rip: number }
  | { type: "RemoveBreakpoint"; rip: number }
  | { type: "ClearBreakpoints" }
  | { type: "ReadMemory"; paddr: number; len: number }
  | { type: "RequestCpuState" }
  | { type: "RequestDeviceState" }
  | { type: "EnableTrace"; filter?: { include_instructions?: boolean; include_port_io?: boolean; sample_rate?: number } }
  | { type: "DisableTrace" };

type InitMessage = {
  type: "init";
  requestRing: SharedArrayBuffer;
  responseRing: SharedArrayBuffer;
};

type WorkerMessage = InitMessage | DebugCommand;

type CpuState = {
  rip: number;
  rflags: number;
  rax: number;
  rbx: number;
  rcx: number;
  rdx: number;
  rsi: number;
  rdi: number;
  rbp: number;
  rsp: number;
};

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    unrefBestEffort(timer);
  });
}

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let io: IoClient | null = null;

let paused = false;
let stepsRemaining = 0;
let wake: (() => void) | null = null;

let rip = 0x1000;
let rflags = 0x2;
let rax = 0;
let rbx = 0;
let rcx = 0;
let rdx = 0;
let rsi = 0;
let rdi = 0;
let rbp = 0;
let rsp = 0x8000;

const breakpoints = new Set<number>();

const memory = new Uint8Array(256 * 1024);
for (let i = 0; i < memory.length; i++) memory[i] = i & 0xff;

let traceEnabled = true;
let traceIncludeInstructions = false;
let traceIncludePortIo = true;
let traceSampleRate = 1;
let traceCounter = 0;
let traceFlushAt = nowMs() + 250;
const traceBuf: unknown[] = [];

function postEvent(event: unknown): void {
  ctx.postMessage(event);
}

function snapshotCpuState(): CpuState {
  return { rip, rflags, rax, rbx, rcx, rdx, rsi, rdi, rbp, rsp };
}

function emitCpuState(): void {
  postEvent({ type: "CpuState", state: snapshotCpuState() });
}

function emitDeviceState(): void {
  postEvent({
    type: "DeviceState",
    state: {
      devices: ["uart16550(com1)", "i8042"],
      uart: { basePort: 0x3f8 },
    },
  });
}

function traceRecord(event: unknown): void {
  if (!traceEnabled) return;

  if (typeof event === "object" && event !== null) {
    const kind = (event as { type?: unknown }).type;
    if (kind === "Instruction" && !traceIncludeInstructions) return;
    if ((kind === "PortRead" || kind === "PortWrite") && !traceIncludePortIo) return;
  }

  traceCounter = (traceCounter + 1) >>> 0;
  const rate = Math.max(1, traceSampleRate | 0);
  if (rate !== 1 && traceCounter % rate !== 0) {
    return;
  }
  traceBuf.push(event);
}

function traceFlushIfNeeded(force = false): void {
  const now = nowMs();
  if (!force && traceBuf.length < 64 && now < traceFlushAt) {
    return;
  }
  traceFlushAt = now + 250;
  if (traceBuf.length === 0) return;

  const events = traceBuf.splice(0, traceBuf.length);
  postEvent({ type: "TraceChunk", events });
}

function writeCom1(byte: number): void {
  if (!io) return;
  io.portWrite(0x3f8, 1, byte & 0xff);
  traceRecord({ type: "PortWrite", port: 0x3f8, size: 1, value: byte & 0xff });
}

let serialScript = new TextEncoder().encode("Aero debug CPU: running. Set a breakpoint to pause.\r\n");
let serialScriptPos = 0;

function execOneInstruction(): void {
  // Fake instruction execution: mutate some registers and memory, advance RIP.
  const oldRip = rip;

  traceRecord({ type: "Instruction", rip: oldRip, bytes: [] });

  rax = (rax + 1) >>> 0;
  rcx = (rcx + 3) >>> 0;
  rdx = (rdx ^ 0x55aa55aa) >>> 0;
  memory[oldRip & (memory.length - 1)] = rax & 0xff;

  // Periodically emit a byte to COM1 to demonstrate the serial console.
  if (serialScriptPos < serialScript.length && (oldRip & 0xf) === 0) {
    writeCom1(serialScript[serialScriptPos++]!);
  }

  rip = (oldRip + 1) >>> 0;
}

async function waitForWake(): Promise<void> {
  if (!paused) return;
  await new Promise<void>((resolve) => {
    wake = resolve;
  });
}

function wakeLoop(): void {
  if (!wake) return;
  const w = wake;
  wake = null;
  w();
}

async function runLoop(): Promise<void> {
  let lastStateAt = 0;

  // eslint-disable-next-line no-constant-condition
  while (true) {
    if (paused) {
      traceFlushIfNeeded(true);
      emitCpuState();
      await waitForWake();
      continue;
    }

    if (breakpoints.has(rip >>> 0)) {
      paused = true;
      postEvent({ type: "BreakpointHit", rip: rip >>> 0 });
      continue;
    }

    // Execute a small batch to keep the worker responsive.
    for (let i = 0; i < 5_000 && !paused; i++) {
      if (breakpoints.has(rip >>> 0)) {
        paused = true;
        postEvent({ type: "BreakpointHit", rip: rip >>> 0 });
        break;
      }

      execOneInstruction();

      if (stepsRemaining > 0) {
        stepsRemaining -= 1;
        if (stepsRemaining === 0) {
          paused = true;
          postEvent({ type: "Paused", reason: { type: "SingleStep" } });
          break;
        }
      }
    }

    const now = nowMs();
    if (now - lastStateAt > 250) {
      lastStateAt = now;
      emitCpuState();
    }
    traceFlushIfNeeded();

    await sleep(0);
  }
}

function handleCommand(cmd: DebugCommand): void {
  switch (cmd.type) {
    case "Pause":
      paused = true;
      postEvent({ type: "Paused", reason: { type: "Manual" } });
      break;
    case "Resume":
      paused = false;
      wakeLoop();
      break;
    case "Step":
      stepsRemaining += 1;
      paused = false;
      wakeLoop();
      break;
    case "SetBreakpoint":
      breakpoints.add(cmd.rip >>> 0);
      break;
    case "RemoveBreakpoint":
      breakpoints.delete(cmd.rip >>> 0);
      break;
    case "ClearBreakpoints":
      breakpoints.clear();
      break;
    case "ReadMemory": {
      const start = cmd.paddr >>> 0;
      const len = Math.max(0, cmd.len | 0);
      const out: number[] = [];
      for (let i = 0; i < len; i++) {
        out.push(memory[(start + i) & (memory.length - 1)]!);
      }
      postEvent({ type: "MemoryData", paddr: start, data: out });
      break;
    }
    case "RequestCpuState":
      emitCpuState();
      break;
    case "RequestDeviceState":
      emitDeviceState();
      break;
    case "EnableTrace":
      traceEnabled = true;
      traceIncludeInstructions = cmd.filter?.include_instructions ?? traceIncludeInstructions;
      traceIncludePortIo = cmd.filter?.include_port_io ?? traceIncludePortIo;
      traceSampleRate = cmd.filter?.sample_rate ?? traceSampleRate;
      break;
    case "DisableTrace":
      traceEnabled = false;
      traceBuf.length = 0;
      break;
    default:
      break;
  }
}

ctx.onmessage = (ev: MessageEvent<WorkerMessage>) => {
  const msg = ev.data;
  if (!msg || typeof msg !== "object") return;

  if (msg.type === "init") {
    const req = SharedRingBuffer.from(msg.requestRing);
    const resp = SharedRingBuffer.from(msg.responseRing);
    io = new IoClient(req, resp, {
      onSerialOutput: (port, data) => {
        traceRecord({ type: "SerialOutput", port, data: Array.from(data) });
        postEvent({ type: "SerialOutput", port, data: Array.from(data) });
      },
    });

    paused = false;
    emitDeviceState();
    emitCpuState();
    void runLoop();
    return;
  }

  handleCommand(msg as DebugCommand);
};
