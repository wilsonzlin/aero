import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import {
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_SOURCE_WDDM,
  SCANOUT_STATE_GENERATION_BUSY_BIT,
  SCANOUT_STATE_BYTE_LEN,
  SCANOUT_STATE_U32_LEN,
  ScanoutStateIndex,
  publishScanoutState,
  snapshotScanoutState,
  wrapScanoutState,
} from "./scanout_state";

describe("ipc/scanout_state", () => {
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

  it("snapshot observes coherent state while another worker publishes updates", async () => {
    const scanoutSab = new SharedArrayBuffer(SCANOUT_STATE_U32_LEN * 4);
    const words = new Int32Array(scanoutSab);

    // Control flag: 0 while running, 1 when writer finishes.
    const ctrlSab = new SharedArrayBuffer(4);
    const ctrl = new Int32Array(ctrlSab);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
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
        execArgv: ["--experimental-strip-types", "--import", registerUrl.href],
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
