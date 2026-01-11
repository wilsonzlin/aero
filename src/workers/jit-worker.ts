/// <reference lib="webworker" />

import type { CompileBlockRequest, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { copyWasmBytes, JIT_BLOCK_WASM_BYTES } from './wasm-bytes';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let sharedMemory: WebAssembly.Memory | null = null;

function postMessageToCpu(msg: JitToCpuMessage, transfer?: Transferable[]) {
  ctx.postMessage(msg, transfer ?? []);
}

async function handleCompileRequest(req: CompileBlockRequest & { type: 'CompileBlockRequest' }) {
  if (!sharedMemory) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: 'JIT worker not initialized with shared memory',
    });
    return;
  }

  // Placeholder for the real Rust JIT (`crates/aero-jit-proto` via wasm-bindgen).
  // The glue is identical: generate bytes → validate → WebAssembly.compile().
  const wasmBytes = copyWasmBytes(JIT_BLOCK_WASM_BYTES);

  if (!WebAssembly.validate(wasmBytes)) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: 'WebAssembly.validate failed for generated block',
    });
    return;
  }

  try {
    const module = await WebAssembly.compile(wasmBytes);
    const base = {
      type: 'CompileBlockResponse' as const,
      id: req.id,
      entry_rip: req.entry_rip,
      meta: { wasm_byte_len: wasmBytes.byteLength },
    };

    // Prefer returning a compiled `WebAssembly.Module` (avoids compiling again in the CPU worker),
    // but fall back to raw bytes when structured cloning the module isn't supported.
    try {
      postMessageToCpu({ ...base, wasm_module: module });
    } catch (err) {
      postMessageToCpu({ ...base, wasm_bytes: wasmBytes }, [wasmBytes.buffer]);
    }
  } catch (err) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: err instanceof Error ? err.message : String(err),
    });
  }
}

ctx.addEventListener('message', (ev: MessageEvent<CpuToJitMessage>) => {
  const msg = ev.data;
  switch (msg.type) {
    case 'JitWorkerInit':
      sharedMemory = msg.memory;
      break;
    case 'CompileBlockRequest':
      void handleCompileRequest(msg);
      break;
    default:
      // Ignore unknown messages for forwards-compat.
      break;
  }
});
