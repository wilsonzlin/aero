/// <reference lib="webworker" />

import { RingBuffer } from "../runtime/ring_buffer";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag } from "../runtime/shared_layout";
import { MessageType, type ProtocolMessage, type WorkerInitMessage, decodeProtocolMessage } from "../runtime/protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let role: "cpu" | "gpu" | "io" | "jit" = "gpu";
let status!: Int32Array;
let commandRing!: RingBuffer;

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  const init = ev.data as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;

  role = init.role ?? "gpu";
  const segments = { control: init.controlSab!, guestMemory: init.guestMemory!, vgaFramebuffer: init.vgaFramebuffer! };
  status = createSharedMemoryViews(segments).status;
  const regions = ringRegionsForWorker(role);
  commandRing = new RingBuffer(segments.control, regions.command.byteOffset, regions.command.byteLength);

  setReadyFlag(status, role, true);
  ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);

  void runLoop();
};

async function runLoop(): Promise<void> {
  while (true) {
    while (true) {
      const bytes = commandRing.pop();
      if (!bytes) break;
      const cmd = decodeProtocolMessage(bytes);
      if (!cmd) continue;
      if (cmd.type === MessageType.STOP) {
        Atomics.store(status, StatusIndex.StopRequested, 1);
      }
    }

    if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;
    await commandRing.waitForData();
  }

  setReadyFlag(status, role, false);
  ctx.close();
}
