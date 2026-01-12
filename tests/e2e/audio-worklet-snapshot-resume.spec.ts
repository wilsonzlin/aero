import { expect, test } from "@playwright/test";

import { probeOpfsSyncAccessHandle } from "./util/opfs";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet producer does not burst after worker-VM snapshot restore", async ({ page }) => {
  test.setTimeout(180_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  // Worker VM snapshots require OPFS SyncAccessHandle. Probe early so unsupported browser variants
  // skip without paying the cost of loading the full `/web/` app (which kicks off main-thread WASM init).
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

  // Use the minimum allowed guest RAM (256 MiB) to keep the snapshot file small enough for CI,
  // while still exercising the worker snapshot protocol.
  await page.goto(`${PREVIEW_ORIGIN}/web/?mem=256`, { waitUntil: "load" });

  const workersPanel = page.getByRole("heading", { name: "Workers" }).locator("..");
  const workersSnapshotLine = workersPanel.locator("div.mono").filter({ hasText: /^snapshot:/ });
  const workersError = workersPanel.locator("pre").last();

  await page.click("#init-audio-output-worker");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`web/src/main.ts`).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    return out?.enabled === true && out?.context?.state === "running";
  });

  // Wait for the worker side to start producing so we have a baseline.
  await page.waitForTimeout(500);

  const snapshotSaveButton = page.getByRole("button", { name: "Save snapshot" });
  const snapshotLoadButton = page.getByRole("button", { name: "Load snapshot" });

  // Wait for worker VM snapshots to become available. If OPFS sync access handles aren't supported,
  // the workers panel keeps snapshot disabled; in that case, exit early.
  try {
    await expect(snapshotSaveButton).toBeEnabled({ timeout: 60_000 });
  } catch (err) {
    const snapshotStatus = await workersSnapshotLine.textContent();
    if (snapshotStatus?.includes("unavailable")) {
      test.skip(true, `VM snapshot unavailable in this build (${snapshotStatus}).`);
    }
    throw err;
  }

  const beforeSave = await page.evaluate(() => {
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
  expect(beforeSave).not.toBeNull();
  expect(beforeSave!.overrun).toBe(0);

  await snapshotSaveButton.click();
  await expect
    .poll(async () => (await workersSnapshotLine.textContent()) ?? "", { timeout: 120_000 })
    .toMatch(/snapshot: (saved|save failed)/);
  const saveStatus = (await workersSnapshotLine.textContent()) ?? "";
  if (saveStatus.includes("save failed")) {
    const errorText = (await workersError.textContent()) ?? "";
    if (errorText.toLowerCase().includes("unavailable")) {
      test.skip(true, errorText);
    }
    throw new Error(`Snapshot save failed: ${errorText || saveStatus}`);
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

  await expect(snapshotLoadButton).toBeEnabled({ timeout: 60_000 });
  await snapshotLoadButton.click();
  await expect
    .poll(async () => (await workersSnapshotLine.textContent()) ?? "", { timeout: 120_000 })
    .toMatch(/snapshot: (restored|restore failed)/);
  const restoreStatus = (await workersSnapshotLine.textContent()) ?? "";
  if (restoreStatus.includes("restore failed")) {
    const errorText = (await workersError.textContent()) ?? "";
    if (errorText.toLowerCase().includes("unavailable")) {
      test.skip(true, errorText);
    }
    throw new Error(`Snapshot restore failed: ${errorText || restoreStatus}`);
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

  await page.waitForFunction(
    () => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutputWorker;
      if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return false;
      const samples: Float32Array = out.ringBuffer.samples;
      const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
      const cc = out.ringBuffer.channelCount | 0;
      const cap = out.ringBuffer.capacityFrames | 0;
      if (cc <= 0 || cap <= 0) return false;
      const write = Atomics.load(writeIndex, 0) >>> 0;
      const framesToInspect = Math.min(1024, cap);
      const startFrame = (write - framesToInspect) >>> 0;
      let maxAbs = 0;
      for (let i = 0; i < framesToInspect; i++) {
        const frame = (startFrame + i) % cap;
        const base = frame * cc;
        for (let c = 0; c < cc; c++) {
          const s = samples[base + c] ?? 0;
          const a = Math.abs(s);
          if (a > maxAbs) maxAbs = a;
        }
      }
      return maxAbs > 0.01;
    },
    { timeout: 10_000 },
  );
});
