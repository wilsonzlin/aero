import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import {
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_B8G8R8A8_SRGB,
  SCANOUT_FORMAT_B8G8R8X8_SRGB,
  SCANOUT_FORMAT_B5G6R5,
  SCANOUT_FORMAT_B5G5R5A1,
  SCANOUT_FORMAT_R8G8B8A8,
  SCANOUT_FORMAT_R8G8B8X8,
  SCANOUT_FORMAT_R8G8B8A8_SRGB,
  SCANOUT_FORMAT_R8G8B8X8_SRGB,
  SCANOUT_SOURCE_WDDM,
  SCANOUT_STATE_GENERATION_BUSY_BIT,
  SCANOUT_STATE_BYTE_LEN,
  SCANOUT_STATE_U32_LEN,
  ScanoutStateIndex,
  publishScanoutState,
  snapshotScanoutState,
  trySnapshotScanoutState,
  wrapScanoutState,
} from "./scanout_state";
import { AerogpuFormat } from "../../../emulator/protocol/aerogpu/aerogpu_pci";
import { makeNodeWorkerExecArgv } from "../test_utils/worker_threads_exec_argv";

const WORKER_EXEC_ARGV = makeNodeWorkerExecArgv();

describe("ipc/scanout_state", () => {
  it("publishScanoutState + snapshotScanoutState roundtrips values", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);

    const update = {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0x89ab_cdef,
      basePaddrHi: 0x0123_4567,
      width: 0x8000_0001,
      height: 0xffff_ffff,
      pitchBytes: 0x8000_0000,
      format: SCANOUT_FORMAT_B8G8R8X8,
    };

    const generation = publishScanoutState(words, update);
    const snap = snapshotScanoutState(words);

    expect((snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
    expect(snap.generation >>> 0).toBe(generation >>> 0);
    expect(snap).toEqual({ generation: generation >>> 0, ...update });
  });

  it("scanout format constants match AerogpuFormat discriminants", () => {
    expect(SCANOUT_FORMAT_B8G8R8X8).toBe(AerogpuFormat.B8G8R8X8Unorm);
    expect(SCANOUT_FORMAT_B8G8R8A8).toBe(AerogpuFormat.B8G8R8A8Unorm);
    expect(SCANOUT_FORMAT_R8G8B8A8).toBe(AerogpuFormat.R8G8B8A8Unorm);
    expect(SCANOUT_FORMAT_R8G8B8X8).toBe(AerogpuFormat.R8G8B8X8Unorm);
    expect(SCANOUT_FORMAT_B5G6R5).toBe(AerogpuFormat.B5G6R5Unorm);
    expect(SCANOUT_FORMAT_B5G5R5A1).toBe(AerogpuFormat.B5G5R5A1Unorm);
    expect(SCANOUT_FORMAT_B8G8R8X8_SRGB).toBe(AerogpuFormat.B8G8R8X8UnormSrgb);
    expect(SCANOUT_FORMAT_B8G8R8A8_SRGB).toBe(AerogpuFormat.B8G8R8A8UnormSrgb);
    expect(SCANOUT_FORMAT_R8G8B8A8_SRGB).toBe(AerogpuFormat.R8G8B8A8UnormSrgb);
    expect(SCANOUT_FORMAT_R8G8B8X8_SRGB).toBe(AerogpuFormat.R8G8B8X8UnormSrgb);
  });

  it("wrapScanoutState validates bounds and 4-byte alignment", () => {
    expect(() => wrapScanoutState(new ArrayBuffer(SCANOUT_STATE_BYTE_LEN) as unknown as SharedArrayBuffer)).toThrow(TypeError);

    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    expect(() => wrapScanoutState(scanoutSab, NaN)).toThrow(RangeError);
    expect(() => wrapScanoutState(scanoutSab, Infinity)).toThrow(RangeError);
    expect(() => wrapScanoutState(scanoutSab, -4)).toThrow(RangeError);
    expect(() => wrapScanoutState(scanoutSab, 2)).toThrow(RangeError);
    // Any positive aligned offset into a minimum-sized SAB would exceed bounds.
    expect(() => wrapScanoutState(scanoutSab, 4)).toThrow(RangeError);

    const tooSmall = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN - 4);
    expect(() => wrapScanoutState(tooSmall, 0)).toThrow(RangeError);

    const withOffset = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN * 2);
    const words = wrapScanoutState(withOffset, SCANOUT_STATE_BYTE_LEN);
    expect(words.length).toBe(SCANOUT_STATE_U32_LEN);
    expect(words.byteOffset).toBe(SCANOUT_STATE_BYTE_LEN);
  });

  it("wrapScanoutState validates size and publish wraps across the busy-bit boundary", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);
    expect(words.length).toBe(SCANOUT_STATE_U32_LEN);

    // Force generation near the busy-bit boundary; publish should wrap without exposing the busy bit.
    Atomics.store(words, ScanoutStateIndex.GENERATION, 0x7fff_fffe);
    const g1 = publishScanoutState(words, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });
    expect(g1 >>> 0).toBe(0x7fff_ffff);
    expect((g1 & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);

    const g2 = publishScanoutState(words, {
      source: SCANOUT_SOURCE_WDDM,
      basePaddrLo: 0,
      basePaddrHi: 0,
      width: 0,
      height: 0,
      pitchBytes: 0,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });
    expect(g2 >>> 0).toBe(0);
    expect((g2 & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
  });

  it("snapshot retries when generation changes mid-read", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);

    const genStart = 123;
    const genNext = (genStart + 1) >>> 0;

    // Initial payload (generation N).
    Atomics.store(words, ScanoutStateIndex.GENERATION, genStart | 0);
    Atomics.store(words, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM | 0);
    Atomics.store(words, ScanoutStateIndex.BASE_PADDR_LO, 11);
    Atomics.store(words, ScanoutStateIndex.BASE_PADDR_HI, 22);
    Atomics.store(words, ScanoutStateIndex.WIDTH, 33);
    Atomics.store(words, ScanoutStateIndex.HEIGHT, 44);
    Atomics.store(words, ScanoutStateIndex.PITCH_BYTES, 55);
    Atomics.store(words, ScanoutStateIndex.FORMAT, SCANOUT_FORMAT_B8G8R8X8 | 0);

    const originalLoad = Atomics.load;
    let generationLoads = 0;
    let didFlipGeneration = false;

    // Force a generation flip right before the first loop's final generation read.
    (Atomics as unknown as { load: typeof Atomics.load }).load = ((arr: Int32Array, idx: number): number => {
      if (arr === words && idx === ScanoutStateIndex.GENERATION) {
        generationLoads += 1;
        if (generationLoads === 2) {
          // Simulate writer publishing a new generation between the two generation reads.
          Atomics.store(words, ScanoutStateIndex.BASE_PADDR_LO, 111);
          Atomics.store(words, ScanoutStateIndex.BASE_PADDR_HI, 222);
          Atomics.store(words, ScanoutStateIndex.WIDTH, 333);
          Atomics.store(words, ScanoutStateIndex.HEIGHT, 444);
          Atomics.store(words, ScanoutStateIndex.PITCH_BYTES, 555);
          Atomics.store(words, ScanoutStateIndex.GENERATION, genNext | 0);
          didFlipGeneration = true;
        }
      }
      return originalLoad(arr, idx);
    }) as typeof Atomics.load;

    try {
      const snap = snapshotScanoutState(words);
      expect(didFlipGeneration).toBe(true);
      // Two generation reads per loop, so a forced retry implies >=4 loads.
      expect(generationLoads).toBe(4);
      expect(snap.generation >>> 0).toBe(genNext);
      expect((snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
      expect(snap.source).toBe(SCANOUT_SOURCE_WDDM);
      expect(snap.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
      expect(snap.basePaddrLo).toBe(111);
      expect(snap.basePaddrHi).toBe(222);
      expect(snap.width).toBe(333);
      expect(snap.height).toBe(444);
      expect(snap.pitchBytes).toBe(555);
    } finally {
      (Atomics as unknown as { load: typeof Atomics.load }).load = originalLoad;
    }
  });

  it("snapshot will not return while the busy bit is set", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);

    const stableGen = 42;
    // Pretend a writer is in progress by setting the busy bit.
    Atomics.store(words, ScanoutStateIndex.GENERATION, (stableGen | SCANOUT_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM | 0);
    Atomics.store(words, ScanoutStateIndex.BASE_PADDR_LO, 1);
    Atomics.store(words, ScanoutStateIndex.BASE_PADDR_HI, 2);
    Atomics.store(words, ScanoutStateIndex.WIDTH, 3);
    Atomics.store(words, ScanoutStateIndex.HEIGHT, 4);
    Atomics.store(words, ScanoutStateIndex.PITCH_BYTES, 5);
    Atomics.store(words, ScanoutStateIndex.FORMAT, SCANOUT_FORMAT_B8G8R8X8 | 0);

    const originalLoad = Atomics.load;
    let generationLoads = 0;

    // After snapshot observes the busy bit once, clear it so snapshot can complete.
    (Atomics as unknown as { load: typeof Atomics.load }).load = ((arr: Int32Array, idx: number): number => {
      const v = originalLoad(arr, idx);
      if (arr === words && idx === ScanoutStateIndex.GENERATION) {
        generationLoads += 1;
        if (generationLoads === 1) {
          // Writer releases lock and publishes the final generation.
          Atomics.store(words, ScanoutStateIndex.GENERATION, stableGen | 0);
        }
      }
      return v;
    }) as typeof Atomics.load;

    try {
      const snap = snapshotScanoutState(words);
      expect(generationLoads).toBe(3);
      expect((snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);
      expect(snap.generation >>> 0).toBe(stableGen);
    } finally {
      (Atomics as unknown as { load: typeof Atomics.load }).load = originalLoad;
    }
  });

  it("publish times out if the busy bit is stuck", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);

    // Simulate a wedged writer holding the lock forever.
    Atomics.store(words, ScanoutStateIndex.GENERATION, (123 | SCANOUT_STATE_GENERATION_BUSY_BIT) | 0);

    // Force the time-based bailout to trigger immediately.
    const originalNow = performance.now;
    let nowCalls = 0;
    (performance as unknown as { now: typeof performance.now }).now = (() => {
      nowCalls += 1;
      return nowCalls === 1 ? 0 : 1000;
    }) as typeof performance.now;

    try {
      expect(() =>
        publishScanoutState(words, {
          source: SCANOUT_SOURCE_WDDM,
          basePaddrLo: 0,
          basePaddrHi: 0,
          width: 0,
          height: 0,
          pitchBytes: 0,
          format: SCANOUT_FORMAT_B8G8R8X8,
        }),
      ).toThrow(/timed out/);
    } finally {
      (performance as unknown as { now: typeof performance.now }).now = originalNow;
    }
  });

  it("trySnapshotScanoutState returns null quickly when the busy bit is stuck", () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_BYTE_LEN);
    const words = wrapScanoutState(scanoutSab, 0);

    // Simulate a wedged writer holding the lock forever.
    Atomics.store(words, ScanoutStateIndex.GENERATION, (123 | SCANOUT_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, ScanoutStateIndex.SOURCE, SCANOUT_SOURCE_WDDM | 0);

    const snap = trySnapshotScanoutState(words, { maxIterations: 16 });
    expect(snap).toBeNull();
  });

  it("snapshot observes coherent state while another worker publishes updates", async () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_U32_LEN * 4);
    const words = new Int32Array(scanoutSab);

    // Control flag: 0 while running, 1 when writer finishes.
    const ctrlSab = new SharedArrayBuffer(4);
    const ctrl = new Int32Array(ctrlSab);

    const scanoutModuleUrl = new URL("./scanout_state.ts", import.meta.url).href;

    const worker = new Worker(
      `
      import { workerData } from "node:worker_threads";
      const mod = await import(workerData.scanoutModuleUrl);
      const words = new Int32Array(workerData.scanoutSab);
      const ctrl = new Int32Array(workerData.ctrlSab);
      for (let token = 0; token < 5000; token += 1) {
        mod.publishScanoutState(words, {
          source: mod.SCANOUT_SOURCE_WDDM,
          basePaddrLo: (token + 3) >>> 0,
          basePaddrHi: (token + 4) >>> 0,
           width: token >>> 0,
           height: (token + 1) >>> 0,
           pitchBytes: (token + 2) >>> 0,
           format: mod.SCANOUT_FORMAT_B8G8R8X8,
         });
       }
      Atomics.store(ctrl, 0, 1);
      Atomics.notify(ctrl, 0);
      `,
      {
        eval: true,
        type: "module",
        workerData: { scanoutSab, ctrlSab, scanoutModuleUrl },
        execArgv: WORKER_EXEC_ARGV,
      } as unknown as WorkerOptions,
    );

    const workerDone = new Promise<void>((resolve, reject) => {
      worker.once("error", (err) => reject(err));
      worker.once("exit", (code) => {
        if (code !== 0) {
          reject(new Error(`scanout writer worker exited with code ${code}`));
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
          throw new Error("timed out waiting for scanout writer worker to finish");
        }

        const snap = snapshotScanoutState(words);
        // Wait for the writer to publish at least one update. The initial shared
        // memory state is all zeros (generation=0), so a fast snapshot can race
        // the first publish and would otherwise fail the invariants below.
        if ((snap.generation >>> 0) === 0) continue;
        expect(snap.source).toBe(SCANOUT_SOURCE_WDDM);
        expect(snap.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
        expect((snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);

        const token = snap.width >>> 0;
        expect(snap.height >>> 0).toBe((token + 1) >>> 0);
        expect(snap.pitchBytes >>> 0).toBe((token + 2) >>> 0);
        expect(snap.basePaddrLo >>> 0).toBe((token + 3) >>> 0);
        expect(snap.basePaddrHi >>> 0).toBe((token + 4) >>> 0);
        snapshotsValidated += 1;
      }

      if (snapshotsValidated === 0) {
        const snap = snapshotScanoutState(words);
        expect(snap.generation >>> 0).not.toBe(0);
        expect(snap.source).toBe(SCANOUT_SOURCE_WDDM);
        expect(snap.format).toBe(SCANOUT_FORMAT_B8G8R8X8);
        expect((snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT) >>> 0).toBe(0);

        const token = snap.width >>> 0;
        expect(snap.height >>> 0).toBe((token + 1) >>> 0);
        expect(snap.pitchBytes >>> 0).toBe((token + 2) >>> 0);
        expect(snap.basePaddrLo >>> 0).toBe((token + 3) >>> 0);
        expect(snap.basePaddrHi >>> 0).toBe((token + 4) >>> 0);
      }
      await workerDone;
    } finally {
      // Ensure we don't leak a background worker if the test fails mid-loop.
      await worker.terminate();
    }
  });
});
