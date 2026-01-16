import type { WasmApi } from "./wasm_loader";
import { formatOneLineError, formatOneLineUtf8 } from "../text";

export class WasmMemoryWiringError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WasmMemoryWiringError";
  }
}

function hex32(value: number): string {
  return `0x${(value >>> 0).toString(16).padStart(8, "0")}`;
}

function describeError(err: unknown): string {
  const message = formatOneLineError(err, 512);
  let rawName: string | undefined;
  if (err && typeof err === "object") {
    try {
      const n = (err as { name?: unknown }).name;
      if (typeof n === "string") rawName = n;
    } catch {
      // ignore getters throwing
    }
  }
  const name = rawName ? (formatOneLineUtf8(rawName, 128) || "Error") : "Error";
  return name && message ? `${name}: ${message}` : (message || name || "Error");
}

function describeMemory(memory: WebAssembly.Memory): string {
  const bytes = memory.buffer.byteLength;
  const shared =
    typeof SharedArrayBuffer !== "undefined" && (memory.buffer as unknown as ArrayBufferLike) instanceof SharedArrayBuffer;
  return `${bytes} bytes (${shared ? "shared" : "unshared"})`;
}

function readU32LE(u8: Uint8Array, offset: number): number {
  // Manual little-endian load to avoid relying on host endianness.
  const b0 = u8[offset] ?? 0;
  const b1 = u8[offset + 1] ?? 0;
  const b2 = u8[offset + 2] ?? 0;
  const b3 = u8[offset + 3] ?? 0;
  return (b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)) >>> 0;
}

function writeU32LE(u8: Uint8Array, offset: number, value: number): void {
  const v = value >>> 0;
  u8[offset] = v & 0xff;
  u8[offset + 1] = (v >>> 8) & 0xff;
  u8[offset + 2] = (v >>> 16) & 0xff;
  u8[offset + 3] = (v >>> 24) & 0xff;
}

function hashStringFNV1a32(text: string): number {
  // 32-bit FNV-1a.
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i) & 0xff;
    // hash *= 16777619 (with 32-bit overflow)
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

export function computeDefaultWasmMemoryProbeOffset(opts: {
  api: Pick<WasmApi, "guest_ram_layout">;
  memory: WebAssembly.Memory;
}): number {
  const memBytes = opts.memory.buffer.byteLength;

  // Probe immediately below the guest RAM base so we don't mutate guest state. In the normal
  // worker configuration this is within the runtime-reserved region and is extremely unlikely to
  // overlap live Rust/WASM runtime allocations.
  //
  // Note: the wasm-side runtime allocator (`crates/aero-wasm/src/runtime_alloc.rs`) leaves a small
  // tail guard at the end of the runtime heap specifically so this probe can use a deterministic
  // word immediately below guest RAM without overlapping real allocations.
  const layout = opts.api.guest_ram_layout(0);
  const runtimeReserved =
    (typeof layout.runtime_reserved === "number" ? layout.runtime_reserved : layout.guest_base) >>> 0;

  const probeEnd = Math.min(runtimeReserved, memBytes);
  if (probeEnd < 4) {
    throw new WasmMemoryWiringError(
      `WASM memory probe offset is out of bounds (memBytes=${memBytes}, runtimeReserved=${runtimeReserved}, memory=${describeMemory(opts.memory)}).`,
    );
  }

  return (probeEnd - 4) >>> 0;
}

export function assertWasmMemoryWiring(opts: {
  api: Pick<WasmApi, "mem_store_u32" | "mem_load_u32" | "guest_ram_layout">;
  memory: WebAssembly.Memory;
  /**
   * Byte offset into wasm linear memory to probe.
   *
   * When omitted, we probe the last 4 bytes of the runtime-reserved region (immediately below
   * guest RAM), so we don't risk dirtying guest memory.
   */
  linearOffset?: number;
  /**
   * Human-readable label used in error messages (e.g. "io.worker").
   */
  context: string;
}): void {
  const { api, memory, context } = opts;

  const memBytes = memory.buffer.byteLength;
  const memDesc = describeMemory(memory);
  let linearOffset: number;
  if (typeof opts.linearOffset === "number") {
    linearOffset = opts.linearOffset;
  } else {
    try {
      const base = computeDefaultWasmMemoryProbeOffset({ api, memory });
      // Avoid cross-context races when multiple workers probe the same shared memory
      // concurrently by spreading the default probe offset across a small window.
      //
      // Callers can always pass an explicit `linearOffset` to probe a known guest RAM
      // address, but making the default context-sensitive helps keep ad-hoc probes
      // deterministic while reducing flakiness.
      // IMPORTANT: this must be <= the tail guard reserved by the wasm-side runtime allocator.
      // See `crates/aero-wasm/src/runtime_alloc.rs` (`HEAP_TAIL_GUARD_BYTES`).
      const spreadWords = 16; // 16 * 4 = 64 bytes window
      const delta = (hashStringFNV1a32(context) % spreadWords) * 4;
      linearOffset = base >= delta ? base - delta : base;
    } catch (err) {
      const msg = formatOneLineError(err, 512);
      throw err instanceof WasmMemoryWiringError
        ? new WasmMemoryWiringError(`[${context}] ${msg}`)
        : new WasmMemoryWiringError(
            [
              `[${context}] Failed to compute WASM memory probe offset.`,
              `memory=${memDesc}`,
              "",
              `error: ${describeError(err)}`,
              "",
              "This usually means the WASM build is missing required exports (e.g. guest_ram_layout) or the worker is running an out-of-date wasm-pack output.",
              "Rebuild the WASM package and ensure the worker is loading the updated module.",
            ].join("\n"),
          );
    }
  }

  if (!Number.isSafeInteger(linearOffset) || linearOffset < 0 || linearOffset + 4 > memBytes) {
    throw new WasmMemoryWiringError(
      `[${context}] WASM memory probe offset out of bounds: offset=${linearOffset} memBytes=${memBytes} memory=${memDesc}`,
    );
  }

  // Use a tiny view rather than `new Uint8Array(memory.buffer)` to avoid creating a 4GiB typed array
  // when the wasm linear memory is at the wasm32 max (65536 pages). Some runtimes reject typed
  // arrays with length === 2^32 even though the underlying WebAssembly.Memory can be 4GiB.
  const u8 = new Uint8Array(memory.buffer, linearOffset, 4);
  const prev = readU32LE(u8, 0);

  // Always try to restore the original value so the probe is side-effect-free when it succeeds.
  try {
    // Direction 1: wasm -> JS (mem_store_u32 writes, JS reads).
    const wasmWrite = 0x11223344;
    try {
      api.mem_store_u32(linearOffset, wasmWrite);
    } catch (err) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (mem_store_u32 threw).`,
          `mem_store_u32(offset=${hex32(linearOffset)}) threw while writing ${hex32(wasmWrite)}.`,
          `memory=${memDesc}`,
          "",
          `error: ${describeError(err)}`,
          "",
          "This can happen if the worker instantiated the WASM module with a different (usually smaller) WebAssembly.Memory than the coordinator-provided guest memory.",
          "Ensure the worker passes the coordinator-provided WebAssembly.Memory to initWasmForContext/initWasm and that the WASM build imports memory.",
        ].join("\n"),
      );
    }
    const gotFromJs = readU32LE(u8, 0);
    if (gotFromJs !== (wasmWrite >>> 0)) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (wasm -> JS).`,
          `mem_store_u32(offset=${hex32(linearOffset)}) wrote ${hex32(wasmWrite)} but JS read ${hex32(gotFromJs)} from the provided WebAssembly.Memory.buffer.`,
          `memory=${memDesc}`,
          "",
          "This usually means the worker instantiated the WASM module with a different WebAssembly.Memory than the coordinator-provided guest memory.",
          "Ensure the worker passes the coordinator-provided WebAssembly.Memory to initWasmForContext/initWasm and that the WASM build imports memory.",
        ].join("\n"),
      );
    }

    // Direction 2: JS -> wasm (JS writes, mem_load_u32 reads).
    const jsWrite = 0x55667788;
    writeU32LE(u8, 0, jsWrite);
    let gotFromWasm: number;
    try {
      gotFromWasm = api.mem_load_u32(linearOffset) >>> 0;
    } catch (err) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (mem_load_u32 threw).`,
          `mem_load_u32(offset=${hex32(linearOffset)}) threw after JS wrote ${hex32(jsWrite)}.`,
          `memory=${memDesc}`,
          "",
          `error: ${describeError(err)}`,
          "",
          "This can happen if the worker instantiated the WASM module with a different WebAssembly.Memory than the coordinator-provided guest memory.",
          "Ensure the worker passes the coordinator-provided WebAssembly.Memory to initWasmForContext/initWasm and that the WASM build imports memory.",
        ].join("\n"),
      );
    }
    if (gotFromWasm !== (jsWrite >>> 0)) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (JS -> wasm).`,
          `JS wrote ${hex32(jsWrite)} at offset=${hex32(linearOffset)} but mem_load_u32 read ${hex32(gotFromWasm)}.`,
          `memory=${memDesc}`,
          "",
          "This usually means the worker instantiated the WASM module with a different WebAssembly.Memory than the coordinator-provided guest memory.",
          "Ensure the worker passes the coordinator-provided WebAssembly.Memory to initWasmForContext/initWasm and that the WASM build imports memory.",
        ].join("\n"),
      );
    }
  } finally {
    try {
      writeU32LE(u8, 0, prev);
    } catch {
      // ignore
    }
  }
}
