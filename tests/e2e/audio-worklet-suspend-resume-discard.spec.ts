import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioContext suspend/resume discards playback ring backlog (stale latency avoidance)", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet suspend/resume discard test only runs on Chromium.");

  // Use a large ring buffer so that "no discard" behaviour would require hundreds of ms of real-time
  // playback to drain, making the regression easy to detect.
  const url = new URL(`${PREVIEW_ORIGIN}/`);
  url.searchParams.set("ringBufferFrames", "48000");
  await page.goto(url.toString(), { waitUntil: "load" });

  const support = await page.evaluate(() => {
    const AudioContextCtor =
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ((globalThis as any).AudioContext ?? (globalThis as any).webkitAudioContext) as unknown;
    return {
      audioContext: typeof AudioContextCtor === "function",
      sharedArrayBuffer: typeof SharedArrayBuffer !== "undefined",
      atomics: typeof Atomics !== "undefined",
    };
  });

  if (!support.audioContext) test.skip(true, "Web Audio API unavailable (AudioContext missing).");
  if (!support.sharedArrayBuffer || !support.atomics) {
    test.skip(true, "SharedArrayBuffer/Atomics unavailable (requires cross-origin isolation).");
  }

  // Ensure we have a user gesture to satisfy autoplay policies.
  await page.click("#init-audio-output");

  await page.waitForFunction(() => {
    // Exposed by the audio UI entrypoint (`src/main.ts` in the root app).
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return out?.enabled === true && out?.context?.state === "running";
  });

  const initResult = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    if (!out) return { ok: false as const, reason: "Missing __aeroAudioOutput global." };
    if (!out.enabled) {
      return { ok: false as const, reason: typeof out.message === "string" ? out.message : "Audio output disabled." };
    }
    return { ok: true as const };
  });

  if (!initResult.ok) {
    test.skip(true, `Audio output unavailable: ${initResult.reason}`);
  }

  // Sanity check: ensure the AudioWorklet consumer is actually running (read index advances)
  // before we start the suspend/resume cycle.
  const initialRead = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    if (!out?.enabled) return null;
    const ring = out.ringBuffer as { readIndex?: Uint32Array } | undefined;
    if (!ring?.readIndex) return null;
    return Atomics.load(ring.readIndex, 0) >>> 0;
  });
  expect(initialRead).not.toBeNull();

  await page.waitForFunction(
    (baselineRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutput;
      if (!out?.enabled) return false;
      const ring = out.ringBuffer as { readIndex?: Uint32Array } | undefined;
      if (!ring?.readIndex) return false;
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      return ((read - (baselineRead as number)) >>> 0) > 0;
    },
    initialRead,
    { timeout: 20_000 },
  );

  const CAPACITY_FRAMES = 48_000;
  const BACKLOG_TARGET_FRAMES = 40_000;
  const BACKLOG_DISCARDED_THRESHOLD_FRAMES = 512;

  const setupResult = await page.evaluate(({ CAPACITY_FRAMES }) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    if (!out?.enabled) return { ok: false as const, reason: "Missing enabled audio output." };

    const ring = out.ringBuffer as
      | {
          readIndex: Uint32Array;
          writeIndex: Uint32Array;
          overrunCount: Uint32Array;
          channelCount: number;
          capacityFrames: number;
        }
      | undefined;
    if (!ring?.readIndex || !ring?.writeIndex || !ring?.overrunCount) {
      return { ok: false as const, reason: "Missing audio ring buffer views." };
    }

    const capacity = ring.capacityFrames >>> 0;
    if (capacity < (CAPACITY_FRAMES as number)) {
      return {
        ok: false as const,
        reason: `Unexpected ring capacity (${capacity} frames); expected >= ${CAPACITY_FRAMES}.`,
      };
    }

    if (typeof out.writeInterleaved !== "function") {
      return { ok: false as const, reason: "Missing audioOutput.writeInterleaved()." };
    }

    // The harness installs a demo tone producer that keeps the ring ~200ms full. Disable it so it
    // doesn't refill the buffer after resume/discard (we want to observe the discard promptly).
    const originalWriteInterleaved = out.writeInterleaved.bind(out) as (samples: Float32Array, srcRate: number) => number;
    out.writeInterleaved = () => 0;

    const context: AudioContext = out.context;
    const channelCount = ring.channelCount | 0;
    const framesPerTick = 2048;
    const tickMs = 20;
    const chunk = new Float32Array(framesPerTick * channelCount);
    for (let i = 0; i < framesPerTick; i++) {
      const s = Math.sin((i / framesPerTick) * 2 * Math.PI) * 0.1;
      for (let c = 0; c < channelCount; c++) chunk[i * channelCount + c] = s;
    }

    let producerTimer: number | null = null;
    const startProducer = () => {
      if (producerTimer !== null) return;
      const id = window.setInterval(() => {
        try {
          originalWriteInterleaved(chunk, context.sampleRate);
        } catch {
          // ignore
        }
      }, tickMs);
      (id as unknown as { unref?: () => void }).unref?.();
      producerTimer = id;
    };
    const stopProducer = () => {
      if (producerTimer === null) return;
      window.clearInterval(producerTimer);
      producerTimer = null;
    };

    const getMetrics = () => {
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      const overrun = Atomics.load(ring.overrunCount, 0) >>> 0;
      const level = typeof out.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : ((write - read) >>> 0);
      return { read, write, overrun, level, capacity, state: context.state, sampleRate: context.sampleRate };
    };

    // Expose for subsequent eval steps.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroAudioSuspendResumeDiscardTest = {
      out,
      context,
      ring,
      originalWriteInterleaved,
      startProducer,
      stopProducer,
      getMetrics,
    };

    return { ok: true as const, capacity };
  }, { CAPACITY_FRAMES });

  if (!setupResult.ok) {
    throw new Error(`Failed to set up suspend/resume harness: ${setupResult.reason}`);
  }

  expect(setupResult.capacity).toBeGreaterThanOrEqual(CAPACITY_FRAMES);

  const beforeSuspend = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });
  expect(beforeSuspend.state).toBe("running");

  await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    await t.context.suspend();
  });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t?.context?.state === "suspended";
  });

  const suspendedBaseline = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    t.startProducer();
  });

  // Wait until we have a large backlog (or the ring saturates and starts overrunning).
  await page.waitForFunction(
    ({ BACKLOG_TARGET_FRAMES, baselineOverrun }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
      const rec = t?.getMetrics?.();
      if (!rec) return false;
      return (rec.level as number) >= (BACKLOG_TARGET_FRAMES as number) || (rec.overrun as number) > (baselineOverrun as number);
    },
    { BACKLOG_TARGET_FRAMES, baselineOverrun: suspendedBaseline.overrun },
    { timeout: 5_000 },
  );

  const afterFill = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  expect(afterFill.state).toBe("suspended");
  expect(afterFill.level >= BACKLOG_TARGET_FRAMES || afterFill.overrun > suspendedBaseline.overrun).toBe(true);

  // Stop the producer so the ring indices remain stable for the discard check.
  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    t.stopProducer();
  });

  await page.evaluate(async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    await t.context.resume();
  });

  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t?.context?.state === "running";
  });

  // On resume, discard any accumulated backlog quickly; otherwise the output would play stale buffered
  // audio for hundreds of ms.
  await page.waitForFunction(
    ({ BACKLOG_DISCARDED_THRESHOLD_FRAMES }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
      const rec = t?.getMetrics?.();
      if (!rec) return false;
      return (rec.level as number) < (BACKLOG_DISCARDED_THRESHOLD_FRAMES as number);
    },
    { BACKLOG_DISCARDED_THRESHOLD_FRAMES },
    { timeout: 350 },
  );

  const afterResume = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  expect(afterResume.state).toBe("running");
  expect(afterResume.level).toBeLessThan(BACKLOG_DISCARDED_THRESHOLD_FRAMES);

  // Best-effort cleanup: restore the original write method so the harness doesn't keep the ring empty
  // if the page is reused.
  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    try {
      t.stopProducer();
    } catch {
      // ignore
    }
    try {
      t.out.writeInterleaved = t.originalWriteInterleaved;
    } catch {
      // ignore
    }
  });
});
