export type JitImportsHint = Record<string, unknown>;

export type JitCompileRequest = {
  type: "jit:compile";
  id: number;
  /**
   * Raw WASM bytes to compile.
   *
   * Callers should transfer this buffer when sending the request to avoid
   * copying large JIT blocks (`postMessage(req, [req.wasmBytes])`).
   */
  wasmBytes: ArrayBuffer;
  importsHint?: JitImportsHint;
};

export type JitCompiledResponse = {
  type: "jit:compiled";
  id: number;
  module: WebAssembly.Module;
  /** Time spent compiling (or retrieving from cache), in milliseconds. */
  durationMs: number;
  /** Whether the response was served from the in-worker cache. */
  cached?: boolean;
};

export type JitErrorCode = "csp_blocked" | "compile_failed" | "unsupported";

export type JitErrorResponse = {
  type: "jit:error";
  id: number;
  message: string;
  code?: JitErrorCode;
  /** Time spent before failing, in milliseconds. */
  durationMs?: number;
};

export type JitWorkerRequest = JitCompileRequest;
export type JitWorkerResponse = JitCompiledResponse | JitErrorResponse;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function isJitCompileRequest(value: unknown): value is JitCompileRequest {
  if (!isRecord(value)) return false;
  if (value.type !== "jit:compile") return false;
  if (typeof value.id !== "number") return false;
  return value.wasmBytes instanceof ArrayBuffer;
}

export function isJitWorkerResponse(value: unknown): value is JitWorkerResponse {
  if (!isRecord(value)) return false;
  if (typeof value.id !== "number") return false;
  if (value.type === "jit:error") {
    if (typeof value.message !== "string") return false;
    if (value.code !== undefined && typeof value.code !== "string") return false;
    if (value.durationMs !== undefined && typeof value.durationMs !== "number") return false;
    return true;
  }

  if (value.type === "jit:compiled") {
    if (typeof value.durationMs !== "number") return false;
    if (typeof value.cached !== "boolean" && value.cached !== undefined) return false;
    if (!("module" in value)) return false;
    const mod = (value as { module: unknown }).module;
    return typeof mod === "object" && mod !== null;
  }
  return false;
}
