import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet producer does not burst after worker-VM snapshot restore", async ({ page }) => {
  test.setTimeout(180_000);
  test.skip(test.info().project.name !== "chromium", "Snapshot + AudioWorklet test only runs on Chromium.");

  // Use the minimum allowed guest RAM (256 MiB) to keep the snapshot file small enough for CI,
  // while still exercising the worker snapshot protocol.
  await page.goto(`${PREVIEW_ORIGIN}/web/?mem=256`, { waitUntil: "load" });

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
    const snapshotStatus = await page.evaluate(() => {
      const nodes = Array.from(document.querySelectorAll(".mono"));
      return nodes.map((n) => n.textContent ?? "").find((t) => t.startsWith("snapshot:")) ?? null;
    });
    if (snapshotStatus?.includes("unavailable")) {
      expect(snapshotStatus).toContain("snapshot: unavailable");
      return;
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
  await page.waitForFunction(() => Array.from(document.querySelectorAll(".mono")).some((n) => (n.textContent ?? "").includes("snapshot: saved")));

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
  await page.waitForFunction(
    () => Array.from(document.querySelectorAll(".mono")).some((n) => (n.textContent ?? "").includes("snapshot: restored")),
    undefined,
    { timeout: 120_000 },
  );

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
});
