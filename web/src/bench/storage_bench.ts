import type { StorageBenchOpts, StorageBenchResult } from "./storage_types";

type WorkerRequest = {
  type: "run";
  id: string;
  opts: StorageBenchOpts | undefined;
};

type WorkerResponse =
  | { type: "result"; id: string; result: StorageBenchResult }
  | { type: "error"; id: string; error: string };

function createRequestId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export async function runStorageBench(opts?: StorageBenchOpts): Promise<StorageBenchResult> {
  const worker = new Worker(new URL("./storage_worker.ts", import.meta.url), {
    type: "module",
  });

  const id = createRequestId();

  const result = await new Promise<StorageBenchResult>((resolve, reject) => {
    const onMessage = (event: MessageEvent<WorkerResponse>) => {
      const msg = event.data;
      if (!msg || msg.id !== id) return;

      worker.removeEventListener("message", onMessage);
      worker.removeEventListener("error", onError);

      if (msg.type === "result") resolve(msg.result);
      else reject(new Error(msg.error));
    };

    const onError = (event: ErrorEvent) => {
      worker.removeEventListener("message", onMessage);
      worker.removeEventListener("error", onError);
      reject(event.error ?? new Error(event.message));
    };

    worker.addEventListener("message", onMessage);
    worker.addEventListener("error", onError);

    const req: WorkerRequest = { type: "run", id, opts };
    worker.postMessage(req);
  }).finally(() => {
    worker.terminate();
  });

  return result;
}

