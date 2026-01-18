import {
  isDemoVmWorkerMessage,
  type DemoVmWorkerInitResult,
  type DemoVmWorkerMessage,
  type DemoVmWorkerRequest,
  type DemoVmWorkerSerializedError,
  type DemoVmWorkerSerialOutputLenResult,
  type DemoVmWorkerSerialStatsResult,
  type DemoVmWorkerStepResult,
} from "./demo_vm_worker_protocol";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

type PendingEntry = {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
};

type DemoVmWorkerClientOptions = {
  onStatus?: (status: DemoVmWorkerStepResult) => void;
  onError?: (err: Error) => void;
  onFatalError?: (err: Error) => void;
};

type RpcOptions = { timeoutMs?: number };

export class DemoVmWorkerClient {
  #worker: Worker | null;
  #nextId = 1;
  #pending = new Map<number, PendingEntry>();
  #destroyed = false;
  #onStatus?: (status: DemoVmWorkerStepResult) => void;
  #onError?: (err: Error) => void;
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

    this.#worker.addEventListener("messageerror", () => {
      const err = new Error("DemoVmWorkerClient worker message could not be deserialized.");
      this.#destroy(err);
      this.#onFatalError?.(err);
    });

    this.#worker.addEventListener("error", (event: ErrorEvent) => {
      const err = new Error(formatOneLineError(event.message, 512, "DemoVm worker error"));
      this.#destroy(err);
      this.#onFatalError?.(err);
    });
  }

  async init(ramBytes: number, options: RpcOptions = {}): Promise<DemoVmWorkerInitResult> {
    return await this.#rpc<DemoVmWorkerInitResult>({ type: "init", ramBytes }, options);
  }

  async runSteps(steps: number, options: RpcOptions = {}): Promise<DemoVmWorkerStepResult> {
    return await this.#rpc<DemoVmWorkerStepResult>({ type: "runSteps", steps }, options);
  }

  async serialOutputLen(options: RpcOptions = {}): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "getSerialOutputLen" }, options);
  }

  async serialStats(options: RpcOptions = {}): Promise<DemoVmWorkerSerialStatsResult> {
    return await this.#rpc<DemoVmWorkerSerialStatsResult>({ type: "getSerialStats" }, options);
  }

  async snapshotFullToOpfs(path: string, options: RpcOptions = {}): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "snapshotFullToOpfs", path }, options);
  }

  async snapshotDirtyToOpfs(path: string, options: RpcOptions = {}): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "snapshotDirtyToOpfs", path }, options);
  }

  async restoreFromOpfs(path: string, options: RpcOptions = {}): Promise<DemoVmWorkerSerialOutputLenResult> {
    return await this.#rpc<DemoVmWorkerSerialOutputLenResult>({ type: "restoreFromOpfs", path }, options);
  }

  async shutdown(): Promise<void> {
    await this.#rpc<void>({ type: "shutdown" });
    this.terminate();
  }

  terminate(): void {
    this.#destroy(new Error("DemoVmWorkerClient terminated"));
  }

  #deserializeError(serialized: DemoVmWorkerSerializedError): Error {
    const err = new Error(serialized.message);
    err.name = serialized.name;
    if (serialized.stack) {
      try {
        err.stack = serialized.stack;
      } catch {
        // ignore (some runtimes expose stack as readonly)
      }
    }
    return err;
  }

  #handleMessage(msg: DemoVmWorkerMessage): void {
    if (msg.type === "rpcResult") {
      const pendingReq = this.#pending.get(msg.id);
      if (!pendingReq) return;
      this.#pending.delete(msg.id);
      if (msg.ok) pendingReq.resolve(msg.result);
      else {
        pendingReq.reject(this.#deserializeError(msg.error));
      }
      return;
    }

    if (msg.type === "status") {
      this.#onStatus?.({ steps: msg.steps, serialBytes: msg.serialBytes });
      return;
    }

    if (msg.type === "error") {
      this.#onError?.(this.#deserializeError(msg.error));
    }
  }

  async #rpc<T>(msg: DistributiveOmit<DemoVmWorkerRequest, "id">, options: RpcOptions = {}): Promise<T> {
    if (this.#destroyed) throw new Error("DemoVmWorkerClient is destroyed.");
    const activeWorker = this.#worker;
    if (!activeWorker) throw new Error("DemoVmWorkerClient worker is unavailable.");

    const id = this.#nextId++;
    const req = { ...msg, id } as DemoVmWorkerRequest;
    return await new Promise<T>((resolve, reject) => {
      const timeoutMs = options.timeoutMs ?? 0;
      let timeout: ReturnType<typeof setTimeout> | null = null;
      const clear = () => {
        if (timeout !== null) {
          clearTimeout(timeout);
          timeout = null;
        }
      };

      this.#pending.set(id, {
        resolve: (v) => {
          clear();
          resolve(v as T);
        },
        reject: (err) => {
          clear();
          reject(err);
        },
      });

      if (timeoutMs > 0) {
        timeout = setTimeout(() => {
          this.#pending.delete(id);
          reject(new Error(`Timed out waiting for demo VM worker RPC (${req.type}) (${timeoutMs}ms).`));
        }, timeoutMs);
        unrefBestEffort(timeout);
      }
      try {
        activeWorker.postMessage(req);
      } catch (err) {
        this.#pending.delete(id);
        clear();
        reject(err instanceof Error ? err : new Error(formatOneLineError(err, 512)));
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
