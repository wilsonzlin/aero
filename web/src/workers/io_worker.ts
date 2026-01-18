import { runIoWorkerServer } from "./io_worker_runtime.ts";
import { parseIoWorkerInitMessage } from "./worker_init_parsers.ts";

type InitMessage = { type: "init" } & import("./io_worker_runtime.ts").IoWorkerInitOptions;

globalThis.onmessage = (ev: MessageEvent<InitMessage>) => {
  const init = parseIoWorkerInitMessage(ev.data);
  if (!init) return;
  runIoWorkerServer(init);
};

