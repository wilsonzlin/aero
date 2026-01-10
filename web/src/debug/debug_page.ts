import { SharedRingBuffer } from "../io/ipc/ring_buffer.ts";
import { IO_MESSAGE_STRIDE_U32 } from "../io/ipc/io_protocol.ts";

type DebugUi = { onEvent: (event: unknown) => void };

function getDebugUi(): DebugUi | null {
  return (globalThis as unknown as { aeroDebug?: DebugUi }).aeroDebug ?? null;
}

function emitToUi(event: unknown): void {
  const ui = getDebugUi();
  if (ui) ui.onEvent(event);
}

function emitText(text: string): void {
  const bytes = Array.from(new TextEncoder().encode(text));
  emitToUi({ type: "SerialOutput", port: 0x3f8, data: bytes });
}

function startSerialDemo(): void {
  if (typeof SharedArrayBuffer === "undefined") {
    emitText(
      "SharedArrayBuffer unavailable; enable COOP/COEP (crossOriginIsolated) to use the debug worker demo.\n",
    );
    return;
  }

  const req = SharedRingBuffer.create({ capacity: 1024, stride: IO_MESSAGE_STRIDE_U32 });
  const resp = SharedRingBuffer.create({ capacity: 1024, stride: IO_MESSAGE_STRIDE_U32 });

  const ioWorker = new Worker(new URL("../workers/io_worker.ts", import.meta.url), { type: "module" });
  ioWorker.postMessage({
    type: "init",
    requestRing: req.sab,
    responseRing: resp.sab,
    devices: ["uart16550"],
    tickIntervalMs: 1,
  });

  const cpuWorker = new Worker(new URL("../workers/serial_demo_cpu_worker.ts", import.meta.url), { type: "module" });
  cpuWorker.onmessage = (ev) => emitToUi(ev.data);
  cpuWorker.postMessage({ type: "init", requestRing: req.sab, responseRing: resp.sab });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", startSerialDemo, { once: true });
} else {
  startSerialDemo();
}

