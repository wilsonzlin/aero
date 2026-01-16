import { parentPort, workerData } from "node:worker_threads";
import { runIoWorkerServer } from "./io_worker_runtime.ts";

// Node-only test harness: allow parent tests to request a clean shutdown.
// This avoids relying exclusively on `Worker.terminate()` semantics across Node versions.
parentPort?.on("message", (msg) => {
  if (!msg || typeof msg !== "object") return;
  if (!("type" in msg)) return;
  if ((msg as { type?: unknown }).type !== "shutdown") return;
  process.exit(0);
});

runIoWorkerServer(workerData);

// If the server loop returns (e.g. Node test stop flag), close the port so the worker can exit.
parentPort?.close();
process.exit(0);

