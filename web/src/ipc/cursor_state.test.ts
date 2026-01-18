import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_FORMAT_B8G8R8X8,
  CURSOR_FORMAT_B8G8R8A8_SRGB,
  CURSOR_FORMAT_B8G8R8X8_SRGB,
  CURSOR_FORMAT_R8G8B8A8,
  CURSOR_FORMAT_R8G8B8X8,
  CURSOR_FORMAT_R8G8B8A8_SRGB,
  CURSOR_FORMAT_R8G8B8X8_SRGB,
  CURSOR_STATE_GENERATION_BUSY_BIT,
  CURSOR_STATE_BYTE_LEN,
  CURSOR_STATE_U32_LEN,
  CursorStateIndex,
  publishCursorState,
  snapshotCursorState,
  trySnapshotCursorState,
  wrapCursorState,
} from "./cursor_state";
import { AerogpuFormat } from "../../../emulator/protocol/aerogpu/aerogpu_pci";
import { makeNodeWorkerExecArgv } from "../test_utils/worker_threads_exec_argv";

const WORKER_EXEC_ARGV = makeNodeWorkerExecArgv();

describe("ipc/cursor_state", () => {
  it("publishCursorState + snapshotCursorState roundtrips values", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    const update = {
      enable: 1,
      x: -123,
      y: 456,
      hotX: 0x89ab_cdef,
      hotY: 0x0123_4567,
      width: 0x8000_0001,
      height: 0xffff_ffff,
      pitchBytes: 0x8000_0000,
      format: CURSOR_FORMAT_B8G8R8A8,
      basePaddrLo: 0x89ab_cdef,
      basePaddrHi: 0x0123_4567,
    };

    const generation = publishCursorState(words, update);
    const snap = snapshotCursorState(words);

    expect((snap.generation & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
    expect(snap.generation >>> 0).toBe(generation >>> 0);
    expect(snap).toEqual({ generation: generation >>> 0, ...update });
  });

  it("cursor format constants match AerogpuFormat discriminants", () => {
    expect(CURSOR_FORMAT_B8G8R8A8).toBe(AerogpuFormat.B8G8R8A8Unorm);
    expect(CURSOR_FORMAT_B8G8R8X8).toBe(AerogpuFormat.B8G8R8X8Unorm);
    expect(CURSOR_FORMAT_R8G8B8A8).toBe(AerogpuFormat.R8G8B8A8Unorm);
    expect(CURSOR_FORMAT_R8G8B8X8).toBe(AerogpuFormat.R8G8B8X8Unorm);
    expect(CURSOR_FORMAT_B8G8R8A8_SRGB).toBe(AerogpuFormat.B8G8R8A8UnormSrgb);
    expect(CURSOR_FORMAT_B8G8R8X8_SRGB).toBe(AerogpuFormat.B8G8R8X8UnormSrgb);
    expect(CURSOR_FORMAT_R8G8B8A8_SRGB).toBe(AerogpuFormat.R8G8B8A8UnormSrgb);
    expect(CURSOR_FORMAT_R8G8B8X8_SRGB).toBe(AerogpuFormat.R8G8B8X8UnormSrgb);
  });

  it("wrapCursorState validates bounds and 4-byte alignment", () => {
    expect(() => wrapCursorState(new ArrayBuffer(CURSOR_STATE_BYTE_LEN) as unknown as SharedArrayBuffer)).toThrow(TypeError);

    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    expect(() => wrapCursorState(cursorSab, NaN)).toThrow(RangeError);
    expect(() => wrapCursorState(cursorSab, Infinity)).toThrow(RangeError);
    expect(() => wrapCursorState(cursorSab, -4)).toThrow(RangeError);
    expect(() => wrapCursorState(cursorSab, 2)).toThrow(RangeError);
    // Any positive aligned offset into a minimum-sized SAB would exceed bounds.
    expect(() => wrapCursorState(cursorSab, 4)).toThrow(RangeError);

    const tooSmall = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN - 4);
    expect(() => wrapCursorState(tooSmall, 0)).toThrow(RangeError);

    const withOffset = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN * 2);
    const words = wrapCursorState(withOffset, CURSOR_STATE_BYTE_LEN);
    expect(words.length).toBe(CURSOR_STATE_U32_LEN);
    expect(words.byteOffset).toBe(CURSOR_STATE_BYTE_LEN);
  });

  it("wrapCursorState validates size and publish wraps across the busy-bit boundary", () => {
    const sab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(sab, 0);
    expect(words.length).toBe(CURSOR_STATE_U32_LEN);

    // Force generation near the busy-bit boundary; publish should wrap without exposing the busy bit.
    Atomics.store(words, CursorStateIndex.GENERATION, 0x7fff_fffe);
    const g1 = publishCursorState(words, {
      enable: 0,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: CURSOR_FORMAT_B8G8R8A8,
      basePaddrLo: 0,
      basePaddrHi: 0,
    });
    expect(g1 >>> 0).toBe(0x7fff_ffff);
    expect((g1 & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);

    const g2 = publishCursorState(words, {
      enable: 0,
      x: 0,
      y: 0,
      hotX: 0,
      hotY: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: CURSOR_FORMAT_B8G8R8A8,
      basePaddrLo: 0,
      basePaddrHi: 0,
    });
    expect(g2 >>> 0).toBe(0);
    expect((g2 & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
  });

  it("snapshot retries when generation changes mid-read", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    const genStart = 123;
    const genNext = (genStart + 1) >>> 0;

    // Initial payload (generation N).
    Atomics.store(words, CursorStateIndex.GENERATION, genStart | 0);
    Atomics.store(words, CursorStateIndex.ENABLE, 1);
    Atomics.store(words, CursorStateIndex.X, 11);
    Atomics.store(words, CursorStateIndex.Y, 22);
    Atomics.store(words, CursorStateIndex.HOT_X, 33);
    Atomics.store(words, CursorStateIndex.HOT_Y, 44);
    Atomics.store(words, CursorStateIndex.WIDTH, 55);
    Atomics.store(words, CursorStateIndex.HEIGHT, 66);
    Atomics.store(words, CursorStateIndex.PITCH_BYTES, 77);
    Atomics.store(words, CursorStateIndex.FORMAT, CURSOR_FORMAT_B8G8R8A8 | 0);
    Atomics.store(words, CursorStateIndex.BASE_PADDR_LO, 88);
    Atomics.store(words, CursorStateIndex.BASE_PADDR_HI, 99);

    const originalLoad = Atomics.load;
    let generationLoads = 0;
    let didFlipGeneration = false;

    // Force a generation flip right before the first loop's final generation read.
    (Atomics as unknown as { load: typeof Atomics.load }).load = ((arr: Int32Array, idx: number): number => {
      if (arr === words && idx === CursorStateIndex.GENERATION) {
        generationLoads += 1;
        if (generationLoads === 2) {
          // Simulate writer publishing a new generation between the two generation reads.
          Atomics.store(words, CursorStateIndex.X, 111);
          Atomics.store(words, CursorStateIndex.Y, 222);
          Atomics.store(words, CursorStateIndex.HOT_X, 333);
          Atomics.store(words, CursorStateIndex.HOT_Y, 444);
          Atomics.store(words, CursorStateIndex.WIDTH, 555);
          Atomics.store(words, CursorStateIndex.HEIGHT, 666);
          Atomics.store(words, CursorStateIndex.PITCH_BYTES, 777);
          Atomics.store(words, CursorStateIndex.BASE_PADDR_LO, 888);
          Atomics.store(words, CursorStateIndex.BASE_PADDR_HI, 999);
          Atomics.store(words, CursorStateIndex.GENERATION, genNext | 0);
          didFlipGeneration = true;
        }
      }
      return originalLoad(arr, idx);
    }) as typeof Atomics.load;

    try {
      const snap = snapshotCursorState(words);
      expect(didFlipGeneration).toBe(true);
      // Two generation reads per loop, so a forced retry implies >=4 loads.
      expect(generationLoads).toBe(4);
      expect(snap.generation >>> 0).toBe(genNext);
      expect((snap.generation & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
      expect(snap.enable >>> 0).toBe(1);
      expect(snap.format >>> 0).toBe(CURSOR_FORMAT_B8G8R8A8);
      expect(snap.x | 0).toBe(111);
      expect(snap.y | 0).toBe(222);
      expect(snap.hotX >>> 0).toBe(333);
      expect(snap.hotY >>> 0).toBe(444);
      expect(snap.width >>> 0).toBe(555);
      expect(snap.height >>> 0).toBe(666);
      expect(snap.pitchBytes >>> 0).toBe(777);
      expect(snap.basePaddrLo >>> 0).toBe(888);
      expect(snap.basePaddrHi >>> 0).toBe(999);
    } finally {
      (Atomics as unknown as { load: typeof Atomics.load }).load = originalLoad;
    }
  });

  it("snapshot will not return while the busy bit is set", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    const stableGen = 42;
    // Pretend a writer is in progress by setting the busy bit.
    Atomics.store(words, CursorStateIndex.GENERATION, (stableGen | CURSOR_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, CursorStateIndex.ENABLE, 1);
    Atomics.store(words, CursorStateIndex.FORMAT, CURSOR_FORMAT_B8G8R8A8 | 0);

    const originalLoad = Atomics.load;
    let generationLoads = 0;

    // After snapshot observes the busy bit once, clear it so snapshot can complete.
    (Atomics as unknown as { load: typeof Atomics.load }).load = ((arr: Int32Array, idx: number): number => {
      const v = originalLoad(arr, idx);
      if (arr === words && idx === CursorStateIndex.GENERATION) {
        generationLoads += 1;
        if (generationLoads === 1) {
          // Writer releases lock and publishes the final generation.
          Atomics.store(words, CursorStateIndex.GENERATION, stableGen | 0);
        }
      }
      return v;
    }) as typeof Atomics.load;

    try {
      const snap = snapshotCursorState(words);
      expect(generationLoads).toBe(3);
      expect((snap.generation & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
      expect(snap.generation >>> 0).toBe(stableGen);
    } finally {
      (Atomics as unknown as { load: typeof Atomics.load }).load = originalLoad;
    }
  });

  it("snapshot times out if the busy bit is stuck", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    const stableGen = 42;
    Atomics.store(words, CursorStateIndex.GENERATION, (stableGen | CURSOR_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, CursorStateIndex.ENABLE, 1);
    Atomics.store(words, CursorStateIndex.FORMAT, CURSOR_FORMAT_B8G8R8A8 | 0);

    // Force the time-based bailout to trigger on the first spin check.
    const originalNow = performance.now;
    let nowCalls = 0;
    (performance as unknown as { now: typeof performance.now }).now = (() => {
      nowCalls += 1;
      return nowCalls === 1 ? 0 : 1000;
    }) as typeof performance.now;

    try {
      expect(() => snapshotCursorState(words)).toThrow(/timed out/);
    } finally {
      (performance as unknown as { now: typeof performance.now }).now = originalNow;
    }
  });

  it("publish times out if the busy bit is stuck", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    // Simulate a wedged writer holding the lock forever.
    Atomics.store(words, CursorStateIndex.GENERATION, (123 | CURSOR_STATE_GENERATION_BUSY_BIT) | 0);

    // Force the time-based bailout to trigger immediately.
    const originalNow = performance.now;
    let nowCalls = 0;
    (performance as unknown as { now: typeof performance.now }).now = (() => {
      nowCalls += 1;
      return nowCalls === 1 ? 0 : 1000;
    }) as typeof performance.now;

    try {
      expect(() =>
        publishCursorState(words, {
          enable: 0,
          x: 0,
          y: 0,
          hotX: 0,
          hotY: 0,
          width: 0,
          height: 0,
          pitchBytes: 0,
          format: CURSOR_FORMAT_B8G8R8A8,
          basePaddrLo: 0,
          basePaddrHi: 0,
        }),
      ).toThrow(/timed out/);
    } finally {
      (performance as unknown as { now: typeof performance.now }).now = originalNow;
    }
  });

  it("trySnapshotCursorState returns null quickly when the busy bit is stuck", () => {
    const cursorSab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(cursorSab, 0);

    // Simulate a wedged writer holding the lock forever.
    Atomics.store(words, CursorStateIndex.GENERATION, (123 | CURSOR_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, CursorStateIndex.ENABLE, 1);

    const snap = trySnapshotCursorState(words, { maxIterations: 16 });
    expect(snap).toBeNull();
  });

  it("snapshot observes coherent state while another worker publishes updates", async () => {
    const sab = new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4);
    const words = new Int32Array(sab);

    // Control flag: 0 while running, 1 when writer finishes.
    const ctrlSab = new SharedArrayBuffer(4);
    const ctrl = new Int32Array(ctrlSab);

    const cursorModuleUrl = new URL("./cursor_state.ts", import.meta.url).href;

    const worker = new Worker(
      `
      import { workerData } from "node:worker_threads";
      const mod = await import(workerData.cursorModuleUrl);
      const words = new Int32Array(workerData.cursorSab);
      const ctrl = new Int32Array(workerData.ctrlSab);
      for (let token = 0; token < 5000; token += 1) {
        mod.publishCursorState(words, {
          enable: 1,
          x: token | 0,
          y: (token + 1) | 0,
          hotX: (token + 2) >>> 0,
          hotY: (token + 3) >>> 0,
          width: (token + 4) >>> 0,
          height: (token + 5) >>> 0,
          pitchBytes: (token + 6) >>> 0,
          format: mod.CURSOR_FORMAT_B8G8R8A8,
          basePaddrLo: (token + 7) >>> 0,
          basePaddrHi: (token + 8) >>> 0,
        });
      }
      Atomics.store(ctrl, 0, 1);
      Atomics.notify(ctrl, 0);
      `,
      {
        eval: true,
        type: "module",
        workerData: { cursorSab: sab, ctrlSab, cursorModuleUrl },
        execArgv: WORKER_EXEC_ARGV,
      } as unknown as WorkerOptions,
    );

    const workerDone = new Promise<void>((resolve, reject) => {
      worker.once("error", (err) => reject(err));
      worker.once("exit", (code) => {
        if (code !== 0) {
          reject(new Error(`cursor writer worker exited with code ${code}`));
          return;
        }
        resolve();
      });
    });

    try {
      const deadlineMs = Date.now() + 10_000;
      let snapshotsValidated = 0;
      while (Atomics.load(ctrl, 0) === 0) {
        if (Date.now() > deadlineMs) {
          throw new Error("timed out waiting for cursor writer worker to finish");
        }

        const snap = snapshotCursorState(words);
        if ((snap.generation >>> 0) === 0) continue;
        expect((snap.generation & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
        expect(snap.enable >>> 0).toBe(1);
        expect(snap.format >>> 0).toBe(CURSOR_FORMAT_B8G8R8A8);

        const token = (snap.width >>> 0) - 4;
        expect(snap.x | 0).toBe(token | 0);
        expect(snap.y | 0).toBe((token + 1) | 0);
        expect(snap.hotX >>> 0).toBe((token + 2) >>> 0);
        expect(snap.hotY >>> 0).toBe((token + 3) >>> 0);
        expect(snap.width >>> 0).toBe((token + 4) >>> 0);
        expect(snap.height >>> 0).toBe((token + 5) >>> 0);
        expect(snap.pitchBytes >>> 0).toBe((token + 6) >>> 0);
        expect(snap.basePaddrLo >>> 0).toBe((token + 7) >>> 0);
        expect(snap.basePaddrHi >>> 0).toBe((token + 8) >>> 0);
        snapshotsValidated += 1;
      }

      if (snapshotsValidated === 0) {
        const snap = snapshotCursorState(words);
        expect(snap.generation >>> 0).not.toBe(0);
        expect((snap.generation & CURSOR_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
        expect(snap.enable >>> 0).toBe(1);
        expect(snap.format >>> 0).toBe(CURSOR_FORMAT_B8G8R8A8);

        const token = (snap.width >>> 0) - 4;
        expect(snap.x | 0).toBe(token | 0);
        expect(snap.y | 0).toBe((token + 1) | 0);
        expect(snap.hotX >>> 0).toBe((token + 2) >>> 0);
        expect(snap.hotY >>> 0).toBe((token + 3) >>> 0);
        expect(snap.width >>> 0).toBe((token + 4) >>> 0);
        expect(snap.height >>> 0).toBe((token + 5) >>> 0);
        expect(snap.pitchBytes >>> 0).toBe((token + 6) >>> 0);
        expect(snap.basePaddrLo >>> 0).toBe((token + 7) >>> 0);
        expect(snap.basePaddrHi >>> 0).toBe((token + 8) >>> 0);
      }

      await workerDone;
    } finally {
      // Ensure we don't leak a background worker if the test fails mid-loop.
      await worker.terminate();
    }
  });
});
