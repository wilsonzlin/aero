/// <reference lib="webworker" />

import type { CompileBlockRequest, CpuToJitMessage, JitToCpuMessage } from './jit-protocol';
import { initJitWasmForContext, type JitWasmApi, type Tier1BlockCompilation } from '../../web/src/runtime/jit_wasm_loader';
import { formatOneLineError } from '../text.js';

const ctx = self as unknown as DedicatedWorkerGlobalScope;

// Keep these values aligned with the Tier-1 compiler's expectations:
// - x86 instruction decoder can read up to 15 bytes per instruction.
// - `aero-jit-wasm` caps the maximum input code slice to 1MiB.
const DEFAULT_MAX_BYTES = 1024;
const DECODE_WINDOW_SLACK_BYTES = 15;
const MAX_COMPILER_CODE_BYTES = 1024 * 1024;

let sharedMemory: WebAssembly.Memory | null = null;
let guestBase = 0;
let guestSize = 0;

let jitWasmApiPromise: Promise<JitWasmApi> | null = null;
let canPostWasmModule: boolean | null = null;

function isDataCloneError(err: unknown): boolean {
  const domException = (globalThis as unknown as { DOMException?: unknown }).DOMException;
  if (typeof domException === 'function') {
    if (err instanceof (domException as unknown as Function) && (err as { name?: unknown }).name === 'DataCloneError') return true;
  }
  if (err && typeof err === 'object') {
    const name = (err as { name?: unknown }).name;
    if (name === 'DataCloneError') return true;
  }
  const message = formatOneLineError(err, 2048);
  return /DataCloneError|could not be cloned/i.test(message);
}

// Debug-only sync word used by the JIT smoke test to coordinate a deterministic
// stale-code scenario.
//
// IMPORTANT: Do not use a low linear-memory address for this (it may overlap wasm
// statics/stack/heap). Instead we use a word inside the wasm runtime allocator's
// reserved tail guard (see `crates/aero-wasm/src/runtime_alloc.rs`).
//
// Keep this in sync with `HEAP_TAIL_GUARD_BYTES` (currently 64).
const DEBUG_SYNC_TAIL_GUARD_BYTES = 64;

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
  const effectiveMax = max === 0 ? DEFAULT_MAX_BYTES : max;
  const desiredLen = effectiveMax + DECODE_WINDOW_SLACK_BYTES;

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
  // Ensure the payload is backed by a detached-transferable ArrayBuffer (not SharedArrayBuffer),
  // and ideally owns exactly the byte range being transferred.
  if (bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength) {
    return bytes as Uint8Array<ArrayBuffer>;
  }
  // Fallback: copy into a fresh ArrayBuffer so:
  // - WebAssembly.validate/compile accept it under `ES2024.SharedMemory` libs
  // - We never accidentally transfer/detach a large wasm memory buffer
  // - It is safe to transfer the exact wasm bytes payload.
  return new Uint8Array(bytes) as Uint8Array<ArrayBuffer>;
}

function normalizeTier1Compilation(result: unknown, fallbackCodeByteLen: number): Tier1BlockCompilation {
  const fallbackLen = clampU32(fallbackCodeByteLen);
  // The Tier-1 wasm-bindgen ABI is still evolving. Older builds returned raw `Uint8Array` bytes,
  // while newer ones return a small object with `{ wasm_bytes, code_byte_len, ... }`.
  if (result instanceof Uint8Array) {
    return {
      wasm_bytes: result,
      code_byte_len: fallbackLen,
      exit_to_interpreter: false,
    };
  }

  if (result && typeof result === 'object') {
    const wasmBytes = (result as Partial<Tier1BlockCompilation>).wasm_bytes;
    if (!(wasmBytes instanceof Uint8Array)) {
      throw new Error('JIT compiler returned unexpected result (missing wasm_bytes Uint8Array)');
    }
    const codeByteLenRaw = (result as Partial<Tier1BlockCompilation>).code_byte_len;
    let codeByteLen = fallbackLen;
    if (typeof codeByteLenRaw === 'number' && Number.isFinite(codeByteLenRaw)) {
      codeByteLen = clampU32(codeByteLenRaw);
      if (codeByteLen > fallbackLen) codeByteLen = fallbackLen;
    }
    const exitToInterp =
      typeof (result as Partial<Tier1BlockCompilation>).exit_to_interpreter === 'boolean'
        ? (result as Partial<Tier1BlockCompilation>).exit_to_interpreter!
        : false;
    return { wasm_bytes: wasmBytes, code_byte_len: codeByteLen, exit_to_interpreter: exitToInterp };
  }

  throw new Error('JIT compiler returned unexpected result (expected Uint8Array or { wasm_bytes: Uint8Array, ... })');
}

function isTier1CompileError(err: unknown): boolean {
  // Newer aero-jit-wasm builds prefix their error messages with `compile_tier1_block:`. Use that
  // to distinguish genuine compilation failures from ABI mismatches (wrong arg types/count) when
  // running against older wasm-pack outputs.
  const message = formatOneLineError(err, 2048);
  return message.trimStart().startsWith('compile_tier1_block:');
}

function isTier1AbiMismatchError(err: unknown): boolean {
  // wasm-bindgen argument mismatches typically show up as TypeErrors (wrong BigInt/number types,
  // wrong arg count, etc). Use a best-effort heuristic so we don't accidentally swallow real
  // compiler/runtime errors by retrying with legacy call signatures.
  if (err instanceof TypeError) return true;
  const message = formatOneLineError(err, 2048);
  return /bigint|cannot convert|argument|parameter|is not a function/i.test(message);
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

  if (!Number.isSafeInteger(req.id) || req.id < 0) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: `invalid request id: ${String(req.id)}`,
    });
    return;
  }

  if (req.mode && req.mode !== 'tier1') {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: `unsupported JIT compile mode: ${String(req.mode)}`,
    });
    return;
  }

  if (!Number.isSafeInteger(req.entry_rip) || req.entry_rip < 0 || req.entry_rip > 0xffffffff) {
    postMessageToCpu({
      type: 'CompileError',
      id: req.id,
      entry_rip: req.entry_rip,
      reason: `invalid entry_rip: ${String(req.entry_rip)}`,
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

  let requestedMaxBytes = clampU32(req.max_bytes);
  if (requestedMaxBytes === 0) requestedMaxBytes = DEFAULT_MAX_BYTES;
  // Clamp to keep the decode window (maxBytes + slack) within the compiler's input cap.
  const maxBytes = Math.min(requestedMaxBytes, MAX_COMPILER_CODE_BYTES - DECODE_WINDOW_SLACK_BYTES);
  const maxInsts = 64;
  const bitnessInput = req.bitness;
  const bitness = bitnessInput === 16 || bitnessInput === 32 || bitnessInput === 64 ? bitnessInput : 0;

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
        bitness,
      ) as unknown;
    } catch (err) {
      if (isTier1CompileError(err)) {
        throw err;
      }
      if (!isTier1AbiMismatchError(err)) {
        // Preserve the original error message instead of masking it with a fallback-call failure.
        throw err;
      }
      // Backwards-compat: older JIT wasm builds used simpler argument lists.
      const compileTier1BlockCompat = api.compile_tier1_block as unknown as (...args: any[]) => unknown;
      try {
        result = compileTier1BlockCompat(entryRip, maxBytes) as unknown;
      } catch {
        result = compileTier1BlockCompat(codeBytes, entryRip, maxBytes) as unknown;
      }
    }
    // Older JIT WASM builds returned only the `Uint8Array` wasm bytes and did not expose the
    // decoded block length. Fall back to a conservative bound that never exceeds the provided
    // code slice or configured max.
    compilation = normalizeTier1Compilation(result, Math.min(maxBytes, codeBytes.byteLength));
  } catch (err) {
    const message = formatOneLineError(err, 512);
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

  if (req.debug_sync) {
    if (guestBase < DEBUG_SYNC_TAIL_GUARD_BYTES) {
      throw new Error(
        `debug_sync unavailable: guest_base (${guestBase}) < tail guard bytes (${DEBUG_SYNC_TAIL_GUARD_BYTES})`,
      );
    }
    const debugSyncOffset = guestBase - DEBUG_SYNC_TAIL_GUARD_BYTES;
    if ((debugSyncOffset & 3) !== 0 || debugSyncOffset + 4 > sharedMemory.buffer.byteLength) {
      throw new Error(
        `debug_sync offset out of bounds: offset=${debugSyncOffset} guest_base=${guestBase} mem_bytes=${sharedMemory.buffer.byteLength}`,
      );
    }
    const sync = new Int32Array(sharedMemory.buffer, debugSyncOffset, 1);
    // Signal "ready to respond" by writing the request id. The CPU worker will mutate the guest
    // bytes and then write `-id` to release us.
    Atomics.store(sync, 0, req.id);
    Atomics.notify(sync, 0);
    // Use a generous timeout to avoid wedging the worker if the CPU side crashes.
    const waitResult = Atomics.wait(sync, 0, req.id, 5_000);
    if (waitResult === 'timed-out') {
      console.warn(`[jit-worker] debug_sync timed out waiting for CPU ack (id=${req.id})`);
    }
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
      reason: formatOneLineError(err, 512),
    });
  }
}

ctx.addEventListener('message', (ev: MessageEvent<CpuToJitMessage>) => {
  const msg = ev.data;
  switch (msg.type) {
    case 'JitWorkerInit':
      sharedMemory = msg.memory;
      guestBase = clampU32(msg.guest_base);
      guestSize = clampU32(msg.guest_size);
      // Clamp guestSize to the memory buffer length so mis-sized init payloads cannot create
      // out-of-bounds TypedArray slices.
      //
      // Note: This worker only uses guest_base/guest_size for bounds checks and reads; the main
      // runtime is authoritative.
      {
        const bufLen = sharedMemory.buffer.byteLength;
        if (guestBase >= bufLen) {
          guestSize = 0;
        } else if (guestBase + guestSize > bufLen) {
          guestSize = Math.max(0, bufLen - guestBase);
        }
      }
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
