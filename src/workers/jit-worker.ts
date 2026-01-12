/// <reference lib="webworker" />

import type { CompileBlockRequest, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { initJitWasm, type JitWasmApi, type Tier1BlockCompilation } from '../../web/src/runtime/jit_wasm_loader';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

let sharedMemory: WebAssembly.Memory | null = null;
let guestBase = 0;
let guestSize = 0;

let jitWasmApiPromise: Promise<JitWasmApi> | null = null;

function postMessageToCpu(msg: JitToCpuMessage, transfer?: Transferable[]) {
  ctx.postMessage(msg, transfer ?? []);
}

async function loadJitWasmApi(): Promise<JitWasmApi> {
  if (jitWasmApiPromise) return await jitWasmApiPromise;
  jitWasmApiPromise = initJitWasm().then(({ api }) => api);
  return await jitWasmApiPromise;
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
    compilation = api.compile_tier1_block(
      BigInt(entryRip),
      codeWindow,
      maxInsts,
      maxBytes,
      true, // inline_tlb (Tier-1 ABI uses jit_ctx_ptr + fast-path TLB)
      true, // memory_shared (compiled blocks must import the shared guest memory)
    ) as Tier1BlockCompilation;
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
    const module = await WebAssembly.compile(wasmBytes);
    const base = {
      type: 'CompileBlockResponse' as const,
      id: req.id,
      entry_rip: req.entry_rip,
      meta: { wasm_byte_len: wasmBytes.byteLength, code_byte_len: compilation.code_byte_len },
    };

    // Prefer returning a compiled `WebAssembly.Module` (avoids compiling again in the CPU worker),
    // but fall back to raw bytes when structured cloning the module isn't supported.
    try {
      postMessageToCpu({ ...base, wasm_module: module });
    } catch {
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
      break;
    case 'CompileBlockRequest':
      void handleCompileRequest(msg);
      break;
    default:
      // Ignore unknown messages for forwards-compat.
      break;
  }
});
