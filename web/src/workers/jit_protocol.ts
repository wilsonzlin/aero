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

export type JitTier1Bitness = 16 | 32 | 64;

export type JitTier1CompileRequest = {
  type: "jit:tier1";
  id: number;
  /**
   * Guest RIP to compile at.
   *
   * Note: the Tier-1 compiler takes a BigInt; callers may pass either a `number`
   * (u32) or a `bigint`.
   */
  entryRip: number | bigint;
  /**
   * Optional explicit code bytes to compile.
   *
   * When omitted, the JIT worker is expected to snapshot bytes out of its shared
   * guest memory (see {@link memoryShared}).
   */
  codeBytes?: Uint8Array;
  /**
   * Maximum guest code bytes to decode.
   */
  maxBytes: number;
  /**
   * x86 decode bitness (16/32/64).
   */
  bitness: JitTier1Bitness;
  /**
   * Whether the compiled block should import shared memory (`shared: true`).
   *
   * In the worker runtime this is typically true (guest RAM is a shared WebAssembly.Memory).
   */
  memoryShared: boolean;
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

export type JitTier1CompiledResponse = {
  type: "jit:tier1:compiled";
  id: number;
  entryRip: number | bigint;
  codeByteLen: number;
  exitToInterpreter: boolean;
} & ({ module: WebAssembly.Module; wasmBytes?: never } | { wasmBytes: ArrayBuffer; module?: never });

export type JitErrorCode = "csp_blocked" | "compile_failed" | "unsupported";

export type JitErrorResponse = {
  type: "jit:error";
  id: number;
  message: string;
  code?: JitErrorCode;
  /** Time spent before failing, in milliseconds. */
  durationMs?: number;
};

export type JitWorkerRequest = JitCompileRequest | JitTier1CompileRequest;
export type JitWorkerResponse = JitCompiledResponse | JitTier1CompiledResponse | JitErrorResponse;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

export function isJitCompileRequest(value: unknown): value is JitCompileRequest {
  if (!isRecord(value)) return false;
  if (value.type !== "jit:compile") return false;
  if (typeof value.id !== "number") return false;
  return value.wasmBytes instanceof ArrayBuffer;
}

export function isJitTier1CompileRequest(value: unknown): value is JitTier1CompileRequest {
  if (!isRecord(value)) return false;
  if (value.type !== "jit:tier1") return false;
  if (typeof value.id !== "number") return false;
  const entryRip = (value as { entryRip?: unknown }).entryRip;
  if (typeof entryRip !== "number" && typeof entryRip !== "bigint") return false;
  const maxBytes = (value as { maxBytes?: unknown }).maxBytes;
  if (typeof maxBytes !== "number") return false;
  const bitness = (value as { bitness?: unknown }).bitness;
  if (bitness !== 16 && bitness !== 32 && bitness !== 64) return false;
  const memoryShared = (value as { memoryShared?: unknown }).memoryShared;
  if (typeof memoryShared !== "boolean") return false;
  const codeBytes = (value as { codeBytes?: unknown }).codeBytes;
  if (codeBytes !== undefined && !(codeBytes instanceof Uint8Array)) return false;
  return true;
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

  if (value.type === "jit:tier1:compiled") {
    const msg = value as Partial<JitTier1CompiledResponse> & { [key: string]: unknown };
    if (typeof msg.codeByteLen !== "number") return false;
    if (typeof msg.exitToInterpreter !== "boolean") return false;
    if (typeof msg.entryRip !== "number" && typeof msg.entryRip !== "bigint") return false;
    if ("module" in msg) {
      const mod = msg.module;
      if (typeof mod !== "object" || mod === null) return false;
      if ("wasmBytes" in msg && msg.wasmBytes !== undefined) return false;
      return true;
    }
    if (!("wasmBytes" in msg)) return false;
    if (!(msg.wasmBytes instanceof ArrayBuffer)) return false;
    if ("module" in msg && msg.module !== undefined) return false;
    return true;
  }
  return false;
}
