import type { WasmApi } from "./wasm_loader";

export class WasmMemoryWiringError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WasmMemoryWiringError";
  }
}

function hex32(value: number): string {
  return `0x${(value >>> 0).toString(16).padStart(8, "0")}`;
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
  // overlap live Rust/WASM runtime allocations (heap is bounded away from guest RAM by the wasm-side
  // runtime allocator).
  const layout = opts.api.guest_ram_layout(0);
  const runtimeReserved =
    (typeof layout.runtime_reserved === "number" ? layout.runtime_reserved : layout.guest_base) >>> 0;

  const probeEnd = Math.min(runtimeReserved, memBytes);
  if (probeEnd < 4) {
    throw new WasmMemoryWiringError(`WASM memory probe offset is out of bounds (memBytes=${memBytes}, runtimeReserved=${runtimeReserved}).`);
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
  const linearOffset =
    opts.linearOffset ??
    (() => {
      const base = computeDefaultWasmMemoryProbeOffset({ api, memory });
      // Avoid cross-context races when multiple workers probe the same shared memory
      // concurrently by spreading the default probe offset across a small window.
      //
      // Callers can always pass an explicit `linearOffset` to probe a known guest RAM
      // address, but making the default context-sensitive helps keep ad-hoc probes
      // deterministic while reducing flakiness.
      const spreadWords = 16; // 64 bytes window
      const delta = (hashStringFNV1a32(context) % spreadWords) * 4;
      return base >= delta ? base - delta : base;
    })();

  if (!Number.isSafeInteger(linearOffset) || linearOffset < 0 || linearOffset + 4 > memBytes) {
    throw new WasmMemoryWiringError(
      `[${context}] WASM memory probe offset out of bounds: offset=${linearOffset} memBytes=${memBytes}`,
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
    api.mem_store_u32(linearOffset, wasmWrite);
    const gotFromJs = readU32LE(u8, 0);
    if (gotFromJs !== (wasmWrite >>> 0)) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (wasm -> JS).`,
          `mem_store_u32(offset=${hex32(linearOffset)}) wrote ${hex32(wasmWrite)} but JS read ${hex32(gotFromJs)} from the provided WebAssembly.Memory.buffer.`,
          "",
          "This usually means the worker instantiated the WASM module with a different WebAssembly.Memory than the coordinator-provided guest memory.",
          "Ensure the worker passes the coordinator-provided WebAssembly.Memory to initWasmForContext/initWasm and that the WASM build imports memory.",
        ].join("\n"),
      );
    }

    // Direction 2: JS -> wasm (JS writes, mem_load_u32 reads).
    const jsWrite = 0x55667788;
    writeU32LE(u8, 0, jsWrite);
    const gotFromWasm = api.mem_load_u32(linearOffset) >>> 0;
    if (gotFromWasm !== (jsWrite >>> 0)) {
      throw new WasmMemoryWiringError(
        [
          `[${context}] WASM memory wiring probe failed (JS -> wasm).`,
          `JS wrote ${hex32(jsWrite)} at offset=${hex32(linearOffset)} but mem_load_u32 read ${hex32(gotFromWasm)}.`,
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
