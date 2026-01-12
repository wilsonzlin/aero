import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("IO-worker HDA PCI audio does not fast-forward after worker snapshot restore", async ({ page }) => {
  test.setTimeout(180_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  // The HDA PCI demo boots multiple workers and may require a cold WASM compile in CI.
  page.setDefaultTimeout(120_000);

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-hda-pci-device");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    return out?.enabled === true && out?.context?.state === "running";
  });

  // Ensure the worker runtime is fully ready before snapshotting (snapshot requires NET too).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    if (!wc || typeof wc.getWorkerStatuses !== "function") return false;
    const statuses = wc.getWorkerStatuses();
    return statuses?.cpu?.state === "ready" && statuses?.io?.state === "ready" && statuses?.net?.state === "ready";
  });

  const snapshotSupported = await page.evaluate(() => {
    const g = globalThis as unknown as { FileSystemFileHandle?: unknown };
    const ctor = g.FileSystemFileHandle as { prototype?: { createSyncAccessHandle?: unknown } } | undefined;
    return typeof ctor?.prototype?.createSyncAccessHandle === "function";
  });
  if (!snapshotSupported) {
    // Some Chromium variants/embeds do not expose OPFS SyncAccessHandle; skip rather than failing.
    return;
  }

  const snapshotPath = `state/playwright-hda-pci-snapshot-${Date.now()}.snap`;

  const saveResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    if (!wc) return { ok: false, error: "Missing __aeroWorkerCoordinator global." };
    try {
      await wc.snapshotSaveToOpfs(path);
      return { ok: true as const };
    } catch (err) {
      return { ok: false as const, error: err instanceof Error ? err.message : String(err) };
    }
  }, snapshotPath);

  if (!saveResult.ok) {
    // Best-effort: tolerate environments where snapshots are compiled out / unavailable.
    expect(saveResult.error).toContain("unavailable");
    return;
  }

  // Simulate time passing between save and restore.
  await page.waitForTimeout(1500);

  const restoreResult = await page.evaluate(async (path) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const wc = (globalThis as any).__aeroWorkerCoordinator;
    if (!wc) return { ok: false, error: "Missing __aeroWorkerCoordinator global." };
    try {
      await wc.snapshotRestoreFromOpfs(path);
      return { ok: true as const };
    } catch (err) {
      return { ok: false as const, error: err instanceof Error ? err.message : String(err) };
    }
  }, snapshotPath);

  if (!restoreResult.ok) {
    throw new Error(`snapshotRestoreFromOpfs failed: ${restoreResult.error}`);
  }

  // Immediately after restore, the IO worker must not treat the paused wall-clock gap as elapsed
  // device time (otherwise the HDA DMA engine will "catch up" and fill the ring buffer in a burst).
  const burst = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
    if (!out?.enabled) return null;

    const sr = typeof out?.context?.sampleRate === "number" ? (out.context.sampleRate as number) : 48_000;
    const level0 = typeof out.getBufferLevelFrames === "function" ? (out.getBufferLevelFrames() as number) : null;
    const write0 = Atomics.load(out.ringBuffer.writeIndex, 0) >>> 0;

    await new Promise((resolve) => setTimeout(resolve, 50));

    const level1 = typeof out.getBufferLevelFrames === "function" ? (out.getBufferLevelFrames() as number) : null;
    const write1 = Atomics.load(out.ringBuffer.writeIndex, 0) >>> 0;

    const maxAllowedIncreaseFrames = Math.ceil(sr / 15); // ~66ms worth of audio (must be < 100ms clamp).
    return {
      sr,
      level0,
      level1,
      levelIncrease: level0 !== null && level1 !== null ? Math.max(0, level1 - level0) : null,
      write0,
      write1,
      writeDelta: ((write1 - write0) >>> 0) as number,
      maxAllowedIncreaseFrames,
    };
  });

  expect(burst).not.toBeNull();
  expect(burst!.level0).not.toBeNull();
  expect(burst!.level1).not.toBeNull();
  expect(burst!.levelIncrease).not.toBeNull();

  // The HDA tick path caps catch-up at 100ms, so a regression would manifest as an immediate
  // ~100ms jump in buffered frames. Ensure we stay well below that.
  expect(burst!.levelIncrease as number).toBeLessThanOrEqual(burst!.maxAllowedIncreaseFrames);

  // Sanity: the device should still be producing audio after restore.
  await page.waitForFunction(
    (write0) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputHdaPciDevice;
      if (!out?.enabled) return false;
      const write = Atomics.load(out.ringBuffer.writeIndex, 0) >>> 0;
      return ((write - (write0 as number)) >>> 0) > 0;
    },
    burst!.write1,
    { timeout: 10_000 },
  );
});
