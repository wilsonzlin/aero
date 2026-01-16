import { expect, test } from "@playwright/test";

import { getAudioOutputMaxAbsSample, waitForAudioOutputNonSilent } from "./util/audio";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioWorklet output runs and does not underrun with CPU-worker tone producer", async ({ page }) => {
  test.skip(test.info().project.name !== "chromium", "AudioWorklet output test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

  await page.click("#init-audio-output-worker");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    return out?.enabled === true && out?.context?.state === "running";
  });

  await waitForAudioOutputNonSilent(page, "__aeroAudioOutputWorker", { threshold: 0.01 });

  // Ignore any startup underruns while the worker runtime + AudioWorklet graph bootstraps.
  // Warm up briefly before taking the steady-state baseline so we don't count initial catch-up.
  await page.waitForTimeout(1000);

  const steady0 = await page.evaluate(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    return {
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });
  expect(steady0).not.toBeNull();

  await page.waitForTimeout(1000);

  const result = await page.evaluate(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutputWorker;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const backend = (globalThis as any).__aeroAudioToneBackendWorker;
    return {
      enabled: out?.enabled,
      state: out?.context?.state,
      backend,
      underruns: typeof out?.getUnderrunCount === "function" ? out.getUnderrunCount() : null,
      overruns: typeof out?.getOverrunCount === "function" ? out.getOverrunCount() : null,
    };
  });

  const maxAbs = await getAudioOutputMaxAbsSample(page, "__aeroAudioOutputWorker");

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("cpu-worker-wasm");
  const deltaUnderrun = (((result.underruns as number) - (steady0!.underruns as number)) >>> 0) as number;
  const deltaOverrun = (((result.overruns as number) - (steady0!.overruns as number)) >>> 0) as number;
  // Underruns are tracked in *frames* (not “events”). Allow a few render quanta of slack over the window
  // to avoid cold-start flakes, while still catching sustained underruns.
  expect(deltaUnderrun).toBeLessThanOrEqual(1024);
  expect(deltaOverrun).toBe(0);
  expect(maxAbs).not.toBeNull();
  expect(maxAbs as number).toBeGreaterThan(0.01);

  // Sanity check that the window.aero.netTrace backend is installed and can
  // fetch a (possibly empty) PCAPNG once the worker runtime is running.
  const netTrace = await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const aero = (globalThis as any).aero;
    const backend = aero?.netTrace;
    if (!backend || typeof backend.downloadPcapng !== "function") {
      return { ok: false, error: "missing backend" };
    }
    try {
      const beforeEnabled = typeof backend.isEnabled === "function" ? backend.isEnabled() : null;
      if (typeof backend.enable === "function") backend.enable();
      const afterEnable = typeof backend.isEnabled === "function" ? backend.isEnabled() : null;
      if (typeof backend.disable === "function") backend.disable();
      const afterDisable = typeof backend.isEnabled === "function" ? backend.isEnabled() : null;

      const snapshotBytes = typeof backend.exportPcapng === "function" ? await backend.exportPcapng() : null;
      const bytes = await backend.downloadPcapng();
      return {
        ok: true,
        beforeEnabled,
        afterEnable,
        afterDisable,
        snapshotByteLength: snapshotBytes ? snapshotBytes.byteLength : null,
        snapshotHead: snapshotBytes ? Array.from(snapshotBytes.slice(0, 4)) : null,
        byteLength: bytes.byteLength,
        head: Array.from(bytes.slice(0, 4)),
      };
    } catch (err) {
      const msg = err instanceof Error ? err.message : err;
      const error = String(msg ?? "Error")
        .replace(/[\\x00-\\x1F\\x7F]/g, " ")
        .replace(/\\s+/g, " ")
        .trim()
        .slice(0, 512);
      return { ok: false, error };
    }
  });

  expect(netTrace.ok).toBe(true);
  if (netTrace.ok) {
    expect(netTrace.afterEnable).toBe(true);
    expect(netTrace.afterDisable).toBe(false);
    expect(netTrace.snapshotByteLength).toBeGreaterThan(0);
    expect(netTrace.snapshotHead).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);
    expect(netTrace.byteLength).toBeGreaterThan(0);
    // PCAPNG section header block magic.
    expect(netTrace.head).toEqual([0x0a, 0x0d, 0x0d, 0x0a]);
  }
});
