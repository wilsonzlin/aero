/// <reference lib="webworker" />

import type { CompileBlockRequest, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { initJitWasmForContext, type JitWasmApi, type Tier1BlockCompilation } from '../../web/src/runtime/jit_wasm_loader';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let sharedMemory: WebAssembly.Memory | null = null;
let guestBase = 0;
let guestSize = 0;

let jitWasmApiPromise: Promise<JitWasmApi> | null = null;
let canPostWasmModule: boolean | null = null;

function isDataCloneError(err: unknown): boolean {
  const domException = (globalThis as unknown as { DOMException?: unknown }).DOMException;
  if (typeof domException === 'function') {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    if (err instanceof (domException as any) && (err as { name?: unknown }).name === 'DataCloneError') return true;
  }
  if (err && typeof err === 'object') {
    const name = (err as { name?: unknown }).name;
    if (name === 'DataCloneError') return true;
  }
  const message = err instanceof Error ? err.message : String(err);
  return /DataCloneError|could not be cloned/i.test(message);
}

function postMessageToCpu(msg: JitToCpuMessage, transfer?: Transferable[]) {
  ctx.postMessage(msg, transfer ?? []);
}

async function loadJitWasmApi(): Promise<JitWasmApi> {
  if (!jitWasmApiPromise) {
    jitWasmApiPromise = initJitWasmForContext().then(({ api }) => api);
  }
  try {
    return await jitWasmApiPromise;
  } catch (err) {
    // Allow retries if initialization fails (e.g. missing assets during dev or CSP restrictions).
    jitWasmApiPromise = null;
    throw err;
  }
}

function clampU32(n: number): number {
  if (!Number.isFinite(n) || n < 0) return 0;
  return n > 0xffffffff ? 0xffffffff : (n >>> 0);
}

function sliceCodeWindow(entryRip: number, maxBytes: number): Uint8Array {
  if (!sharedMemory) throw new Error('shared memory not initialized');
  const buf = sharedMemory.buffer;

  const entry = clampU32(entryRip);
  const max = clampU32(maxBytes);
  const effectiveMax = max === 0 ? 1024 : max;
  const desiredLen = effectiveMax + 15; // decoder may read up to 15 bytes per instruction

  const availableGuest = Math.max(0, guestSize - entry);
  const lenGuest = Math.min(desiredLen, availableGuest);

  const base = guestBase + entry;
  if (base < 0 || base > buf.byteLength) {
    throw new Error(
      `entry_rip out of wasm memory bounds: entry_rip=0x${entryRip.toString(16)} guest_base=0x${guestBase.toString(16)} wasm_bytes=0x${buf.byteLength.toString(16)}`,
    );
  }
  const availableBuf = Math.max(0, buf.byteLength - base);
  const len = Math.min(lenGuest, availableBuf);

  return new Uint8Array(buf, base, len);
}

function toOwnedArrayBufferBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  // Always copy into an ArrayBuffer-backed view so:
  // - WebAssembly.validate/compile accept it under `ES2024.SharedMemory` libs
  // - We never accidentally transfer/detach a large wasm memory buffer
  // - It is safe to transfer the exact wasm bytes payload.
  return new Uint8Array(bytes) as Uint8Array<ArrayBuffer>;
}

function normalizeTier1Compilation(result: unknown, fallbackCodeByteLen: number): Tier1BlockCompilation {
  // The Tier-1 wasm-bindgen ABI is still evolving. Older builds returned raw `Uint8Array` bytes,
  // while newer ones return a small object with `{ wasm_bytes, code_byte_len, ... }`.
  if (result instanceof Uint8Array) {
    return {
      wasm_bytes: result,
      code_byte_len: fallbackCodeByteLen,
      exit_to_interpreter: false,
    };
  }

  if (result && typeof result === 'object') {
    const wasmBytes = (result as Partial<Tier1BlockCompilation>).wasm_bytes;
    if (!(wasmBytes instanceof Uint8Array)) {
      throw new Error('JIT compiler returned unexpected result (missing wasm_bytes Uint8Array)');
    }
    const codeByteLen =
      typeof (result as Partial<Tier1BlockCompilation>).code_byte_len === 'number'
        ? (result as Partial<Tier1BlockCompilation>).code_byte_len!
        : fallbackCodeByteLen;
    const exitToInterp =
      typeof (result as Partial<Tier1BlockCompilation>).exit_to_interpreter === 'boolean'
        ? (result as Partial<Tier1BlockCompilation>).exit_to_interpreter!
        : false;
    return { wasm_bytes: wasmBytes, code_byte_len: codeByteLen, exit_to_interpreter: exitToInterp };
  }

  throw new Error('JIT compiler returned unexpected result (expected Uint8Array or { wasm_bytes: Uint8Array, ... })');
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

  const entryRip = clampU32(req.entry_rip);
  if (entryRip >= guestSize) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: `entry_rip out of guest RAM bounds: entry_rip=0x${entryRip.toString(16)} guest_size=0x${guestSize.toString(16)}`,
    });
    return;
  }

  const maxBytes = req.max_bytes > 0 ? req.max_bytes : 1024;
  const maxInsts = 64;

  let compilation: Tier1BlockCompilation;
  try {
    const api = await loadJitWasmApi();
    const codeWindow = sliceCodeWindow(entryRip, maxBytes);
    // Copy bytes into an unshared buffer so compilation cannot race with guest writes.
    const codeBytes = new Uint8Array(codeWindow);
    let result: unknown;
    try {
      result = api.compile_tier1_block(
        BigInt(entryRip),
        codeBytes,
        maxInsts,
        maxBytes,
        true, // inline_tlb (Tier-1 ABI uses jit_ctx_ptr + fast-path TLB)
        true, // memory_shared (compiled blocks must import the shared guest memory)
      ) as unknown;
    } catch {
      // Backwards-compat: older JIT wasm builds used simpler argument lists.
      const compileTier1BlockCompat = api.compile_tier1_block as unknown as (...args: any[]) => unknown;
      try {
        result = compileTier1BlockCompat(entryRip, maxBytes) as unknown;
      } catch {
        result = compileTier1BlockCompat(codeBytes, entryRip, maxBytes) as unknown;
      }
    }
    compilation = normalizeTier1Compilation(result, maxBytes);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    postMessageToCpu({ type: 'CompileError', id: req.id, entry_rip: req.entry_rip, reason: message });
    return;
  }

  const wasmBytes = toOwnedArrayBufferBytes(compilation.wasm_bytes);
  if (!WebAssembly.validate(wasmBytes)) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: 'WebAssembly.validate failed for compiled Tier-1 block',
    });
    return;
  }

  try {
    const base = {
      type: 'CompileBlockResponse' as const,
      id: req.id,
      entry_rip: req.entry_rip,
      meta: { wasm_byte_len: wasmBytes.byteLength, code_byte_len: compilation.code_byte_len },
    };

    if (canPostWasmModule === false) {
      postMessageToCpu({ ...base, wasm_bytes: wasmBytes }, [wasmBytes.buffer]);
      return;
    }

    const module = await WebAssembly.compile(wasmBytes);

    // Prefer returning a compiled `WebAssembly.Module` (avoids compiling again in the CPU worker),
    // but fall back to raw bytes when structured cloning the module isn't supported.
    try {
      postMessageToCpu({ ...base, wasm_module: module });
      canPostWasmModule = true;
    } catch (err) {
      // If WebAssembly.Module cannot be structured-cloned, avoid compiling modules next time.
      if (canPostWasmModule === null && isDataCloneError(err)) canPostWasmModule = false;
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
      guestBase = msg.guest_base;
      guestSize = msg.guest_size;
      // Warm up the compiler module in the background so the first hot block compile has lower latency.
      void loadJitWasmApi().catch(() => {});
      break;
    case 'CompileBlockRequest':
      void handleCompileRequest(msg);
      break;
    default:
      // Ignore unknown messages for forwards-compat.
      break;
  }
});
