import test from "node:test";
import assert from "node:assert/strict";

import {
  JIT_BIGINT_ABI_WASM_BYTES,
  JIT_BLOCK_WASM_BYTES,
  JIT_CODE_PAGE_VERSION_ABI_WASM_BYTES,
} from "../src/workers/wasm-bytes.js";

const HAS_SHARED_WASM_MEMORY = (() => {
  try {
    // eslint-disable-next-line no-new
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    return true;
  } catch {
    return false;
  }
})();

test("jit i64 BigInt ABI: wasm fixture imports/returns use BigInt", { skip: !HAS_SHARED_WASM_MEMORY }, async () => {
  const calls = {
    mem_read_u64: 0,
    mem_write_u64: 0,
    mmu_translate: 0,
    page_fault: 0,
    jit_exit_mmio: 0,
    jit_exit: 0,
  };

  // Keep the shared memory max tiny: for shared memories, engines may allocate the full maximum
  // upfront (SharedArrayBuffer is not resizable). The wasm fixture's import type allows a larger
  // maximum, so we can safely pass the minimal 1-page memory here.
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });

  const env = {
    memory,
    mem_read_u8(_cpuPtr, addr) {
      assert.equal(typeof addr, "bigint");
      return 0;
    },
    mem_read_u16(_cpuPtr, addr) {
      assert.equal(typeof addr, "bigint");
      return 0;
    },
    mem_read_u32(_cpuPtr, addr) {
      assert.equal(typeof addr, "bigint");
      return 0;
    },
    mem_read_u64(_cpuPtr, addr) {
      assert.equal(typeof addr, "bigint");
      calls.mem_read_u64 += 1;
      return 0n;
    },
    mem_write_u8(_cpuPtr, addr, value) {
      assert.equal(typeof addr, "bigint");
      assert.equal(typeof value, "number");
    },
    mem_write_u16(_cpuPtr, addr, value) {
      assert.equal(typeof addr, "bigint");
      assert.equal(typeof value, "number");
    },
    mem_write_u32(_cpuPtr, addr, value) {
      assert.equal(typeof addr, "bigint");
      assert.equal(typeof value, "number");
    },
    mem_write_u64(_cpuPtr, addr, value) {
      assert.equal(typeof addr, "bigint");
      assert.equal(typeof value, "bigint");
      calls.mem_write_u64 += 1;
    },
    mmu_translate(_cpuPtr, jitCtxPtr, vaddr, access) {
      assert.equal(typeof jitCtxPtr, "number");
      assert.equal(typeof vaddr, "bigint");
      assert.equal(typeof access, "number");
      calls.mmu_translate += 1;
      return 0n;
    },
    page_fault(_cpuPtr, addr) {
      assert.equal(typeof addr, "bigint");
      calls.page_fault += 1;
      return -1n;
    },
    jit_exit_mmio(_cpuPtr, vaddr, size, isWrite, value, rip) {
      assert.equal(typeof vaddr, "bigint");
      assert.equal(typeof size, "number");
      assert.equal(typeof isWrite, "number");
      assert.equal(typeof value, "bigint");
      assert.equal(typeof rip, "bigint");
      calls.jit_exit_mmio += 1;
      return rip;
    },
    jit_exit(kind, rip) {
      assert.equal(typeof kind, "number");
      assert.equal(typeof rip, "bigint");
      calls.jit_exit += 1;
      return rip;
    },
  };

  const module = await WebAssembly.compile(JIT_BIGINT_ABI_WASM_BYTES);
  const instance = await WebAssembly.instantiate(module, { env });
  const { block } = instance.exports;
  assert.equal(typeof block, "function");

  const ret = block(0, 0);
  assert.equal(typeof ret, "bigint");
  assert.equal(ret, -1n);

  assert.ok(calls.mem_read_u64 > 0, "expected mem_read_u64 to be called");
  assert.ok(calls.mem_write_u64 > 0, "expected mem_write_u64 to be called");
  assert.ok(calls.mmu_translate > 0, "expected mmu_translate to be called");
  assert.ok(calls.page_fault > 0, "expected page_fault to be called");
  assert.ok(calls.jit_exit_mmio > 0, "expected jit_exit_mmio to be called");
  assert.ok(calls.jit_exit > 0, "expected jit_exit to be called");
});

test("jit i64 BigInt ABI: code_page_version import uses BigInt", async () => {
  let seenPage = null;

  const env = {
    code_page_version(_cpuPtr, page) {
      assert.equal(typeof page, "bigint");
      seenPage = page;
      return 0n;
    },
  };

  const module = await WebAssembly.compile(JIT_CODE_PAGE_VERSION_ABI_WASM_BYTES);
  const instance = await WebAssembly.instantiate(module, { env });
  const { block } = instance.exports;
  assert.equal(typeof block, "function");

  const ret = block(0, 0);
  assert.equal(typeof ret, "bigint");
  assert.equal(seenPage, -1n);
});

test("jit i64 BigInt ABI: rollback fixture returns sentinel and uses BigInt params", { skip: !HAS_SHARED_WASM_MEMORY }, async () => {
  let sawWrite = false;
  let sawExit = false;

  // The fixture expects a shared memory with min=1 and some max; use a tiny max to keep the
  // unit test lightweight.
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });

  const env = {
    memory,
    mem_write_u32(_cpuPtr, addr, value) {
      assert.equal(typeof addr, "bigint");
      assert.equal(typeof value, "number");
      sawWrite = true;
    },
    jit_exit(kind, rip) {
      assert.equal(typeof kind, "number");
      assert.equal(typeof rip, "bigint");
      sawExit = true;
      return rip;
    },
  };

  const module = await WebAssembly.compile(JIT_BLOCK_WASM_BYTES);
  const instance = await WebAssembly.instantiate(module, { env });
  const { block } = instance.exports;
  assert.equal(typeof block, "function");

  const ret = block(0, 0);
  assert.equal(ret, -1n);
  assert.ok(sawWrite, "expected mem_write_u32 to be called");
  assert.ok(sawExit, "expected jit_exit to be called");
});
