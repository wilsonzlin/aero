import { runIoWorkerServer } from "./io_worker_runtime.ts";

type InitMessage = { type: "init" } & import("./io_worker_runtime.ts").IoWorkerInitOptions;

globalThis.onmessage = (ev: MessageEvent<InitMessage>) => {
  if (ev.data?.type !== "init") return;
  runIoWorkerServer(ev.data);
};

