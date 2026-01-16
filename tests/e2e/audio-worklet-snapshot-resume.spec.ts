import { expect, test } from "@playwright/test";

import { waitForAudioOutputNonSilent } from "./util/audio";
import { probeOpfsSyncAccessHandle, removeOpfsEntryBestEffort } from "./util/opfs";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet producer does not burst after worker-VM snapshot restore", async ({ page }) => {
  test.setTimeout(180_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  page.setDefaultTimeout(120_000);

  // Worker VM snapshots require OPFS SyncAccessHandle. Probe early so unsupported browser variants
  // skip without paying the cost of booting workers + AudioWorklet graphs.
  // We only need an origin context for the OPFS capability probe; avoid waiting for full page load.
  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "domcontentloaded" });
  const snapshotSupport = await probeOpfsSyncAccessHandle(page);

  if (!snapshotSupport.ok || !snapshotSupport.supported) {
    test.skip(
      true,
      snapshotSupport.ok
        ? `OPFS SyncAccessHandle unsupported in this browser/context (${snapshotSupport.reason ?? "unknown reason"}).`
        : `Failed to probe OPFS SyncAccessHandle support (${snapshotSupport.reason ?? "unknown error"}).`,
    );
  }

  // Coordinator is exposed by the repo-root harness (`src/main.ts`).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return !!(globalThis as any).__aeroWorkerCoordinator;
  });

  await page.click("#init-audio-output-worker");

  await page.waitForFunction(() => {
    // Exposed by the repo-root harness audio panel (`src/main.ts`).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    return out?.enabled === true && out?.context?.state === "running";
  });

  // Ensure the worker runtime is fully ready before snapshotting.
  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const wc = (globalThis as any).__aeroWorkerCoordinator;
      if (!wc || typeof wc.getWorkerStatuses !== "function") return false;
      const statuses = wc.getWorkerStatuses();
      return statuses?.cpu?.state === "ready" && statuses?.io?.state === "ready" && statuses?.net?.state === "ready";
    },
    undefined,
    { timeout: 120_000 },
  );

  // Worker statuses can flip to READY before the background WASM initialization finishes.
  // Wait for WASM_READY from the CPU + IO workers so snapshot save/restore APIs are available.
  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const wc = (globalThis as any).__aeroWorkerCoordinator;
      if (!wc || typeof wc.getWorkerWasmStatus !== "function") return false;
      return Boolean(wc.getWorkerWasmStatus("cpu")) && Boolean(wc.getWorkerWasmStatus("io"));
    },
    undefined,
    { timeout: 120_000 },
  );

  const beforeSnapshot = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
      underrunCount: Uint32Array;
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(beforeSnapshot).not.toBeNull();

  // Ensure the AudioWorklet consumer is alive (read index advances).
  await page.waitForFunction(
    (baselineRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputWorker;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (baselineRead as number)) >>> 0) > 0;
    },
    beforeSnapshot!.read,
    { timeout: 20_000 },
  );

  // Ensure the worker-side producer is alive (write index advances).
  await page.waitForFunction(
    (baselineWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputWorker;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (baselineWrite as number)) >>> 0) > 0;
    },
    beforeSnapshot!.write,
    { timeout: 60_000 },
  );

  // Ensure the producer is writing actual (non-silent) samples into the ring.
  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputWorker", { threshold: 0.01, timeoutMs: 20_000 });

  // Ignore any startup underruns while the worker/runtime bootstraps; assert on the *delta*
  // over a steady-state window so this stays robust on cold CI runners.
  const steady0 = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { underrunCount: Uint32Array; overrunCount: Uint32Array };
    return {
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
    };
  });
  expect(steady0).not.toBeNull();

  // Let the system run for a bit so we catch sustained underruns/overruns (not just “it started once”).
  await page.waitForTimeout(1000);

  const beforeSave = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { underrunCount: Uint32Array; overrunCount: Uint32Array; capacityFrames: number };
    return {
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(beforeSave).not.toBeNull();
  const deltaOverrunBeforeSave = ((beforeSave!.overrun - steady0!.overrun) >>> 0) as number;
  const deltaUnderrunBeforeSave = ((beforeSave!.underrun - steady0!.underrun) >>> 0) as number;
  expect(deltaOverrunBeforeSave).toBe(0);
  // Underruns are tracked in frames. Allow a few render quanta of slack over the window
  // (covers occasional scheduling jitter while still catching sustained underruns).
  expect(deltaUnderrunBeforeSave).toBeLessThanOrEqual(1024);

  const snapshotPath = `state/playwright-worker-tone-snapshot-${Date.now()}-${Math.random().toString(16).slice(2)}.snap`;

  const saveResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator;
    if (!coord || typeof coord.snapshotSaveToOpfs !== "function") {
      return { ok: false as const, error: "Missing __aeroWorkerCoordinator.snapshotSaveToOpfs()" };
    }
    try {
      await coord.snapshotSaveToOpfs(path);
      return { ok: true as const };
    } catch (err) {
      const msg = err instanceof Error ? err.message : err;
      const error = String(msg ?? "Error")
        .replace(/[\\x00-\\x1F\\x7F]/g, " ")
        .replace(/\\s+/g, " ")
        .trim()
        .slice(0, 512);
      return { ok: false as const, error };
    }
  }, snapshotPath);

  if (!saveResult.ok) {
    if (typeof saveResult.error === "string" && saveResult.error.toLowerCase().includes("unavailable")) {
      test.skip(true, `VM snapshot save unavailable in this build (${saveResult.error}).`);
    }
    throw new Error(`snapshot save failed: ${String(saveResult.error)}`);
  }

  // Simulate time passing between save and restore (e.g. user waiting, slow restore, etc.).
  // This only needs to be long enough to expose any post-resume "catch up" behaviour; keep it
  // modest to reduce CI time.
  await page.waitForTimeout(500);

  const beforeRestore = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
      underrunCount: Uint32Array;
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(beforeRestore).not.toBeNull();

  const restoreResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator;
    if (!coord || typeof coord.snapshotRestoreFromOpfs !== "function") {
      return { ok: false as const, error: "Missing __aeroWorkerCoordinator.snapshotRestoreFromOpfs()" };
    }
    try {
      await coord.snapshotRestoreFromOpfs(path);
      return { ok: true as const };
    } catch (err) {
      const msg = err instanceof Error ? err.message : err;
      const error = String(msg ?? "Error")
        .replace(/[\\x00-\\x1F\\x7F]/g, " ")
        .replace(/\\s+/g, " ")
        .trim()
        .slice(0, 512);
      return { ok: false as const, error };
    }
  }, snapshotPath);

  if (!restoreResult.ok) {
    if (typeof restoreResult.error === "string" && restoreResult.error.toLowerCase().includes("unavailable")) {
      test.skip(true, `VM snapshot restore unavailable in this build (${restoreResult.error}).`);
    }
    throw new Error(`snapshot restore failed: ${String(restoreResult.error)}`);
  }

  // Give the CPU worker a moment to tick after resume.
  await page.waitForTimeout(250);

  const afterRestore = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
      underrunCount: Uint32Array;
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(afterRestore).not.toBeNull();

  const deltaOverrun = ((afterRestore!.overrun - beforeRestore!.overrun) >>> 0) as number;

  // The producer should not attempt to "catch up" by writing seconds worth of frames immediately after restore.
  // Allow a small amount of slop (one render quantum worth of frames) for scheduling jitter.
  expect(deltaOverrun).toBeLessThanOrEqual(Math.min(128, beforeRestore!.capacity));

  // Ensure the producer resumes after restore (write index advances) and continues writing real (non-silent) audio.
  await page.waitForFunction(
    (baselineWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputWorker;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (baselineWrite as number)) >>> 0) > 0;
    },
    afterRestore!.write,
    { timeout: 60_000 },
  );

  // Ensure the AudioWorklet resumes consuming frames after restore (read index advances).
  await page.waitForFunction(
    (baselineRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputWorker;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { readIndex: Uint32Array };
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (baselineRead as number)) >>> 0) > 0;
    },
    afterRestore!.read,
    { timeout: 20_000 },
  );

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputWorker", { threshold: 0.01 });

  await removeOpfsEntryBestEffort(page, snapshotPath);
});
