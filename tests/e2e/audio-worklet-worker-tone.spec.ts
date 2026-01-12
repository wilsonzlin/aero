import { expect, test } from "@playwright/test";

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
      maxAbsSample: (() => {
        if (!out?.ringBuffer?.samples || !out?.ringBuffer?.writeIndex) return null;
        const samples: Float32Array = out.ringBuffer.samples;
        const writeIndex: Uint32Array = out.ringBuffer.writeIndex;
        const cc = out.ringBuffer.channelCount | 0;
        const cap = out.ringBuffer.capacityFrames | 0;
        if (cc <= 0 || cap <= 0) return null;
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
        return maxAbs;
      })(),
    };
  });

  expect(result.enabled).toBe(true);
  expect(result.state).toBe("running");
  expect(result.backend).toBe("cpu-worker-wasm");
  // Underruns are tracked in *frames* (not “events”). One AudioWorklet render quantum is 128 frames,
  // so allowing 128 frames keeps the test stable while still catching sustained underruns.
  expect(result.underruns).toBeLessThanOrEqual(128);
  expect(result.overruns).toBe(0);
  expect(result.maxAbsSample).not.toBeNull();
  expect(result.maxAbsSample as number).toBeGreaterThan(0.01);

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
      return { ok: false, error: err instanceof Error ? err.message : String(err) };
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
