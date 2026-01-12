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
}

export interface CompileBlockResponseMeta {
  wasm_byte_len: number;
  /**
   * Length of the compiled guest code block in bytes (from guest memory).
   *
   * Used by the tiered runtime to track self-modifying code invalidation via page versions.
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
