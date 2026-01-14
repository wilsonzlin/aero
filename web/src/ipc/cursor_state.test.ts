import { describe, expect, it } from "vitest";

import { Worker, type WorkerOptions } from "node:worker_threads";

import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_FORMAT_B8G8R8X8,
  CURSOR_FORMAT_R8G8B8A8,
  CURSOR_FORMAT_R8G8B8X8,
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

describe("ipc/cursor_state", () => {
  it("cursor format constants match AerogpuFormat discriminants", () => {
    expect(CURSOR_FORMAT_B8G8R8A8).toBe(AerogpuFormat.B8G8R8A8Unorm);
    expect(CURSOR_FORMAT_B8G8R8X8).toBe(AerogpuFormat.B8G8R8X8Unorm);
    expect(CURSOR_FORMAT_R8G8B8A8).toBe(AerogpuFormat.R8G8B8A8Unorm);
    expect(CURSOR_FORMAT_R8G8B8X8).toBe(AerogpuFormat.R8G8B8X8Unorm);
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

  it("snapshot observes coherent state while another worker publishes updates", async () => {
    const sab = new SharedArrayBuffer(CURSOR_STATE_U32_LEN * 4);
    const words = new Int32Array(sab);

    // Control flag: 0 while running, 1 when writer finishes.
    const ctrlSab = new SharedArrayBuffer(4);
    const ctrl = new Int32Array(ctrlSab);

    const registerUrl = new URL("../../../scripts/register-ts-strip-loader.mjs", import.meta.url);
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
        execArgv: ["--experimental-strip-types", "--import", registerUrl.href],
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

  it("trySnapshotCursorState returns null quickly when the busy bit is stuck", () => {
    const sab = new SharedArrayBuffer(CURSOR_STATE_BYTE_LEN);
    const words = wrapCursorState(sab, 0);

    // Simulate a wedged writer holding the lock forever.
    Atomics.store(words, CursorStateIndex.GENERATION, (999 | CURSOR_STATE_GENERATION_BUSY_BIT) | 0);
    Atomics.store(words, CursorStateIndex.ENABLE, 1);

    const snap = trySnapshotCursorState(words, { maxIterations: 16 });
    expect(snap).toBeNull();
  });
});
