import { afterEach, expect, test, vi } from "vitest";

import { MicCapture } from "./mic_capture";

const ORIGINAL_AUDIO_CONTEXT = (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
const ORIGINAL_MEDIA_DEVICES = (navigator as unknown as { mediaDevices?: unknown }).mediaDevices;

afterEach(() => {
  if (ORIGINAL_AUDIO_CONTEXT) {
    (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = ORIGINAL_AUDIO_CONTEXT;
  } else {
    delete (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext;
  }

  if (ORIGINAL_MEDIA_DEVICES) {
    (navigator as unknown as { mediaDevices?: unknown }).mediaDevices = ORIGINAL_MEDIA_DEVICES;
  } else {
    delete (navigator as unknown as { mediaDevices?: unknown }).mediaDevices;
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

  (globalThis as typeof globalThis & { AudioContext?: unknown }).AudioContext = FakeAudioContext;

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
