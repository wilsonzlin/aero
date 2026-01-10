/// <reference lib="webworker" />

import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage, decodeProtocolMessage } from "../runtime/protocol";
import { InputEventType } from "../input/event_queue";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: "cpu" | "gpu" | "io" | "jit" = "io";
let status!: Int32Array;
let commandRing!: RingBuffer;

let started = false;
let pollTimer: number | undefined;

type InputBatchMessage = { type: "in:input-batch"; buffer: ArrayBuffer };

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const data = ev.data as Partial<WorkerInitMessage> | Partial<InputBatchMessage> | undefined;
  if (!data) return;

  // First message is the shared-memory init handshake.
  if ((data as Partial<WorkerInitMessage>).kind === "init") {
    const init = data as Partial<WorkerInitMessage>;
    role = init.role ?? "io";
    const segments = { control: init.controlSab!, guestMemory: init.guestMemory!, vgaFramebuffer: init.vgaFramebuffer! };
    status = createSharedMemoryViews(segments).status;
    const regions = ringRegionsForWorker(role);
    commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);

    setReadyFlag(status, role, true);
    ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);

    startPolling();
    return;
  }

  // Input is delivered via structured `postMessage` to avoid SharedArrayBuffer contention on the
  // main thread and to keep the hot path in JS simple.
  if ((data as Partial<InputBatchMessage>).type === "in:input-batch") {
    const msg = data as Partial<InputBatchMessage>;
    if (!(msg.buffer instanceof ArrayBuffer)) return;
    if (!started) return;
    handleInputBatch(msg.buffer);
    return;
  }
};

function startPolling(): void {
  if (started) return;
  started = true;

  // This worker must remain responsive to `postMessage` input batches. Avoid blocking loops / Atomics.wait
  // here; instead poll the command ring at a low rate.
  pollTimer = setInterval(() => {
    drainCommands();
    if (Atomics.load(status, StatusIndex.StopRequested) === 1) {
      shutdown();
    }
  }, 8) as unknown as number;
}

function drainCommands(): void {
  while (true) {
    const bytes = commandRing.pop();
    if (!bytes) break;
    const cmd = decodeProtocolMessage(bytes);
    if (!cmd) continue;
    if (cmd.type === MessageType.STOP) {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    }
  }
}

function handleInputBatch(buffer: ArrayBuffer): void {
  // `buffer` is transferred from the main thread, so it is uniquely owned here.
  const words = new Int32Array(buffer);
  const count = words[0] >>> 0;

  Atomics.add(status, StatusIndex.IoInputBatchCounter, 1);
  Atomics.add(status, StatusIndex.IoInputEventCounter, count);

  // The actual i8042 device model is implemented in Rust; once the WASM-side I/O devices are
  // integrated, this loop should feed those devices directly.
  // For now, we only track counters so tests can assert that the worker is receiving input.
  const base = 2;
  for (let i = 0; i < count; i++) {
    const off = base + i * 4;
    const type = words[off] >>> 0;
    if (type === InputEventType.KeyScancode) {
      // Key payload is packed bytes + len. No-op for now.
      continue;
    }
  }
}

function shutdown(): void {
  if (pollTimer !== undefined) {
    clearInterval(pollTimer);
    pollTimer = undefined;
  }

  setReadyFlag(status, role, false);
  ctx.close();
}
