import {
  type JitCompileRequest,
  type JitTier1Bitness,
  type JitTier1CompileRequest,
  type JitTier1CompiledResponse,
  type JitWorkerResponse,
  isJitWorkerResponse,
  type JitImportsHint,
} from "./jit_protocol";
import { formatOneLineError } from "../text";
import { unrefBestEffort } from "../unrefSafe";

export type Tier1CompileResult =
  | { module: WebAssembly.Module; entryRip: number | bigint; codeByteLen: number; exitToInterpreter: boolean }
  | { wasmBytes: ArrayBuffer; entryRip: number | bigint; codeByteLen: number; exitToInterpreter: boolean };

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
  #destroyed = false;

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
      this.#rejectAll(new Error(formatOneLineError(message, 512, "JIT worker error")));
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
    if (this.#destroyed) {
      return Promise.reject(new Error("JitWorkerClient is destroyed."));
    }
    // Note: `wasmBytes` will be transferred to the worker, so the caller must not
    // use this ArrayBuffer after calling `compile()`.
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
      unrefBestEffort(timeoutId);

      this.#pending.set(id, { resolve, reject, timeoutId });

      try {
        this.#worker.postMessage(req, [wasmBytes]);
      } catch (err) {
        globalThis.clearTimeout(timeoutId);
        this.#pending.delete(id);
        reject(err instanceof Error ? err : new Error(formatOneLineError(err, 512)));
      }
    });
  }

  compileTier1(
    entryRip: number | bigint,
    opts?: { codeBytes?: Uint8Array; maxBytes?: number; bitness?: JitTier1Bitness; memoryShared?: boolean; timeoutMs?: number },
  ): Promise<Tier1CompileResult> {
    if (this.#destroyed) {
      return Promise.reject(new Error("JitWorkerClient is destroyed."));
    }
    const id = this.#nextId++;
    const timeoutMs = Math.max(0, opts?.timeoutMs ?? 10_000);

    const req: JitTier1CompileRequest = {
      type: "jit:tier1",
      id,
      entryRip,
      maxBytes: opts?.maxBytes ?? 1024,
      bitness: opts?.bitness ?? 64,
      memoryShared: opts?.memoryShared ?? true,
      ...(opts?.codeBytes ? { codeBytes: opts.codeBytes } : {}),
    };

    const transfer: Transferable[] = [];
    const bytes = opts?.codeBytes;
    if (
      bytes &&
      bytes.buffer instanceof ArrayBuffer &&
      bytes.byteOffset === 0 &&
      bytes.byteLength === bytes.buffer.byteLength
    ) {
      transfer.push(bytes.buffer);
    }

    return new Promise((resolve, reject) => {
      const timeoutId = globalThis.setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error("Timed out waiting for JIT worker response."));
      }, timeoutMs);
      unrefBestEffort(timeoutId);

      this.#pending.set(id, {
        resolve: (value) => {
          if (value.type === "jit:error") {
            reject(new Error(value.message));
            return;
          }
          if (value.type !== "jit:tier1:compiled") {
            reject(new Error(`Unexpected JIT worker response type: ${value.type}`));
            return;
          }
          const response = value as JitTier1CompiledResponse;
          if (response.module instanceof WebAssembly.Module) {
            resolve({
              module: response.module,
              entryRip: response.entryRip,
              codeByteLen: response.codeByteLen,
              exitToInterpreter: response.exitToInterpreter,
            });
            return;
          }
          if (!(response.wasmBytes instanceof ArrayBuffer)) {
            reject(new Error("Invalid JIT tier1 response: missing module/wasmBytes payload."));
            return;
          }
          resolve({
            wasmBytes: response.wasmBytes,
            entryRip: response.entryRip,
            codeByteLen: response.codeByteLen,
            exitToInterpreter: response.exitToInterpreter,
          });
        },
        reject,
        timeoutId,
      });

      try {
        if (transfer.length) {
          this.#worker.postMessage(req, transfer);
        } else {
          this.#worker.postMessage(req);
        }
      } catch (err) {
        globalThis.clearTimeout(timeoutId);
        this.#pending.delete(id);
        reject(err instanceof Error ? err : new Error(formatOneLineError(err, 512)));
      }
    });
  }

  #rejectAll(err: Error): void {
    const pending = Array.from(this.#pending.values());
    this.#pending.clear();
    for (const entry of pending) {
      globalThis.clearTimeout(entry.timeoutId);
      entry.reject(err);
    }
  }

  destroy(reason: Error = new Error("JitWorkerClient destroyed")): void {
    if (this.#destroyed) return;
    this.#destroyed = true;

    this.#worker.removeEventListener("message", this.#onMessage);
    this.#worker.removeEventListener("error", this.#onError);
    this.#worker.removeEventListener("messageerror", this.#onMessageError);

    this.#rejectAll(reason);
  }
}
