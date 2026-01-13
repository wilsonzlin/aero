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

test("createAudioOutput falls back when AudioContext constructor rejects option objects", async () => {
  const ctorCalls: unknown[][] = [];

  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};
    audioWorklet = {
      addModule: vi.fn(async () => undefined),
    };

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);

    constructor(...args: unknown[]) {
      ctorCalls.push(args);
      if (args.length > 0) {
        throw new TypeError("AudioContext options are not supported");
      }
    }
  }

  class FakeAudioWorkletNode {
    connect = vi.fn();
    disconnect = vi.fn();
    constructor(_context: unknown, _name: string, _options?: unknown) {}
  }

  GLOBALS.AudioContext = FakeAudioContext;
  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive" });
  expect(output.enabled).toBe(true);

  // Ensure we progressively retried until the no-args constructor path.
  expect(ctorCalls.length).toBe(4);
  expect(ctorCalls[0]?.length).toBe(1);
  expect(ctorCalls[1]?.length).toBe(1);
  expect(ctorCalls[2]?.length).toBe(1);
  expect(ctorCalls[3]?.length).toBe(0);

  if (output.enabled) {
    await output.close();
  }
});

test("createAudioOutput falls back when AudioWorkletNode rejects outputChannelCount", async () => {
  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};
    audioWorklet = {
      addModule: vi.fn(async () => undefined),
    };

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);

    constructor(_opts: unknown = {}) {}
  }

  const nodeCtorCalls: unknown[] = [];

  class FakeAudioWorkletNode {
    connect = vi.fn();
    disconnect = vi.fn();
    constructor(_context: unknown, _name: string, options?: unknown) {
      nodeCtorCalls.push(options);
      if (options && typeof options === "object" && "outputChannelCount" in options) {
        throw new TypeError("outputChannelCount not supported");
      }
    }
  }

  GLOBALS.AudioContext = FakeAudioContext;
  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  const output = await createAudioOutput({ sampleRate: 48_000, latencyHint: "interactive", channelCount: 2 });
  expect(output.enabled).toBe(true);

  expect(nodeCtorCalls.length).toBe(2);
  expect(nodeCtorCalls[0]).toMatchObject({ outputChannelCount: [2] });
  expect(nodeCtorCalls[1]).not.toHaveProperty("outputChannelCount");

  if (output.enabled) {
    await output.close();
  }
});

