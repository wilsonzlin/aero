/// <reference lib="webworker" />
import type { RuntimeDiskRequestMessage } from "./runtime_disk_protocol";
import { RuntimeDiskWorker } from "./runtime_disk_worker_impl";

const scope = globalThis as unknown as DedicatedWorkerGlobalScope;

const impl = new RuntimeDiskWorker((msg, transfer) => {
  scope.postMessage(msg, transfer ?? []);
});

scope.onmessage = (ev: MessageEvent<RuntimeDiskRequestMessage>) => {
  void impl.handleMessage(ev.data as RuntimeDiskRequestMessage);
};
