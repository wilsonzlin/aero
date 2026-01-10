/// <reference lib="webworker" />

import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import {
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
  decodeProtocolMessage,
  encodeProtocolMessage,
} from "../runtime/protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: "cpu" | "gpu" | "io" | "jit" = "cpu";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing!: RingBuffer;
let guestI32!: Int32Array;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const init = ev.data as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;

  role = init.role ?? "cpu";
  const segments = { control: init.controlSab!, guestMemory: init.guestMemory! };
  const views = createSharedMemoryViews(segments);
  status = views.status;
  guestI32 = views.guestI32;

  const regions = ringRegionsForWorker(role);
  commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);
  eventRing = new RingBuffer(segments.control, regions.event.byteOffset, regions.event.byteLength);

  setReadyFlag(status, role, true);
  ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);

  runLoop();
};

function runLoop(): void {
  let running = false;

  while (true) {
    // Drain commands.
    while (true) {
      const bytes = commandRing.pop();
      if (!bytes) break;
      const cmd = decodeProtocolMessage(bytes);
      if (!cmd) continue;

      if (cmd.type === MessageType.START) {
        running = true;
      } else if (cmd.type === MessageType.STOP) {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;

    if (running) {
      const counter = Atomics.add(status, StatusIndex.HeartbeatCounter, 1) + 1;
      Atomics.add(guestI32, 0, 1);
      // Best-effort: heartbeat events are allowed to drop if the ring is full.
      eventRing.push(encodeProtocolMessage({ type: MessageType.HEARTBEAT, role, counter }));
    }

    // Sleep until either new commands arrive or the next heartbeat tick.
    commandRing.waitForData(running ? 250 : undefined);
  }

  setReadyFlag(status, role, false);
  ctx.close();
}
