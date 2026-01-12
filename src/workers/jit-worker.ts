/// <reference lib="webworker" />

import type { CompileBlockRequest, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { copyWasmBytes, JIT_BLOCK_WASM_BYTES } from './wasm-bytes';
import { initJitWasm, type JitWasmApi } from '../../web/src/runtime/jit_wasm_loader';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let sharedMemory: WebAssembly.Memory | null = null;
let jitWasmApiPromise: Promise<JitWasmApi | null> | null = null;

function postMessageToCpu(msg: JitToCpuMessage, transfer?: Transferable[]) {
  ctx.postMessage(msg, transfer ?? []);
}

async function maybeLoadJitWasmApi(): Promise<JitWasmApi | null> {
  if (jitWasmApiPromise) return await jitWasmApiPromise;
  jitWasmApiPromise = initJitWasm()
    .then(({ api }) => api)
    .catch((err) => {
      const message = err instanceof Error ? err.message : String(err);
      console.warn(`[jit-worker] Failed to init aero-jit-wasm; falling back to placeholder bytes. Error: ${message}`);
      return null;
    });
  return await jitWasmApiPromise;
}

function clampToU32(n: number): number {
  if (!Number.isFinite(n) || n < 0) return 0;
  return n > 0xffffffff ? 0xffffffff : (n >>> 0);
}

function sliceCodeWindow(memory: WebAssembly.Memory, entryRip: number, maxBytes: number): Uint8Array {
  const buf = memory.buffer;
  const entry = clampToU32(entryRip);
  const desiredLen = clampToU32(maxBytes) + 15;
  const available = Math.max(0, buf.byteLength - entry);
  const len = Math.min(desiredLen, available);
  return new Uint8Array(buf, entry, len);
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

  // The synthetic JIT smoke test sends `max_bytes=0`; keep returning the static fixture until
  // a real guest code pipeline is wired up.
  const shouldUseWasmCompiler = req.max_bytes > 0;

  let wasmBytes: Uint8Array<ArrayBuffer>;
  if (shouldUseWasmCompiler) {
    const api = await maybeLoadJitWasmApi();
    if (api) {
      try {
        // Tier-1 WASM ABI is still evolving; attempt a couple of call shapes to keep the worker glue
        // compatible across iterations.
        try {
          wasmBytes = api.compile_tier1_block(req.entry_rip, req.max_bytes) as Uint8Array<ArrayBuffer>;
        } catch {
          const codeWindow = sliceCodeWindow(sharedMemory, req.entry_rip, req.max_bytes);
          wasmBytes = api.compile_tier1_block(codeWindow, req.entry_rip, req.max_bytes) as Uint8Array<ArrayBuffer>;
        }
      } catch (err) {
        postMessageToCpu({
          type: 'CompileError',
          id: req.id,
          entry_rip: req.entry_rip,
          reason: err instanceof Error ? err.message : String(err),
        });
        return;
      }
    } else {
      wasmBytes = copyWasmBytes(JIT_BLOCK_WASM_BYTES);
    }
  } else {
    // Placeholder for the real Rust JIT (likely `crates/aero-jit-x86` exposed via wasm-bindgen).
    // The glue is identical: generate bytes → validate → WebAssembly.compile().
    wasmBytes = copyWasmBytes(JIT_BLOCK_WASM_BYTES);
  }

  if (!(wasmBytes instanceof Uint8Array)) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: 'JIT compiler returned non-Uint8Array wasm bytes',
    });
    return;
  }

  let wasmValid = false;
  try {
    wasmValid = WebAssembly.validate(wasmBytes);
  } catch (err) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: err instanceof Error ? err.message : String(err),
    });
    return;
  }

  if (!wasmValid) {
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
