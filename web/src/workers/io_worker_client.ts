import type { WorkerOpenToken } from "../storage/disk_image_store";

type OpenActiveDiskRequest = {
  id: number;
  type: "openActiveDisk";
  token: WorkerOpenToken;
};

type OpenActiveDiskResponse = {
  id: number;
  type: "openActiveDiskResult";
  ok: true;
  size: number;
  syncAccessHandleAvailable: boolean;
};

type OpenActiveDiskErrorResponse = {
  id: number;
  type: "openActiveDiskResult";
  ok: false;
  error: string;
};

type WorkerResponse = OpenActiveDiskResponse | OpenActiveDiskErrorResponse;

export class IoWorkerClient {
  readonly #worker: Worker;
  #nextId = 1;
  readonly #pending = new Map<
    number,
    { resolve: (value: OpenActiveDiskResponse) => void; reject: (err: Error) => void }
  >();

  constructor() {
    this.#worker = new Worker(new URL("./io.worker.ts", import.meta.url), { type: "module" });

    this.#worker.addEventListener("message", (event: MessageEvent<WorkerResponse>) => {
      const msg = event.data;
      const pending = this.#pending.get(msg.id);
      if (!pending) return;
      this.#pending.delete(msg.id);

      if (msg.ok) {
        pending.resolve(msg);
      } else {
        pending.reject(new Error(msg.error));
      }
    });
  }

  async openActiveDisk(token: WorkerOpenToken): Promise<OpenActiveDiskResponse> {
    const id = this.#nextId++;
    const req: OpenActiveDiskRequest = { id, type: "openActiveDisk", token };
    return await new Promise((resolve, reject) => {
      this.#pending.set(id, { resolve, reject });
      this.#worker.postMessage(req);
    });
  }
}

