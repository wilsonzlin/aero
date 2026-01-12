import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("IO-worker HDA PCI audio continues after worker-VM snapshot restore (no burst)", async ({ page }) => {
  // HDA PCI audio exercises the full worker runtime + IO-worker WASM snapshot pipeline (uncached in CI).
  test.setTimeout(240_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  page.setDefaultTimeout(120_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  // Coordinator is exposed by the repo-root harness (`src/main.ts`).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return !!(globalThis as any).__aeroWorkerCoordinator;
  });

  await page.click("#init-audio-hda-pci-device");

  await page.waitForFunction(
    () => {
      // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      return out?.enabled === true && out?.context?.state === "running";
    },
    undefined,
    // Full IO-worker WASM init + PCI enumeration can be slow on cold CI runners.
    { timeout: 120_000 },
  );

  const initialWrite = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { writeIndex: Uint32Array };
    return Atomics.load(ring.writeIndex, 0) >>> 0;
  });
  expect(initialWrite).not.toBeNull();

  // Confirm the IO worker is producing into the ring buffer before snapshotting.
  await page.waitForFunction(
    (initialWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (initialWrite as number)) >>> 0) > 0;
    },
    initialWrite,
    { timeout: 60_000 },
  );

  // Worker VM snapshots require OPFS SyncAccessHandle.
  const snapshotSupport = await page.evaluate(async () => {
    try {
      const storage = navigator.storage as StorageManager & { getDirectory?: () => Promise<FileSystemDirectoryHandle> };
      if (typeof storage?.getDirectory !== "function") {
        return { ok: true, supported: false, reason: "navigator.storage.getDirectory unavailable" };
      }

      const root = await storage.getDirectory();
      // Ensure the snapshot directory exists (WorkerCoordinator writes under `state/` by default).
      try {
        await root.getDirectoryHandle("state", { create: true });
      } catch {
        // ignore best-effort
      }
      const handle = await root.getFileHandle("aero-sync-access-handle-probe.tmp", { create: true });
      return { ok: true, supported: typeof (handle as unknown as { createSyncAccessHandle?: unknown }).createSyncAccessHandle === "function" };
    } catch (err) {
      return { ok: false, supported: false, reason: err instanceof Error ? err.message : String(err) };
    }
  });

  if (!snapshotSupport.ok || !snapshotSupport.supported) {
    test.skip(
      true,
      snapshotSupport.ok
        ? `OPFS SyncAccessHandle unsupported in this browser/context (${snapshotSupport.reason ?? "unknown reason"}).`
        : `Failed to probe OPFS SyncAccessHandle support (${snapshotSupport.reason ?? "unknown error"}).`,
    );
  }

  const beforeSave = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
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
  expect(beforeSave).not.toBeNull();

  // Save snapshot via coordinator (pause → save → resume).
  await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator;
    if (!coord || typeof coord.snapshotSaveToOpfs !== "function") {
      throw new Error("Missing __aeroWorkerCoordinator.snapshotSaveToOpfs()");
    }
    await coord.snapshotSaveToOpfs(path);
  }, "state/worker-vm-hda-pci.snap");

  // Simulate time passing between save and restore (user delay, slow restore, etc.).
  await page.waitForTimeout(1000);

  const beforeRestore = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
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

  // Restore snapshot via coordinator (pause → restore → resume).
  await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const coord = (globalThis as any).__aeroWorkerCoordinator;
    if (!coord || typeof coord.snapshotRestoreFromOpfs !== "function") {
      throw new Error("Missing __aeroWorkerCoordinator.snapshotRestoreFromOpfs()");
    }
    await coord.snapshotRestoreFromOpfs(path);
  }, "state/worker-vm-hda-pci.snap");

  // Give the workers a moment to tick and begin producing again.
  await page.waitForTimeout(250);

  const afterRestore = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      readIndex: Uint32Array;
      writeIndex: Uint32Array;
      underrunCount: Uint32Array;
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      state: out?.context?.state ?? null,
      read: Atomics.load(ring.readIndex, 0) >>> 0,
      write: Atomics.load(ring.writeIndex, 0) >>> 0,
      underrun: Atomics.load(ring.underrunCount, 0) >>> 0,
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(afterRestore).not.toBeNull();
  expect(afterRestore!.state).toBe("running");

  // Confirm the producer resumes after restore (write index advances).
  await page.waitForFunction(
    (baselineWrite) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { writeIndex: Uint32Array };
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      return ((write - (baselineWrite as number)) >>> 0) > 0;
    },
    afterRestore!.write,
    { timeout: 60_000 },
  );

  const afterAdvance = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as {
      overrunCount: Uint32Array;
      capacityFrames: number;
    };
    return {
      overrun: Atomics.load(ring.overrunCount, 0) >>> 0,
      capacity: ring.capacityFrames as number,
    };
  });
  expect(afterAdvance).not.toBeNull();

  const deltaOverrun = ((afterAdvance!.overrun - beforeRestore!.overrun) >>> 0) as number;
  // The producer must not attempt to "catch up" by dumping seconds worth of frames after restore.
  expect(deltaOverrun).toBeLessThanOrEqual(Math.min(128, afterAdvance!.capacity));
});
