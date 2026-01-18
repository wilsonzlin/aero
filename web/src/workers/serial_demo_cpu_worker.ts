/// <reference lib="webworker" />

import { IoClient } from "../io/ipc/io_client.ts";
import { SharedRingBuffer } from "../io/ipc/ring_buffer.ts";
import { parseSerialDemoCpuWorkerInitMessage } from "./worker_init_parsers.ts";

type InitMessage = {
  type: "init";
  requestRing: SharedArrayBuffer;
  responseRing: SharedArrayBuffer;
  text?: string;
};

function encodeBytes(text: string): number[] {
  const encoder = new TextEncoder();
  return Array.from(encoder.encode(text));
}

globalThis.onmessage = (ev: MessageEvent<InitMessage>) => {
  const init = parseSerialDemoCpuWorkerInitMessage(ev.data);
  if (!init) return;

  const req = SharedRingBuffer.from(init.requestRing);
  const resp = SharedRingBuffer.from(init.responseRing);

  const io = new IoClient(req, resp, {
    onSerialOutput: (port, data) => {
      // Mirror the debug UI event shape.
      (globalThis as unknown as DedicatedWorkerGlobalScope).postMessage({
        type: "SerialOutput",
        port,
        data: Array.from(data),
      });
    },
  });

  const bytes = encodeBytes(init.text);
  for (const b of bytes) {
    io.portWrite(0x3f8, 1, b);
  }

  (globalThis as unknown as DedicatedWorkerGlobalScope).postMessage({ type: "done" });
};
