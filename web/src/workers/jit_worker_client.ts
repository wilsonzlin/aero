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

  constructor(worker: Worker) {
    this.#worker = worker;
    this.#worker.addEventListener("message", (event) => {
      const data = event.data as unknown;
      if (!isJitWorkerResponse(data)) return;
      const pending = this.#pending.get(data.id);
      if (!pending) return;
      this.#pending.delete(data.id);
      globalThis.clearTimeout(pending.timeoutId);
      pending.resolve(data);
    });
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
}
