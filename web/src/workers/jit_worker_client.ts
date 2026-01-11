import {
  type JitCompileRequest,
  type JitWorkerResponse,
  isJitWorkerResponse,
  type JitImportsHint,
} from "./jit_protocol";

export class JitWorkerClient {
  readonly #worker: Worker;
  #nextId = 1;
  readonly #pending = new Map<
    number,
    { resolve: (value: JitWorkerResponse) => void; reject: (err: Error) => void; timeoutId: ReturnType<typeof setTimeout> }
  >();
  readonly #onMessage: (event: MessageEvent) => void;
  readonly #onError: (event: Event) => void;
  readonly #onMessageError: () => void;

  constructor(worker: Worker) {
    this.#worker = worker;

    this.#onMessage = (event: MessageEvent) => {
      const data = event.data as unknown;
      if (!isJitWorkerResponse(data)) return;
      const pending = this.#pending.get(data.id);
      if (!pending) return;
      this.#pending.delete(data.id);
      globalThis.clearTimeout(pending.timeoutId);
      pending.resolve(data);
    };

    this.#onError = (event: Event) => {
      const message =
        typeof (event as ErrorEvent | undefined)?.message === "string"
          ? (event as ErrorEvent).message
          : "JIT worker error";
      this.#rejectAll(new Error(message));
    };

    this.#onMessageError = () => {
      this.#rejectAll(new Error("JIT worker message deserialization failed"));
    };

    this.#worker.addEventListener("message", this.#onMessage);
    this.#worker.addEventListener("error", this.#onError);
    this.#worker.addEventListener("messageerror", this.#onMessageError);
  }

  compile(
    wasmBytes: ArrayBuffer,
    opts?: { importsHint?: JitImportsHint; timeoutMs?: number },
  ): Promise<JitWorkerResponse> {
    const id = this.#nextId++;
    const timeoutMs = Math.max(0, opts?.timeoutMs ?? 10_000);

    const req: JitCompileRequest = {
      type: "jit:compile",
      id,
      wasmBytes,
      importsHint: opts?.importsHint,
    };

    return new Promise((resolve, reject) => {
      const timeoutId = globalThis.setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error("Timed out waiting for JIT worker response."));
      }, timeoutMs);

      this.#pending.set(id, { resolve, reject, timeoutId });

      try {
        this.#worker.postMessage(req, [wasmBytes]);
      } catch (err) {
        globalThis.clearTimeout(timeoutId);
        this.#pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  private #rejectAll(err: Error): void {
    const pending = Array.from(this.#pending.values());
    this.#pending.clear();
    for (const entry of pending) {
      globalThis.clearTimeout(entry.timeoutId);
      entry.reject(err);
    }
  }
}
