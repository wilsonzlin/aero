import { afterEach, expect, test, vi } from "vitest";

import { MicCapture } from "./mic_capture";

const GLOBALS = globalThis as unknown as { AudioContext?: unknown; AudioWorkletNode?: unknown };
const ORIGINAL_AUDIO_WORKLET_NODE = GLOBALS.AudioWorkletNode;

const ORIGINAL_AUDIO_CONTEXT = GLOBALS.AudioContext;
const ORIGINAL_MEDIA_DEVICES = (navigator as unknown as { mediaDevices?: unknown }).mediaDevices;

afterEach(() => {
  if (ORIGINAL_AUDIO_CONTEXT) {
    GLOBALS.AudioContext = ORIGINAL_AUDIO_CONTEXT;
  } else {
    delete GLOBALS.AudioContext;
  }

  if (ORIGINAL_MEDIA_DEVICES) {
    (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = ORIGINAL_MEDIA_DEVICES;
  } else {
    delete (navigator as unknown as { mediaDevices?: unknown }).mediaDevices;
  }

  if (ORIGINAL_AUDIO_WORKLET_NODE) {
    GLOBALS.AudioWorkletNode = ORIGINAL_AUDIO_WORKLET_NODE;
  } else {
    delete GLOBALS.AudioWorkletNode;
  }
});

test("MicCapture exposes AudioContext.sampleRate as actualSampleRate (even if requested differs)", async () => {
  const actualSampleRate = 44_100;

  let requestedSampleRate: number | undefined;

  class FakeNode {
    connect = vi.fn();
    disconnect = vi.fn();
  }

  class FakeGainNode extends FakeNode {
    gain = { value: 1 };
  }

  class FakeScriptProcessorNode extends FakeNode {
    onaudioprocess: ((ev: unknown) => void) | null = null;
  }

  class FakeAudioContext {
    sampleRate = actualSampleRate;
    destination = {};
    audioWorklet = { addModule: vi.fn(async () => undefined) };

    constructor(opts: { sampleRate?: number } = {}) {
      requestedSampleRate = opts.sampleRate;
    }

    createGain(): FakeGainNode {
      return new FakeGainNode();
    }

    createMediaStreamSource(_stream: unknown): FakeNode {
      return new FakeNode();
    }

    createScriptProcessor(_bufferSize: number, _inChannels: number, _outChannels: number): FakeScriptProcessorNode {
      return new FakeScriptProcessorNode();
    }

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);
  }

  GLOBALS.AudioContext = FakeAudioContext;

  const track = { addEventListener: vi.fn(), stop: vi.fn() };
  const stream = {
    getAudioTracks: () => [track],
    getTracks: () => [track],
  };

  (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = {
    getUserMedia: vi.fn(async () => stream),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  const mic = new MicCapture({ sampleRate: 48_000, bufferMs: 100, preferWorklet: true });
  await mic.start();

  expect(requestedSampleRate).toBe(48_000);
  expect(mic.options.sampleRate).toBe(48_000);
  expect(mic.actualSampleRate).toBe(actualSampleRate);

  // Ring buffer capacity should be derived from the *actual* capture rate.
  expect(mic.ringBuffer.capacity).toBe(Math.max(1, Math.floor((actualSampleRate * mic.options.bufferMs) / 1000)));

  await mic.stop();
});

test("MicCapture captures track debug info and clears it on stop", async () => {
  class FakeNode {
    connect = vi.fn();
    disconnect = vi.fn();
  }

  class FakeGainNode extends FakeNode {
    gain = { value: 1 };
  }

  class FakeAudioWorkletNode extends FakeNode {
    port = { onmessage: null as unknown };
    constructor(..._args: unknown[]) {
      super();
    }
  }

  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};
    audioWorklet = { addModule: vi.fn(async () => undefined) };

    createGain(): FakeGainNode {
      return new FakeGainNode();
    }

    createMediaStreamSource(_stream: unknown): FakeNode {
      return new FakeNode();
    }

    createScriptProcessor(): never {
      throw new Error("Unexpected ScriptProcessorNode path");
    }

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);
  }

  GLOBALS.AudioContext = FakeAudioContext;

  const track = {
    label: "Test Mic",
    enabled: true,
    muted: false,
    readyState: "live",
    addEventListener: vi.fn(),
    stop: vi.fn(),
    getSettings: vi.fn(() => ({ deviceId: "raw-device-id", sampleRate: 48_000 })),
    getConstraints: vi.fn(() => ({ deviceId: { exact: "raw-device-id" } })),
    getCapabilities: vi.fn(() => ({ deviceId: "raw-device-id", echoCancellation: [true, false] })),
  };
  const stream = {
    getAudioTracks: () => [track],
    getTracks: () => [track],
  };

  (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = {
    getUserMedia: vi.fn(async () => stream),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  const mic = new MicCapture({ sampleRate: 48_000, bufferMs: 50, preferWorklet: true });
  await mic.start();

  const dbg = mic.getDebugInfo();
  expect(dbg.backend).toBe("worklet");
  expect(dbg.audioContextState).toBe(null);
  expect(dbg.workletInitError).toBe(null);
  expect(dbg.trackLabel).toBe("Test Mic");
  expect(dbg.trackEnabled).toBe(true);
  expect(dbg.trackMuted).toBe(false);
  expect(dbg.trackReadyState).toBe("live");
  expect(dbg.trackSettings).toEqual({ deviceId: "raw-device-id", sampleRate: 48_000 });
  expect(dbg.trackConstraints).toEqual({ deviceId: { exact: "raw-device-id" } });
  expect(dbg.trackCapabilities).toEqual({ deviceId: "raw-device-id", echoCancellation: [true, false] });

  await mic.stop();
  expect(mic.getDebugInfo()).toEqual({
    backend: null,
    audioContextState: null,
    workletInitError: null,
    trackLabel: null,
    trackEnabled: null,
    trackMuted: null,
    trackReadyState: null,
    trackSettings: null,
    trackConstraints: null,
    trackCapabilities: null,
  });
});

test("MicCapture falls back to ScriptProcessorNode when AudioWorklet initialization fails", async () => {
  class FakeNode {
    connect = vi.fn();
    disconnect = vi.fn();
  }

  class FakeGainNode extends FakeNode {
    gain = { value: 1 };
  }

  class FakeScriptProcessorNode extends FakeNode {
    onaudioprocess: ((ev: unknown) => void) | null = null;
  }

  // Presence of AudioWorkletNode makes MicCapture *attempt* worklet first.
  class FakeAudioWorkletNode extends FakeNode {
    port = { onmessage: null as unknown };
    constructor(..._args: unknown[]) {
      super();
    }
  }

  GLOBALS.AudioWorkletNode = FakeAudioWorkletNode;

  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};
    audioWorklet = { addModule: vi.fn(async () => Promise.reject(new Error("worklet load failed"))) };

    createGain(): FakeGainNode {
      return new FakeGainNode();
    }

    createMediaStreamSource(_stream: unknown): FakeNode {
      return new FakeNode();
    }

    createScriptProcessor(_bufferSize: number, _inChannels: number, _outChannels: number): FakeScriptProcessorNode {
      return new FakeScriptProcessorNode();
    }

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);
  }

  GLOBALS.AudioContext = FakeAudioContext;

  const track = { addEventListener: vi.fn(), stop: vi.fn() };
  const stream = {
    getAudioTracks: () => [track],
    getTracks: () => [track],
  };

  (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = {
    getUserMedia: vi.fn(async () => stream),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  const mic = new MicCapture({ sampleRate: 48_000, bufferMs: 50, preferWorklet: true });
  await mic.start();
  expect(mic.getDebugInfo().backend).toBe("script");
  expect(mic.getDebugInfo().workletInitError).toBe("worklet load failed");
  await mic.stop();
});

test("MicCapture script backend emits periodic stats messages", async () => {
  delete GLOBALS.AudioWorkletNode;

  class FakeNode {
    connect = vi.fn();
    disconnect = vi.fn();
  }

  class FakeGainNode extends FakeNode {
    gain = { value: 1 };
  }

  class FakeScriptProcessorNode extends FakeNode {
    onaudioprocess: ((ev: any) => void) | null = null;
  }

  class FakeAudioContext {
    sampleRate = 48_000;
    destination = {};

    createGain(): FakeGainNode {
      return new FakeGainNode();
    }

    createMediaStreamSource(_stream: unknown): FakeNode {
      return new FakeNode();
    }

    createScriptProcessor(_bufferSize: number, _inChannels: number, _outChannels: number): FakeScriptProcessorNode {
      return new FakeScriptProcessorNode();
    }

    resume = vi.fn(async () => undefined);
    close = vi.fn(async () => undefined);
  }

  GLOBALS.AudioContext = FakeAudioContext;

  const track = { addEventListener: vi.fn(), stop: vi.fn() };
  const stream = {
    getAudioTracks: () => [track],
    getTracks: () => [track],
  };

  (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = {
    getUserMedia: vi.fn(async () => stream),
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  };

  const mic = new MicCapture({ sampleRate: 48_000, bufferMs: 100, preferWorklet: false });
  const messages: any[] = [];
  mic.addEventListener("message", (ev) => {
    messages.push((ev as MessageEvent).data);
  });
  await mic.start();

  expect(mic.getDebugInfo().backend).toBe("script");
  expect(mic.getDebugInfo().workletInitError).toBe(null);

  const node = (mic as unknown as { scriptNode: FakeScriptProcessorNode }).scriptNode;
  expect(node).toBeTruthy();
  expect(typeof node.onaudioprocess).toBe("function");

  node.onaudioprocess?.({
    inputBuffer: {
      getChannelData: () => new Float32Array([0.1, 0.2, 0.3, 0.4]),
    },
  });

  const stats = messages.find((m) => m && typeof m === "object" && m.type === "stats");
  expect(stats).toBeTruthy();
  expect(stats.buffered).toBe(4);
  expect(stats.dropped).toBe(0);

  await mic.stop();
});
