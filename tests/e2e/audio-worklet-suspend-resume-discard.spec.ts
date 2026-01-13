import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioContext suspend/resume discards playback ring backlog (stale latency avoidance)", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet suspend/resume discard test only runs on Chromium.");

  await page.goto(`${PREVIEW_ORIGIN}/`, { waitUntil: "load" });

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

  const CAPACITY_FRAMES = 48_000;
  // Target a large backlog so that "no discard" behaviour would require hundreds of ms
  // of real-time playback to drain, even on higher-sample-rate devices.
  const BACKLOG_TARGET_FRAMES = 40_000;
  const BACKLOG_DISCARDED_THRESHOLD_FRAMES = 512;

  const setupResult = await page.evaluate(
    ({ CAPACITY_FRAMES }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutput;
      if (!out?.enabled) return { ok: false as const, reason: "Missing enabled audio output." };

      const context: AudioContext = out.context;
      if (!context) return { ok: false as const, reason: "Missing AudioContext." };
      if (typeof SharedArrayBuffer === "undefined") {
        return { ok: false as const, reason: "SharedArrayBuffer unavailable." };
      }

      const channelCount = (out?.ringBuffer?.channelCount as number | undefined) ?? 2;
      const headerBytes = 4 * Uint32Array.BYTES_PER_ELEMENT;
      const sab = new SharedArrayBuffer(headerBytes + CAPACITY_FRAMES * channelCount * Float32Array.BYTES_PER_ELEMENT);
      const header = new Uint32Array(sab, 0, 4);
      const samples = new Float32Array(sab, headerBytes);

      const ring = {
        buffer: sab,
        header,
        readIndex: header.subarray(0, 1),
        writeIndex: header.subarray(1, 2),
        underrunCount: header.subarray(2, 3),
        overrunCount: header.subarray(3, 4),
        samples,
        channelCount,
        capacityFrames: CAPACITY_FRAMES,
      };

      Atomics.store(ring.readIndex, 0, 0);
      Atomics.store(ring.writeIndex, 0, 0);
      Atomics.store(ring.underrunCount, 0, 0);
      Atomics.store(ring.overrunCount, 0, 0);

      // Disconnect the harness' default node so it stops consuming/writing into its
      // own ring while we run this spec. (The demo tone loop is level-based; once
      // disconnected, it will stop writing after the initial fill.)
      try {
        out.node?.disconnect?.();
      } catch {
        // ignore
      }

      let node: AudioWorkletNode;
      try {
        node = new AudioWorkletNode(context, "aero-audio-processor", {
          processorOptions: {
            ringBuffer: sab,
            channelCount,
            capacityFrames: CAPACITY_FRAMES,
            // Task 20: enable resume-discard behaviour when supported (harmless for older processors).
            discardOnResume: true,
          },
          outputChannelCount: [channelCount],
        });
        node.connect(context.destination);
      } catch (err) {
        return {
          ok: false as const,
          reason: err instanceof Error ? `Failed to create AudioWorkletNode: ${err.message}` : "Failed to create AudioWorkletNode.",
        };
      }

      const framesAvailableClamped = (read: number, write: number) => {
        const cap = CAPACITY_FRAMES >>> 0;
        return Math.min(((write - read) >>> 0) >>> 0, cap);
      };

      const framesFree = (read: number, write: number) => {
        const cap = CAPACITY_FRAMES >>> 0;
        return (cap - framesAvailableClamped(read, write)) >>> 0;
      };

      const writeInterleaved = (input: Float32Array) => {
        const cc = channelCount | 0;
        const requestedFrames = Math.floor(input.length / cc);
        if (requestedFrames <= 0) return 0;

        const read = Atomics.load(ring.readIndex, 0) >>> 0;
        const write = Atomics.load(ring.writeIndex, 0) >>> 0;
        const free = framesFree(read, write);
        const framesToWrite = Math.min(requestedFrames, free);
        const dropped = requestedFrames - framesToWrite;
        if (dropped > 0) Atomics.add(ring.overrunCount, 0, dropped);
        if (framesToWrite === 0) return 0;

        const cap = CAPACITY_FRAMES >>> 0;
        const writePos = write % cap;
        const firstFrames = Math.min(framesToWrite, cap - writePos);
        const secondFrames = framesToWrite - firstFrames;
        const firstSamples = firstFrames * cc;
        const totalSamples = framesToWrite * cc;
        ring.samples.set(input.subarray(0, firstSamples), writePos * cc);
        if (secondFrames > 0) {
          ring.samples.set(input.subarray(firstSamples, totalSamples), 0);
        }
        Atomics.store(ring.writeIndex, 0, write + framesToWrite);
        return framesToWrite;
      };

      const producer = {
        timer: null as number | null,
        tickMs: 20,
        framesPerTick: 2048,
        chunk: null as Float32Array | null,
      };
      producer.chunk = new Float32Array(producer.framesPerTick * channelCount);
      for (let i = 0; i < producer.framesPerTick; i++) {
        const s = Math.sin((i / producer.framesPerTick) * 2 * Math.PI) * 0.1;
        for (let c = 0; c < channelCount; c++) producer.chunk[i * channelCount + c] = s;
      }

      const startProducer = () => {
        if (producer.timer !== null) return;
        const id = window.setInterval(() => {
          writeInterleaved(producer.chunk!);
        }, producer.tickMs);
        (id as unknown as { unref?: () => void }).unref?.();
        producer.timer = id;
      };

      const stopProducer = () => {
        if (producer.timer === null) return;
        window.clearInterval(producer.timer);
        producer.timer = null;
      };

      const getMetrics = () => {
        const read = Atomics.load(ring.readIndex, 0) >>> 0;
        const write = Atomics.load(ring.writeIndex, 0) >>> 0;
        const level = framesAvailableClamped(read, write);
        const overrun = Atomics.load(ring.overrunCount, 0) >>> 0;
        return { read, write, level, overrun, state: context.state };
      };

      // Expose the custom output for subsequent `page.evaluate()` calls.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (globalThis as any).__aeroAudioSuspendResumeDiscardTest = {
        context,
        node,
        ring,
        startProducer,
        stopProducer,
        getMetrics,
      };

      startProducer();

      return { ok: true as const };
    },
    { CAPACITY_FRAMES },
  );

  if (!setupResult.ok) {
    test.skip(true, `Failed to set up suspend/resume harness: ${setupResult.reason}`);
  }

  // Ensure the consumer is actually alive (read index advances) before we suspend.
  const warmupRead = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t?.getMetrics?.()?.read ?? 0;
  });

  await page.waitForFunction(
    (baselineRead) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
      const rec = t?.getMetrics?.();
      if (!rec) return false;
      return (((rec.read as number) - (baselineRead as number)) >>> 0) > 0;
    },
    warmupRead,
    { timeout: 20_000 },
  );

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

  // Let the producer run while the AudioWorklet consumer is suspended.
  // Wait until we have a large backlog (or the ring saturates and starts overrunning).
  await page.waitForFunction(
    ({ BACKLOG_TARGET_FRAMES }) => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
      const rec = t?.getMetrics?.();
      if (!rec) return false;
      return (rec.level as number) >= (BACKLOG_TARGET_FRAMES as number) || (rec.overrun as number) > 0;
    },
    { BACKLOG_TARGET_FRAMES },
    { timeout: 5_000 },
  );

  const afterSuspend = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  expect(afterSuspend.state).toBe("suspended");
  // Ensure the backlog actually grew while suspended (or we hit producer overruns, which
  // implies the backlog reached capacity).
  const backlogIncreased = afterSuspend.level > beforeSuspend.level;
  const overrunIncreased = afterSuspend.overrun > beforeSuspend.overrun;
  expect(backlogIncreased || overrunIncreased).toBe(true);
  expect(afterSuspend.level >= BACKLOG_TARGET_FRAMES || overrunIncreased).toBe(true);

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

  // On resume, discard any accumulated backlog quickly; otherwise the output would play
  // stale buffered audio for hundreds of ms.
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

  // Best-effort cleanup: disconnect the test node so future specs aren't impacted if the page
  // is reused in the same worker (rare, but cheap to do).
  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    try {
      t.stopProducer();
    } catch {
      // ignore
    }
    try {
      t.node?.disconnect?.();
    } catch {
      // ignore
    }
  });
});
