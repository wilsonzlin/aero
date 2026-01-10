/// <reference lib="webworker" />

import { ErrorCode, EmulatorError, outOfMemory, resourceLimitExceeded, serializeError } from "../errors.js";
import { SizedLruCache } from "../resourceLimits.js";
import { nowMs, sleep, yieldToEventLoop } from "../utils.js";

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

      post({
        type: "heartbeat",
        at: Date.now(),
        executed,
        totalInstructions: cpu.totalInstructions,
        pc: cpu.pc,
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
          while (true) {}
        }

        if (mode === "crash") {
          throw new EmulatorError(ErrorCode.InternalError, "Simulated CPU crash.");
        }

        void runLoop();
        wakeLoop();
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
        post({ type: "resumed" });
        wakeLoop();
        break;
      }
      case "step": {
        if (!running) break;
        stepsRemaining += 1;
        paused = false;
        post({ type: "stepping" });
        wakeLoop();
        break;
      }
      case "setBackgrounded": {
        backgrounded = Boolean(msg.backgrounded);
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

