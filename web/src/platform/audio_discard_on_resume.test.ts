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

test("createAudioOutput auto-discards buffered frames only after a suspend→running transition", async () => {
  class FakePort {
    start = vi.fn(() => undefined);
    postMessage = vi.fn((_message: unknown) => undefined);
  }

  class FakeAudioWorkletNode {
    port = new FakePort();
    connect = vi.fn((_dest: unknown) => undefined);
    disconnect = vi.fn(() => undefined);
    constructor(
      _context: unknown,
      _name: string,
      _options: {
        processorOptions?: unknown;
        outputChannelCount?: number[];
      },
    ) {}
  }

  class FakeAudioContext extends EventTarget {
    sampleRate = 48_000;
    destination = {};
    state: AudioContextState = "suspended";
    audioWorklet = {
      addModule: vi.fn(async (_url: string) => undefined),
    };

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);

    constructor(_opts: { sampleRate?: number; latencyHint?: unknown } = {}) {
      super();
    }

    _setState(next: AudioContextState) {
      this.state = next;
      this.dispatchEvent(new Event("statechange"));
    }
  }

  GLOBALS.AudioContext = FakeAudioContext;
  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  const output = await createAudioOutput();
  expect(output.enabled).toBe(true);
  if (!output.enabled) return;

  const port = output.node.port as unknown as FakePort;

  // First-ever transition to running: do not discard (avoid breaking initial silence prefill).
  (output.context as unknown as FakeAudioContext)._setState("running");
  expect(port.postMessage).not.toHaveBeenCalled();

  // Later suspend → running: discard buffered backlog.
  (output.context as unknown as FakeAudioContext)._setState("suspended");
  (output.context as unknown as FakeAudioContext)._setState("running");

  expect(port.postMessage).toHaveBeenCalledTimes(1);
  expect(port.postMessage).toHaveBeenCalledWith({ type: "ring.reset" });
  expect(port.start).toHaveBeenCalled();

  await output.close();
});
