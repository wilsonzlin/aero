export type JitCompileMode = 'tier1';

export interface CompileBlockRequest {
  id: number;
  entry_rip: number;
  mode: JitCompileMode;
  max_bytes: number;
}

export interface CompileBlockResponseMeta {
  wasm_byte_len: number;
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
  | { type: 'JitWorkerInit'; memory: WebAssembly.Memory }
  | ({ type: 'CompileBlockRequest' } & CompileBlockRequest);

export type JitToCpuMessage =
  | ({ type: 'CompileBlockResponse' } & CompileBlockResponse)
  | ({ type: 'CompileError' } & CompileError);
