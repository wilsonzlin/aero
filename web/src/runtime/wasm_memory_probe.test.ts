import { describe, expect, it } from "vitest";

import { assertWasmMemoryWiring, computeDefaultWasmMemoryProbeOffset, WasmMemoryWiringError } from "./wasm_memory_probe";

function hashStringFNV1a32(text: string): number {
  // Keep this in sync with the internal helper in `wasm_memory_probe.ts`.
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i) & 0xff;
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

describe("runtime/wasm_memory_probe", () => {
  it("restores the probed word after a successful probe", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);
    const offset = 16;
    dv.setUint32(offset, 0xaabbccdd, true);

    const api = {
      mem_store_u32: (off: number, value: number) => dv.setUint32(off, value >>> 0, true),
      mem_load_u32: (off: number) => dv.getUint32(off, true),
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
    };

    assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "test" });
    expect(dv.getUint32(offset, true)).toBe(0xaabbccdd);
  });

  it("derives a deterministic, context-sensitive default probe offset", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);

    const api = {
      mem_store_u32: (_off: number, _value: number) => {},
      mem_load_u32: (_off: number) => 0,
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0x1000, guest_size: 0, runtime_reserved: 64 }),
    };

    const base = computeDefaultWasmMemoryProbeOffset({ api, memory });
    expect(base).toBe(60);

    const contexts = ["cpu.worker", "io.worker"];
    const offsets: number[] = [];
    for (const context of contexts) {
      const used: number[] = [];
      const probeApi = {
        ...api,
        mem_store_u32: (off: number, value: number) => {
          used.push(off >>> 0);
          dv.setUint32(off, value >>> 0, true);
        },
        mem_load_u32: (off: number) => {
          used.push(off >>> 0);
          return dv.getUint32(off, true);
        },
      };
      assertWasmMemoryWiring({ api: probeApi, memory, context });
      expect(used).toHaveLength(2);

      const spreadWords = 16;
      const delta = (hashStringFNV1a32(context) % spreadWords) * 4;
      const expected = base >= delta ? base - delta : base;
      expect(used[0]).toBe(expected);
      expect(used[1]).toBe(expected);
      offsets.push(expected);
    }

    expect(offsets[0]).not.toBe(offsets[1]);
  });

  it("throws when wasm->JS writes are not observed in the provided memory", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const other = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);
    const dvOther = new DataView(other.buffer);
    const offset = 32;
    dv.setUint32(offset, 0x01020304, true);

    const api = {
      mem_store_u32: (off: number, value: number) => dvOther.setUint32(off, value >>> 0, true),
      mem_load_u32: (off: number) => dvOther.getUint32(off, true),
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
    };

    expect(() => assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "test" })).toThrow(WasmMemoryWiringError);
    // JS-side memory is restored even on failure.
    expect(dv.getUint32(offset, true)).toBe(0x01020304);
  });

  it("throws when JS->wasm writes are not observed by mem_load_u32", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const other = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);
    const dvOther = new DataView(other.buffer);
    const offset = 48;
    dv.setUint32(offset, 0x0badf00d, true);

    const api = {
      mem_store_u32: (off: number, value: number) => dv.setUint32(off, value >>> 0, true),
      mem_load_u32: (off: number) => dvOther.getUint32(off, true),
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
    };

    expect(() => assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "test" })).toThrow(WasmMemoryWiringError);
    expect(dv.getUint32(offset, true)).toBe(0x0badf00d);
  });

  it("wraps mem_store_u32 traps in WasmMemoryWiringError", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);
    const offset = 64;
    dv.setUint32(offset, 0xdeadbeef, true);

    const api = {
      mem_store_u32: (_off: number, _value: number) => {
        throw new WebAssembly.RuntimeError("memory access out of bounds");
      },
      mem_load_u32: (_off: number) => 0,
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
    };

    expect(() => assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "test" })).toThrow(WasmMemoryWiringError);
    expect(dv.getUint32(offset, true)).toBe(0xdeadbeef);
  });

  it("wraps mem_load_u32 traps in WasmMemoryWiringError", () => {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const dv = new DataView(memory.buffer);
    const offset = 128;
    dv.setUint32(offset, 0xcafebabe, true);

    const api = {
      mem_store_u32: (off: number, value: number) => dv.setUint32(off, value >>> 0, true),
      mem_load_u32: (_off: number) => {
        throw new WebAssembly.RuntimeError("memory access out of bounds");
      },
      guest_ram_layout: (_desiredBytes: number) => ({ guest_base: 0, guest_size: 0, runtime_reserved: 0 }),
    };

    expect(() => assertWasmMemoryWiring({ api, memory, linearOffset: offset, context: "test" })).toThrow(WasmMemoryWiringError);
    expect(dv.getUint32(offset, true)).toBe(0xcafebabe);
  });
});
