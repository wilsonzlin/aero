import { afterEach, expect, test, vi } from "vitest";

import { createAudioOutput } from "./audio";

const GLOBALS = globalThis as unknown as {
  AudioContext?: unknown;
  webkitAudioContext?: unknown;
  AudioWorkletNode?: unknown;
};

const ORIGINAL_AUDIO_CONTEXT = GLOBALS.AudioContext;
const ORIGINAL_WEBKIT_AUDIO_CONTEXT = GLOBALS.webkitAudioContext;
const ORIGINAL_AUDIO_WORKLET_NODE = GLOBALS.AudioWorkletNode;

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

  if (ORIGINAL_AUDIO_WORKLET_NODE) {
    GLOBALS.AudioWorkletNode = ORIGINAL_AUDIO_WORKLET_NODE;
  } else {
    delete GLOBALS.AudioWorkletNode;
  }
});

test("EnabledAudioOutput.resume retries after initial resume rejection", async () => {
  let resumeCalls = 0;

  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};
    state: AudioContextState = "suspended";
    audioWorklet = {
      addModule: vi.fn(async () => undefined),
    };

    resume = vi.fn(() => {
      resumeCalls += 1;
      if (resumeCalls === 1) {
        return Promise.reject(new Error("NotAllowedError"));
      }
      this.state = "running";
      return Promise.resolve();
    });

    close = vi.fn(async () => undefined);

    constructor(_opts: { sampleRate?: number; latencyHint?: unknown } = {}) {}
  }

  class FakeAudioWorkletNode {
    connect = vi.fn();
    disconnect = vi.fn();
    port = {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      start: vi.fn(),
      postMessage: vi.fn(),
    };

    constructor(_context: unknown, _name: string, _opts: unknown) {}
  }

  GLOBALS.AudioContext = FakeAudioContext;
  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  const output = await createAudioOutput();
  expect(output.enabled).toBe(true);
  if (!output.enabled) throw new Error(output.message);

  // The initial `AudioContext.resume()` attempt happens during `createAudioOutput()` and may
  // reject (e.g. outside a user gesture). `output.resume()` must be able to retry later.
  await expect(output.resume()).resolves.toBeUndefined();
  expect(resumeCalls).toBeGreaterThanOrEqual(2);
});

