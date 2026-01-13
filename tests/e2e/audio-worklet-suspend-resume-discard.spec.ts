import { expect, test } from "@playwright/test";

const PREVIEW_ORIGIN = process.env.AERO_PLAYWRIGHT_PREVIEW_ORIGIN ?? "http://127.0.0.1:4173";

test("AudioContext suspend/resume discards playback ring backlog (stale latency avoidance)", async ({ page }) => {
  test.setTimeout(60_000);
  test.skip(test.info().project.name !== "chromium", "AudioWorklet suspend/resume discard test only runs on Chromium.");

  // Use a large ring buffer so that "no discard" behaviour would require hundreds of ms of real-time
  // playback to drain, making the regression easy to detect.
  const url = new URL(`${PREVIEW_ORIGIN}/`);
  // Use a multi-second ring so that even at unusually high `AudioContext.sampleRate` values, a
  // large backlog cannot naturally drain within the bounded resume window (without an explicit
  // `ring.reset` discard).
  //
  // 262144 frames is ~5.5s @ 48kHz and ~1.4s @ 192kHz.
  url.searchParams.set("ringBufferFrames", "262144");
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

  // Wait for the harness to publish the audio output handle (it can be disabled if browser APIs
  // are missing or initialization fails).
  await page.waitForFunction(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    return out && typeof out.enabled === "boolean";
  });

  const initResult = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    if (!out) return { ok: false as const, reason: "Missing __aeroAudioOutput global." };
    if (!out.enabled) {
      return { ok: false as const, reason: typeof out.message === "string" ? out.message : "Audio output disabled." };
    }
    return { ok: true as const, state: out?.context?.state ?? null };
  });

  if (!initResult.ok) {
    test.skip(true, `Audio output unavailable: ${initResult.reason}`);
  }

  // Ensure the context is actually running (autoplay policies can leave it suspended even after
  // the output is enabled).
  if (initResult.state !== "running") {
    await page.waitForFunction(() => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const out = (globalThis as any).__aeroAudioOutput;
      return out?.enabled === true && out?.context?.state === "running";
    });
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

  const CAPACITY_FRAMES = 262_144;
  const BACKLOG_TARGET_FRAMES = 250_000;
  const BACKLOG_DISCARDED_THRESHOLD_FRAMES = 512;
  // If resume-discard is broken, draining ~120k frames at 48kHz takes ~2.5s (and still >600ms at
  // 192kHz). Keep the assertion window tight enough that a real-time drain can't satisfy it, while
  // still allowing for CI jitter.
  const DISCARD_TIMEOUT_MS = 150;

  const setupResult = await page.evaluate(({ CAPACITY_FRAMES }) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const out = (globalThis as any).__aeroAudioOutput;
    if (!out?.enabled) return { ok: false as const, reason: "Missing enabled audio output." };

    const ring = out.ringBuffer as
      | {
          readIndex: Uint32Array;
          writeIndex: Uint32Array;
          overrunCount: Uint32Array;
          samples: Float32Array;
          channelCount: number;
          capacityFrames: number;
          header?: Uint32Array;
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

    const node = out.node as AudioWorkletNode | undefined;
    if (!node?.port || typeof node.port.postMessage !== "function") {
      return { ok: false as const, reason: "Missing AudioWorkletNode port." };
    }

    // The harness installs a demo tone producer that keeps the ring ~200ms full. Disable it so it
    // doesn't refill the buffer after resume/discard (we want to observe the discard promptly).
    const originalWriteInterleaved = out.writeInterleaved.bind(out) as (samples: Float32Array, srcRate: number) => number;
    out.writeInterleaved = () => 0;

    // Track whether the resume-discard mechanism actually posts the control message to the worklet.
    // This makes the test explicitly cover the implemented feature (a `{ type: "ring.reset" }`
    // message on resume), not an implicit browser behaviour.
    let ringResetPosts = 0;
    let portPatched = false;
    let originalPortPostMessage: ((message: unknown, transfer?: Transferable[]) => void) | null = null;
    try {
      originalPortPostMessage = node.port.postMessage.bind(node.port) as (message: unknown, transfer?: Transferable[]) => void;
      const patched = (message: unknown, transfer?: Transferable[]) => {
        try {
          const msg = message as { type?: unknown } | null;
          if (msg && typeof msg === "object" && msg.type === "ring.reset") ringResetPosts++;
        } catch {
          // ignore
        }
        if (transfer !== undefined) {
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          return (originalPortPostMessage as any)(message, transfer);
        }
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        return (originalPortPostMessage as any)(message);
      };
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (node.port as any).postMessage = patched;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      portPatched = (node.port as any).postMessage === patched;
    } catch {
      // ignore; the buffer-level assertion still validates discard behaviour.
    }

    const context: AudioContext = out.context;

    const fillBacklog = (targetFrames: number) => {
      const frames = Math.max(0, Math.min(capacity, Math.floor(Number.isFinite(targetFrames) ? targetFrames : 0)));
      if (frames === 0) return;

      // Best-effort: write silence into the ring (so even if discard is broken, playback is not a
      // loud glitch) and then advance the producer write index while the AudioContext is suspended.
      //
      // The ring is shared memory; advancing `writeIndex` is sufficient to create a backlog that
      // the AudioWorklet will drain unless it receives a `{ type: "ring.reset" }`.
      try {
        ring.samples?.fill(0);
      } catch {
        // ignore
      }

      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      const currentWrite = Atomics.load(ring.writeIndex, 0) >>> 0;
      const currentLevel = typeof out.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : ((currentWrite - read) >>> 0);
      if (currentLevel >= frames) return;

      Atomics.store(ring.writeIndex, 0, (read + frames) >>> 0);
    };

    const getMetrics = () => {
      const read = Atomics.load(ring.readIndex, 0) >>> 0;
      const write = Atomics.load(ring.writeIndex, 0) >>> 0;
      const overrun = Atomics.load(ring.overrunCount, 0) >>> 0;
      const level = typeof out.getBufferLevelFrames === "function" ? out.getBufferLevelFrames() : ((write - read) >>> 0);
      return {
        read,
        write,
        overrun,
        level,
        capacity,
        state: context.state,
        sampleRate: context.sampleRate,
        ringResetPosts,
        portPatched,
      };
    };

    // Expose for subsequent eval steps.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (globalThis as any).__aeroAudioSuspendResumeDiscardTest = {
      out,
      context,
      ring,
      originalWriteInterleaved,
      originalPortPostMessage,
      fillBacklog,
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

  await page.evaluate(({ BACKLOG_TARGET_FRAMES }) => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    t.fillBacklog(BACKLOG_TARGET_FRAMES);
  }, { BACKLOG_TARGET_FRAMES });

  const afterFill = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  expect(afterFill.state).toBe("suspended");
  expect(afterFill.level).toBeGreaterThanOrEqual(BACKLOG_TARGET_FRAMES);
  // Ensure the backlog-building step actually did work (avoid silently passing due to an already-empty ring).
  expect(afterFill.level).toBeGreaterThanOrEqual(suspendedBaseline.level);

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
    { timeout: DISCARD_TIMEOUT_MS },
  );

  const afterResume = await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    return t.getMetrics();
  });

  expect(afterResume.state).toBe("running");
  expect(afterResume.level).toBeLessThan(BACKLOG_DISCARDED_THRESHOLD_FRAMES);
  if (afterResume.portPatched) {
    expect(afterResume.ringResetPosts).toBeGreaterThanOrEqual(1);
  }
  // Stronger assertion than bufferLevelFrames alone: a discard should advance the consumer read
  // index all the way to the producer write index (i.e. an empty ring).
  expect(((afterResume.write - afterResume.read) >>> 0)).toBe(0);

  // Best-effort cleanup: restore the original write method so the harness doesn't keep the ring empty
  // if the page is reused.
  await page.evaluate(() => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const t = (globalThis as any).__aeroAudioSuspendResumeDiscardTest;
    try {
      if (t.originalPortPostMessage) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        t.out.node.port.postMessage = t.originalPortPostMessage;
      }
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
