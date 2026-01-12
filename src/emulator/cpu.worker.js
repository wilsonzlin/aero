/// <reference lib="webworker" />

import { ErrorCode, EmulatorError, outOfMemory, resourceLimitExceeded, serializeError } from "../errors.js";
import { SizedLruCache } from "../resourceLimits.js";
import { nowMs, sleep, yieldToEventLoop } from "../utils.js";
import {
  CAPACITY_SAMPLES_INDEX as MIC_CAPACITY_SAMPLES_INDEX,
  DROPPED_SAMPLES_INDEX as MIC_DROPPED_SAMPLES_INDEX,
  HEADER_BYTES as MIC_HEADER_BYTES,
  HEADER_U32_LEN as MIC_HEADER_U32_LEN,
  READ_POS_INDEX as MIC_READ_POS_INDEX,
  WRITE_POS_INDEX as MIC_WRITE_POS_INDEX,
} from "../../web/src/audio/mic_ring.js";

const ctx = /** @type {DedicatedWorkerGlobalScope} */ (/** @type {unknown} */ (self));

const DEFAULT_CONFIG = Object.freeze({
  guestRamBytes: 64 * 1024 * 1024,
  limits: {
    maxGuestRamBytes: 512 * 1024 * 1024,
    maxDiskCacheBytes: 128 * 1024 * 1024,
    maxShaderCacheBytes: 64 * 1024 * 1024,
  },
  cpu: {
    maxSliceMs: 8,
    maxInstructionsPerSlice: 250_000,
    backgroundThrottleMs: 50,
  },
  autoSaveSnapshotOnCrash: false,
});

function mergeConfig(base, overrides) {
  return {
    ...base,
    ...overrides,
    limits: { ...base.limits, ...(overrides?.limits ?? {}) },
    cpu: { ...base.cpu, ...(overrides?.cpu ?? {}) },
  };
}

class FakeCpu {
  constructor() {
    this.pc = 0;
    this.totalInstructions = 0;
    this.reg0 = 0;
  }

  executeSlice({ maxInstructions, deadlineMs }) {
    let executed = 0;
    while (executed < maxInstructions && nowMs() < deadlineMs) {
      this.reg0 = (this.reg0 + 1) | 0;
      this.pc = (this.pc + 1) >>> 0;
      executed += 1;
    }
    this.totalInstructions += executed;
    return { executed, didIo: false, didInterrupt: false };
  }
}

let initialized = false;
let config = DEFAULT_CONFIG;
const cpu = new FakeCpu();

let guestRam = null;
let diskCache = null;
let shaderCache = null;

// Optional SharedArrayBuffer microphone ring buffer (producer: AudioWorklet, consumer: this worker).
let micHeader = null;
let micSamples = null;
let micCapacity = 0;
let micTmp = null;
let micSampleRate = 0;
let micLastRms = 0;
let micLastDropped = 0;

let mode = "cooperativeInfiniteLoop";
let running = false;
let paused = true;
let stopping = false;
let backgrounded = false;
let stepsRemaining = 0;
let wake = null;
let loopStarted = false;

function snapshot(reason) {
  return {
    reason,
    capturedAt: Date.now(),
    cpu: {
      pc: cpu.pc,
      totalInstructions: cpu.totalInstructions,
      reg0: cpu.reg0,
    },
    resources: {
      guestRamBytes: guestRam?.byteLength ?? 0,
      diskCacheBytes: diskCache?.bytes ?? 0,
      shaderCacheBytes: shaderCache?.bytes ?? 0,
    },
  };
}

function post(msg) {
  ctx.postMessage(msg);
}

function attachMicRingBuffer(ringBuffer, sampleRate) {
  const prevSab = micHeader?.buffer ?? null;
  if (ringBuffer === null || ringBuffer === undefined) {
    micHeader = null;
    micSamples = null;
    micCapacity = 0;
    micTmp = null;
    micSampleRate = 0;
    micLastRms = 0;
    micLastDropped = 0;
    return;
  }
  const Sab = globalThis.SharedArrayBuffer;
  if (typeof Sab === "undefined" || !(ringBuffer instanceof Sab)) {
    throw new EmulatorError(ErrorCode.InvalidConfig, "mic ringBuffer must be a SharedArrayBuffer or null.");
  }

  const isNewAttach = prevSab !== ringBuffer;

  micHeader = new Uint32Array(ringBuffer, 0, MIC_HEADER_U32_LEN);
  micSamples = new Float32Array(ringBuffer, MIC_HEADER_BYTES);
  micCapacity = Atomics.load(micHeader, MIC_CAPACITY_SAMPLES_INDEX) >>> 0;
  if (!micCapacity) micCapacity = micSamples.length;
  if (micCapacity !== micSamples.length) {
    // Be permissive: clamp to whichever is smaller to avoid OOB reads.
    micCapacity = Math.min(micCapacity, micSamples.length);
  }

  micTmp = new Float32Array(1024);
  micSampleRate = (sampleRate ?? 0) | 0;
  micLastRms = 0;
  micLastDropped = 0;

  // The AudioWorklet microphone producer can start writing before (or while) this worker is
  // attached as the consumer. To avoid replaying stale samples (large perceived latency),
  // discard any buffered data when attaching by advancing readPos := writePos.
  if (isNewAttach) {
    try {
      const writePos = Atomics.load(micHeader, MIC_WRITE_POS_INDEX) >>> 0;
      Atomics.store(micHeader, MIC_READ_POS_INDEX, writePos);
    } catch {
      // ignore
    }
  }
}

function consumeMicSamples() {
  if (!micHeader || !micSamples || !micTmp || micCapacity === 0) return;

  const writePos = Atomics.load(micHeader, MIC_WRITE_POS_INDEX) >>> 0;
  const readPos = Atomics.load(micHeader, MIC_READ_POS_INDEX) >>> 0;
  const available = (writePos - readPos) >>> 0;
  if (available === 0) {
    micLastRms = 0;
    micLastDropped = Atomics.load(micHeader, MIC_DROPPED_SAMPLES_INDEX) >>> 0;
    return;
  }

  const toRead = Math.min(available, micTmp.length);
  const start = readPos % micCapacity;
  const firstPart = Math.min(toRead, micCapacity - start);

  micTmp.set(micSamples.subarray(start, start + firstPart), 0);
  const remaining = toRead - firstPart;
  if (remaining) {
    micTmp.set(micSamples.subarray(0, remaining), firstPart);
  }

  Atomics.store(micHeader, MIC_READ_POS_INDEX, (readPos + toRead) >>> 0);

  let sumSq = 0;
  for (let i = 0; i < toRead; i++) {
    const s = micTmp[i];
    sumSq += s * s;
  }
  micLastRms = Math.sqrt(sumSq / toRead);
  micLastDropped = Atomics.load(micHeader, MIC_DROPPED_SAMPLES_INDEX) >>> 0;
}

function fatal(err) {
  const structured = serializeError(err);
  const payload = { type: "error", error: structured };
  if (config.autoSaveSnapshotOnCrash) payload.snapshot = snapshot("crash");
  try {
    post(payload);
  } finally {
    try {
      ctx.close();
    } catch {
      // ignore
    }
  }
}

ctx.addEventListener("error", (event) => {
  event.preventDefault?.();
  fatal(event.error ?? new Error(event.message));
});
ctx.addEventListener("unhandledrejection", (event) => {
  event.preventDefault?.();
  fatal(event.reason);
});

function initResources() {
  if (config.guestRamBytes > config.limits.maxGuestRamBytes) {
    throw resourceLimitExceeded({
      resource: "guest RAM",
      requestedBytes: config.guestRamBytes,
      maxBytes: config.limits.maxGuestRamBytes,
    });
  }

  try {
    guestRam = new ArrayBuffer(config.guestRamBytes);
  } catch (err) {
    throw outOfMemory({ resource: "guest RAM", attemptedBytes: config.guestRamBytes, cause: err });
  }

  diskCache = new SizedLruCache({ maxBytes: config.limits.maxDiskCacheBytes, name: "disk cache" });
  shaderCache = new SizedLruCache({ maxBytes: config.limits.maxShaderCacheBytes, name: "shader cache" });
}

async function waitForWake() {
  if (!paused && !stopping) return;
  await new Promise((resolve) => {
    wake = resolve;
  });
}

function wakeLoop() {
  if (wake) {
    const resolve = wake;
    wake = null;
    resolve();
  }
}

async function runLoop() {
  if (loopStarted) return;
  loopStarted = true;

  try {
    while (!stopping) {
      if (!running || paused) {
        await waitForWake();
        continue;
      }

      const sliceStart = nowMs();
      const deadline = sliceStart + config.cpu.maxSliceMs;
      const maxInstructions = stepsRemaining > 0 ? 1 : config.cpu.maxInstructionsPerSlice;
      const { executed } = cpu.executeSlice({ maxInstructions, deadlineMs: deadline });

      consumeMicSamples();

      post({
        type: "heartbeat",
        at: Date.now(),
        executed,
        totalInstructions: cpu.totalInstructions,
        pc: cpu.pc,
        resources: {
          guestRamBytes: guestRam?.byteLength ?? 0,
          diskCacheBytes: diskCache?.bytes ?? 0,
          shaderCacheBytes: shaderCache?.bytes ?? 0,
        },
        mic: micHeader
          ? {
              rms: micLastRms,
              dropped: micLastDropped,
              sampleRate: micSampleRate,
            }
          : null,
      });

      if (stepsRemaining > 0) {
        stepsRemaining -= 1;
        paused = true;
        post({ type: "stepped", executed, snapshot: snapshot("step") });
        post({ type: "paused" });
      }

      await yieldToEventLoop();

      if (backgrounded && config.cpu.backgroundThrottleMs > 0) {
        await sleep(config.cpu.backgroundThrottleMs);
      }
    }

    post({ type: "shutdownAck" });
    ctx.close();
  } catch (err) {
    fatal(err);
  }
}

ctx.onmessage = (ev) => {
  const msg = ev.data;
  try {
    if (!msg || typeof msg !== "object") {
      throw new EmulatorError(ErrorCode.InvalidConfig, "Invalid message sent to CPU worker.");
    }

    if (!initialized) {
      if (msg.type !== "init") {
        throw new EmulatorError(ErrorCode.InvalidConfig, "CPU worker must be initialized before use.");
      }
      config = mergeConfig(DEFAULT_CONFIG, msg.config ?? {});
      initResources();
      initialized = true;
      post({ type: "ready", config: { ...config, guestRamBytes: guestRam.byteLength } });
      return;
    }

    switch (msg.type) {
      case "start": {
        if (running) break;
        mode = msg.mode ?? "cooperativeInfiniteLoop";
        running = true;
        paused = false;
        post({ type: "started", mode });

        if (mode === "nonYieldingLoop") {
          // Simulate a hung CPU worker that never yields back to the event loop.
          //
          // Prefer an `Atomics.wait`-based block when SharedArrayBuffer is available so we don't
          // peg a CPU core (which can make local debugging and automated tests noisy).
          //
          // When SharedArrayBuffer is unavailable (e.g. crossOriginIsolated=false), we can't
          // truly block the worker thread without burning CPU. In that case, simulate a hang by
          // never starting the execution loop (no heartbeats), which is sufficient to trip the
          // watchdog in the coordinator.
          const Sab = globalThis.SharedArrayBuffer;
          if (
            typeof Sab !== "undefined" &&
            typeof Atomics !== "undefined" &&
            typeof Atomics.wait === "function"
          ) {
            try {
              const buf = new Sab(4);
              const int32 = new Int32Array(buf);
              // Loop to tolerate spurious wakeups while remaining unresponsive.
              // eslint-disable-next-line no-constant-condition
              while (true) Atomics.wait(int32, 0, 0, 60_000);
            } catch {
              // Fall through to the no-heartbeats simulation below.
            }
          }
          break;
        }

        if (mode === "crash") {
          throw new EmulatorError(ErrorCode.InternalError, "Simulated CPU crash.");
        }

        void runLoop();
        wakeLoop();
        break;
      }
      case "cacheWrite": {
        const requestId = typeof msg.requestId === "number" ? msg.requestId : 0;
        const cache = msg.cache === "disk" ? diskCache : msg.cache === "shader" ? shaderCache : null;
        if (!cache) {
          post({
            type: "cacheWriteResult",
            requestId,
            ok: false,
            cache: msg.cache ?? null,
            error: serializeError(new EmulatorError(ErrorCode.InvalidConfig, "Unknown cache selector.")),
          });
          break;
        }

        const sizeBytes = Number(msg.sizeBytes);
        if (!Number.isFinite(sizeBytes) || sizeBytes < 0) {
          post({
            type: "cacheWriteResult",
            requestId,
            ok: false,
            cache: msg.cache,
            error: serializeError(new EmulatorError(ErrorCode.InvalidConfig, "Invalid cache entry size.")),
          });
          break;
        }

        const key =
          typeof msg.key === "string" && msg.key.length > 0
            ? msg.key
            : `${msg.cache}:${Date.now()}:${Math.random().toString(16).slice(2)}`;

        try {
          cache.set(key, { cachedAt: Date.now() }, sizeBytes);
          post({
            type: "cacheWriteResult",
            requestId,
            ok: true,
            cache: msg.cache,
            stats: {
              diskCacheBytes: diskCache?.bytes ?? 0,
              shaderCacheBytes: shaderCache?.bytes ?? 0,
            },
          });
        } catch (err) {
          post({
            type: "cacheWriteResult",
            requestId,
            ok: false,
            cache: msg.cache,
            stats: {
              diskCacheBytes: diskCache?.bytes ?? 0,
              shaderCacheBytes: shaderCache?.bytes ?? 0,
            },
            error: serializeError(err),
          });
        }
        break;
      }
      case "pause": {
        if (!running) break;
        paused = true;
        post({ type: "paused" });
        break;
      }
      case "resume": {
        if (!running) break;
        paused = false;
        // The host-side microphone producer can continue writing into the ring buffer while the
        // VM is paused. Discard any buffered samples on resume so capture/loopback starts from
        // the most recent audio rather than replaying a stale backlog.
        if (micHeader) {
          try {
            const writePos = Atomics.load(micHeader, MIC_WRITE_POS_INDEX) >>> 0;
            Atomics.store(micHeader, MIC_READ_POS_INDEX, writePos);
          } catch {
            // ignore
          }
        }
        post({ type: "resumed" });
        wakeLoop();
        break;
      }
      case "step": {
        if (!running) break;
        stepsRemaining += 1;
        paused = false;
        // Treat stepping like a resume boundary for mic capture: if the worker was paused for a
        // while, discard any buffered samples so the next tick observes current audio.
        if (micHeader) {
          try {
            const writePos = Atomics.load(micHeader, MIC_WRITE_POS_INDEX) >>> 0;
            Atomics.store(micHeader, MIC_READ_POS_INDEX, writePos);
          } catch {
            // ignore
          }
        }
        post({ type: "stepping" });
        wakeLoop();
        break;
      }
      case "setBackgrounded": {
        backgrounded = Boolean(msg.backgrounded);
        break;
      }
      case "setMicrophoneRingBuffer": {
        attachMicRingBuffer(msg.ringBuffer ?? null, msg.sampleRate);
        break;
      }
      case "requestSnapshot": {
        post({ type: "snapshot", snapshot: snapshot(msg.reason ?? "manual") });
        break;
      }
      case "shutdown": {
        stopping = true;
        paused = false;
        wakeLoop();
        break;
      }
      default:
        throw new EmulatorError(ErrorCode.InvalidConfig, `Unknown command: ${msg.type}`);
    }
  } catch (err) {
    fatal(err);
  }
};
