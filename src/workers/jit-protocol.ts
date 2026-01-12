export type JitCompileMode = 'tier1';

export interface CompileBlockRequest {
  id: number;
  entry_rip: number;
  mode: JitCompileMode;
  max_bytes: number;
  /**
   * x86 decode bitness for the guest CPU mode at `entry_rip`.
   *
   * - 16: real mode / 16-bit protected mode
   * - 32: protected mode
   * - 64: long mode
   *
   * Optional: omitted/0 defaults to 64 for backwards compatibility.
   */
  bitness?: number;
  /**
   * Debug-only barrier used by the JIT smoke test to force a deterministic stale
   * compilation race (CPU mutates code after the JIT worker reads bytes but before
   * it responds).
   */
  debug_sync?: boolean;
}

export interface CompileBlockResponseMeta {
  wasm_byte_len: number;
  /**
   * Number of guest code bytes consumed by this compiled block.
   *
   * Used by the CPU worker to shrink the pre-snapshotted page-version metadata to
   * the actual block length (so stale installs can be rejected).
   */
  code_byte_len: number;
}

export interface CompileBlockResponse {
  id: number;
  entry_rip: number;
  wasm_bytes?: Uint8Array<ArrayBuffer>;
  wasm_module?: WebAssembly.Module;
  meta: CompileBlockResponseMeta;
}

export interface CompileError {
  id: number;
  entry_rip: number;
  reason: string;
}

export type CpuToJitMessage =
  | { type: 'JitWorkerInit'; memory: WebAssembly.Memory; guest_base: number; guest_size: number }
  | ({ type: 'CompileBlockRequest' } & CompileBlockRequest);

export type JitToCpuMessage =
  | ({ type: 'CompileBlockResponse' } & CompileBlockResponse)
  | ({ type: 'CompileError' } & CompileError);
