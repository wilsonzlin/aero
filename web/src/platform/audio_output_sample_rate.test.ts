import { afterEach, expect, test, vi } from "vitest";

import { createAudioOutput, getDefaultRingBufferFrames } from "./audio";

const GLOBALS = globalThis as unknown as { AudioContext?: unknown; webkitAudioContext?: unknown };
const ORIGINAL_AUDIO_CONTEXT = GLOBALS.AudioContext;
const ORIGINAL_WEBKIT_AUDIO_CONTEXT = GLOBALS.webkitAudioContext;

afterEach(() => {
  if (ORIGINAL_AUDIO_CONTEXT) {
    GLOBALS.AudioContext = ORIGINAL_AUDIO_CONTEXT;
  } else {
    delete GLOBALS.AudioContext;
  }

  if (ORIGINAL_WEBKIT_AUDIO_CONTEXT) {
    GLOBALS.webkitAudioContext = ORIGINAL_WEBKIT_AUDIO_CONTEXT;
  } else {
    delete GLOBALS.webkitAudioContext;
  }
});

test("createAudioOutput sizes default ring buffer using actual AudioContext.sampleRate", async () => {
  const requestedSampleRate = 48_000;
  const actualSampleRate = 44_100;

  let ctorRequestedSampleRate: number | undefined;

  class FakeAudioContext {
    sampleRate = actualSampleRate;
    destination = {};
    // Force the disabled-path by omitting `audioWorklet.addModule`.
    audioWorklet = null;

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);

    constructor(opts: { sampleRate?: number; latencyHint?: unknown } = {}) {
      ctorRequestedSampleRate = opts.sampleRate;
    }
  }

  GLOBALS.AudioContext = FakeAudioContext;

  const output = await createAudioOutput({ sampleRate: requestedSampleRate, latencyHint: "interactive" });
  expect(ctorRequestedSampleRate).toBe(requestedSampleRate);

  expect(output.enabled).toBe(false);
  expect(output.ringBuffer).toBeDefined();

  // Default capacity should be derived from the *actual* AudioContext rate (Safari/iOS can ignore
  // the requested sample rate).
  const expectedFrames = getDefaultRingBufferFrames(actualSampleRate);
  expect(output.ringBuffer?.capacityFrames).toBe(expectedFrames);

  // When returning a disabled output after creating an AudioContext, report the *actual* sample
  // rate for diagnostics.
  expect(output.getMetrics().sampleRate).toBe(actualSampleRate);

  expect(output.message).toContain("AudioWorklet is unavailable");
});

