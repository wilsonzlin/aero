import {
  isDemoVmWorkerMessage,
  type DemoVmWorkerInitResult,
  type DemoVmWorkerMessage,
  type DemoVmWorkerRequest,
  type DemoVmWorkerSerialOutputLenResult,
  type DemoVmWorkerSerialStatsResult,
  type DemoVmWorkerStepResult,
} from "./demo_vm_worker_protocol";

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

type PendingEntry = {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
};

type DemoVmWorkerClientOptions = {
  onStatus?: (status: DemoVmWorkerStepResult) => void;
  onError?: (message: string) => void;
  onFatalError?: (err: Error) => void;
};

export class DemoVmWorkerClient {
  #worker: Worker | null;
  #nextId = 1;
  #pending = new Map<number, PendingEntry>();
  #destroyed = false;
  #onStatus?: (status: DemoVmWorkerStepResult) => void;
  #onError?: (message: string) => void;
  #onFatalError?: (err: Error) => void;

  constructor(options: DemoVmWorkerClientOptions = {}) {
    this.#onStatus = options.onStatus;
    this.#onError = options.onError;
    this.#onFatalError = options.onFatalError;
    this.#worker = new Worker(new URL("./demo_vm.worker.ts", import.meta.url), { type: "module" });

    this.#worker.addEventListener("message", (event: MessageEvent<unknown>) => {
      const msg = event.data;
      if (!isDemoVmWorkerMessage(msg)) return;
      this.#handleMessage(msg);
    });

    this.#worker.addEventListener("error", (event: ErrorEvent) => {
      const err = new Error(event.message);
      this.#destroy(err);
      this.#onFatalError?.(err);
    });
  }

  async init(ramBytes: number): Promise<DemoVmWorkerInitResult> {
    return await this.#rpc<DemoVmWorkerInitResult>({ type: "init", ramBytes });
  }

  async runSteps(steps: number): Promise<DemoVmWorkerStepResult> {
    return await this.#rpc<DemoVmWorkerStepResult>({ type: "runSteps", steps });
  }

  async serialOutputLen(): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "getSerialOutputLen" });
  }

  async serialStats(): Promise<DemoVmWorkerSerialStatsResult> {
    return await this.#rpc<DemoVmWorkerSerialStatsResult>({ type: "getSerialStats" });
  }

  async snapshotFullToOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "snapshotFullToOpfs", path });
  }

  async snapshotDirtyToOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "snapshotDirtyToOpfs", path });
  }

  async restoreFromOpfs(path: string): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "restoreFromOpfs", path });
  }

  async shutdown(): Promise<void> {
    await this.#rpc<void>({ type: "shutdown" });
    this.terminate();
  }

  terminate(): void {
    this.#destroy(new Error("DemoVmWorkerClient terminated"));
  }

  #handleMessage(msg: DemoVmWorkerMessage): void {
    if (msg.type === "rpcResult") {
      const pendingReq = this.#pending.get(msg.id);
      if (!pendingReq) return;
      this.#pending.delete(msg.id);
      if (msg.ok) pendingReq.resolve(msg.result);
      else pendingReq.reject(new Error(msg.error));
      return;
    }

    if (msg.type === "status") {
      this.#onStatus?.({ steps: msg.steps, serialBytes: msg.serialBytes });
      return;
    }

    if (msg.type === "error") {
      this.#onError?.(msg.message);
    }
  }

  async #rpc<T>(msg: DistributiveOmit<DemoVmWorkerRequest, "id">): Promise<T> {
    if (this.#destroyed) throw new Error("DemoVmWorkerClient is destroyed.");
    const activeWorker = this.#worker;
    if (!activeWorker) throw new Error("DemoVmWorkerClient worker is unavailable.");

    const id = this.#nextId++;
    const req = { ...msg, id } as DemoVmWorkerRequest;
    return await new Promise<T>((resolve, reject) => {
      this.#pending.set(id, { resolve: (v) => resolve(v as T), reject });
      try {
        activeWorker.postMessage(req);
      } catch (err) {
        this.#pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  #destroy(reason: Error): void {
    if (this.#destroyed) return;
    this.#destroyed = true;

    for (const entry of this.#pending.values()) {
      entry.reject(reason);
    }
    this.#pending.clear();

    if (this.#worker) {
      this.#worker.terminate();
      this.#worker = null;
    }
  }
}
