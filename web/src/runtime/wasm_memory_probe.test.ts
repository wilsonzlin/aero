import { describe, expect, it } from "vitest";

import { assertWasmMemoryWiring, WasmMemoryWiringError } from "./wasm_memory_probe";

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
});

