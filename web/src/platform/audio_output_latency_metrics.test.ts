import { afterEach, expect, test, vi } from "vitest";

import { createAudioOutput } from "./audio";

const GLOBALS = globalThis as unknown as { AudioContext?: unknown; webkitAudioContext?: unknown; AudioWorkletNode?: unknown };
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

test("EnabledAudioOutput.getMetrics() includes Web Audio baseLatency/outputLatency when available", async () => {
  class FakeAudioWorklet {
    addModule = vi.fn(async () => undefined);
  }

  class FakeAudioContext {
    readonly sampleRate: number;
    state: AudioContextState = "suspended";
    readonly destination = {};
    readonly audioWorklet = new FakeAudioWorklet();
    readonly baseLatency = 0.123;
    readonly outputLatency = 0.456;

    resume = vi.fn(async () => {
      this.state = "running";
    });
    close = vi.fn(async () => {
      this.state = "closed";
    });

    constructor(opts: { sampleRate?: number; latencyHint?: unknown } = {}) {
      this.sampleRate = opts.sampleRate ?? 48_000;
    }
  }

  class FakeAudioWorkletNode {
    connect = vi.fn(() => undefined);
    disconnect = vi.fn(() => undefined);
    port = { addEventListener: vi.fn(), removeEventListener: vi.fn(), start: vi.fn() };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    constructor(_context: unknown, _name: string, _options?: any) {}
  }

  GLOBALS.AudioContext = FakeAudioContext;
  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  const output = await createAudioOutput({ sampleRate: 48_000 });
  expect(output.enabled).toBe(true);
  if (!output.enabled) throw new Error("Expected enabled audio output");

  const metrics = output.getMetrics();
  expect(metrics.baseLatencySeconds).toBe(0.123);
  expect(metrics.outputLatencySeconds).toBe(0.456);

  await output.close();
});

