import { ErrorCode, serializeError } from "../errors.js";
import { createCustomEvent } from "../custom_event.js";
import { isInstanceOfSafe } from "../instanceof_safe.js";
import { tryGetNumberProp, tryGetProp, tryGetStringProp } from "../safe_props.js";
import { formatOneLineError } from "../text.js";
import { unrefBestEffort } from "../unref_safe.js";

const DEFAULT_CONFIG = Object.freeze({
  cpu: {
    watchdogTimeoutMs: 2000,
    ackTimeoutMs: 10_000,
  },
  autoSaveSnapshotOnCrash: false,
});

const LOCAL_STORAGE_CRASH_SNAPSHOT_KEY = "aero:lastCrashSnapshot";
const OPFS_CRASH_SNAPSHOT_FILE = "last-crash-snapshot.json";

function mergeConfig(base, overrides) {
  return {
    ...base,
    ...overrides,
    cpu: { ...base.cpu, ...(overrides?.cpu ?? {}) },
  };
}

function safeJsonStringify(value, space) {
  return JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v), space);
}

async function trySaveSnapshotToOpfs(snapshot) {
  try {
    const { getOpfsStateDir } = await import("../platform/opfs");
    const dir = await getOpfsStateDir();
    const handle = await dir.getFileHandle(OPFS_CRASH_SNAPSHOT_FILE, { create: true });
    let writable = null;
    let truncateFallback = false;
    try {
      writable = await handle.createWritable({ keepExistingData: false });
    } catch {
      // Some implementations may not accept options; fall back to default.
      writable = await handle.createWritable();
      truncateFallback = true;
    }
    if (truncateFallback) {
      try {
        if (writable && typeof writable.truncate === "function") await writable.truncate(0);
      } catch {
        // ignore
      }
    }
    try {
      await writable.write(safeJsonStringify(snapshot, 2));
      await writable.close();
    } catch (err) {
      // Abort on error so a failed write does not leave behind a truncated/partial crash snapshot.
      try {
        if (writable && typeof writable.abort === "function") await writable.abort(err);
      } catch {
        // ignore
      }
      throw err;
    }
    return `opfs:state/${OPFS_CRASH_SNAPSHOT_FILE}`;
  } catch {
    return null;
  }
}

async function tryLoadSnapshotFromOpfs() {
  try {
    const { getOpfsStateDir } = await import("../platform/opfs");
    const dir = await getOpfsStateDir();
    const handle = await dir.getFileHandle(OPFS_CRASH_SNAPSHOT_FILE);
    const file = await handle.getFile();
    const text = await file.text();
    return {
      savedTo: `opfs:state/${OPFS_CRASH_SNAPSHOT_FILE}`,
      snapshot: JSON.parse(text),
    };
  } catch {
    return null;
  }
}

async function tryDeleteSnapshotFromOpfs() {
  try {
    const { getOpfsStateDir } = await import("../platform/opfs");
    const dir = await getOpfsStateDir();
    await dir.removeEntry(OPFS_CRASH_SNAPSHOT_FILE);
    return true;
  } catch {
    return false;
  }
}

export class VmCoordinator extends EventTarget {
  constructor({ config = {}, workerUrl } = {}) {
    super();
    this.config = mergeConfig(DEFAULT_CONFIG, config);
    // Optional override for the CPU worker script URL.
    //
    // Note: when `workerUrl` is unset, `start()` constructs the worker using
    // `new Worker(new URL("./cpu.worker.js", import.meta.url), ...)` so that Vite can
    // statically detect and bundle the worker and its dependencies for production
    // builds. See `docs/` and Vite worker docs for details.
    this.workerUrl = workerUrl ?? null;
    this.state = "stopped";
    this.worker = null;
    this.lastHeartbeatAt = 0;
    this.lastHeartbeat = null;
    this.lastSnapshot = null;
    this.lastSnapshotSavedTo = null;
    // Most recent error emitted by the coordinator. This is a convenience for UIs/tests that
    // want to inspect the underlying failure (stack trace, details, etc.) without wiring an
    // explicit event listener.
    this.lastError = null;

    this._ackQueues = new Map();
    this._watchdogTimer = null;
    this._terminated = false;
    this._nextRequestId = 1;

    // Optional SharedArrayBuffer-backed microphone ring buffer, set by the UI.
    // This is forwarded to the worker so it can consume mic samples (or in the
    // real emulator, feed them into the guest's capture device model).
    // IMPORTANT: `_micSampleRate` is the *actual* capture sample rate
    // (AudioContext.sampleRate), not the requested rate.
    this._micRingBuffer = null;
    this._micSampleRate = 0;
  }

  static async loadSavedCrashSnapshot() {
    const opfs = await tryLoadSnapshotFromOpfs();
    if (opfs) return opfs;

    try {
      if (typeof localStorage === "undefined") return null;
      const raw = localStorage.getItem(LOCAL_STORAGE_CRASH_SNAPSHOT_KEY);
      if (!raw) return null;
      return {
        savedTo: `localStorage:${LOCAL_STORAGE_CRASH_SNAPSHOT_KEY}`,
        snapshot: JSON.parse(raw),
      };
    } catch {
      return null;
    }
  }

  static async clearSavedCrashSnapshot() {
    try {
      if (typeof localStorage !== "undefined") localStorage.removeItem(LOCAL_STORAGE_CRASH_SNAPSHOT_KEY);
    } catch {
      // ignore
    }
    await tryDeleteSnapshotFromOpfs();
  }

  async _awaitAck(type, options) {
    try {
      return await this._waitForAck(type, options);
    } catch (err) {
      if (this.state !== "error") {
        this._emitError(
          {
            code: ErrorCode.WorkerCrashed,
            message: options?.message ?? "Worker became unresponsive.",
            details: { ackType: type },
            suggestion: "The worker was terminated to keep the UI responsive. Reset the VM to continue.",
          },
          { snapshot: this.lastSnapshot },
        );
      }
      throw err;
    }
  }

  async start({ mode = "cooperativeInfiniteLoop" } = {}) {
    if (this.worker) throw new Error("VM already started");

    this._setState("starting");
    this._terminated = false;
    this.lastHeartbeatAt = Date.now();
    this.lastSnapshotSavedTo = null;

    const worker =
      this.workerUrl instanceof URL
        ? new Worker(this.workerUrl, { type: "module" })
        : new Worker(new URL("./cpu.worker.js", import.meta.url), { type: "module" });
    this.worker = worker;

    worker.onmessage = (event) => this._onWorkerMessage(event.data);
    worker.onerror = (event) => this._onWorkerError(event);
    worker.onmessageerror = () => {
      this._emitError({
        code: ErrorCode.WorkerCrashed,
        message: "CPU worker message deserialization failed.",
        suggestion: "Reset the VM and try again.",
      });
    };

    worker.postMessage({ type: "init", config: this.config });

    await this._awaitAck("ready", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to initialize.",
    });

    if (this._micRingBuffer) {
      this._send({
        type: "setMicrophoneRingBuffer",
        ringBuffer: this._micRingBuffer,
        sampleRate: this._micSampleRate,
      });
    }

    this._startWatchdog();
    this._send({ type: "start", mode });

    await this._awaitAck("started", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to start.",
    });

    this._setState("running");
  }

  async pause() {
    if (!this.worker) return;
    if (this.state !== "running") return;
    this._send({ type: "pause" });
    await this._awaitAck("paused", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to pause.",
    });
    this._setState("paused");
  }

  async resume() {
    if (!this.worker) return;
    if (this.state !== "paused") return;
    this._send({ type: "resume" });
    await this._awaitAck("resumed", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to resume.",
    });
    this.lastHeartbeatAt = Date.now();
    this._setState("running");
  }

  async step() {
    if (!this.worker) return;
    if (this.state !== "paused") return;
    this._send({ type: "step" });
    await this._awaitAck("stepped", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to step.",
    });
    await this._awaitAck("paused", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for CPU worker to pause after step.",
    });
    this._setState("paused");
  }

  setBackgrounded(backgrounded) {
    if (!this.worker) return;
    this._send({ type: "setBackgrounded", backgrounded: Boolean(backgrounded) });
  }

  setMicrophoneRingBuffer(ringBuffer, { sampleRate = 0 } = {}) {
    if (ringBuffer !== null) {
      const Sab = globalThis.SharedArrayBuffer;
      if (typeof Sab === "undefined") {
        throw new Error("SharedArrayBuffer is unavailable; microphone capture requires crossOriginIsolated.");
      }
      if (!(ringBuffer instanceof Sab)) {
        throw new Error("setMicrophoneRingBuffer expects a SharedArrayBuffer or null.");
      }
    }

    const sr = (sampleRate ?? 0) | 0;
    const changed = this._micRingBuffer !== ringBuffer || this._micSampleRate !== sr;
    this._micRingBuffer = ringBuffer;
    this._micSampleRate = sr;

    // Avoid resending identical attachments: in worker runtimes, re-attaching can flush
    // buffered mic samples (readPos := writePos), which would drop live audio and cause
    // glitches if called redundantly.
    if (!changed) return;
    this._send({ type: "setMicrophoneRingBuffer", ringBuffer, sampleRate: this._micSampleRate });
  }

  async requestSnapshot({ reason = "manual" } = {}) {
    if (!this.worker) return null;
    this._send({ type: "requestSnapshot", reason });
    const msg = await this._awaitAck("snapshot", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for snapshot.",
    });
    return msg.snapshot;
  }

  async writeCacheEntry({ cache, sizeBytes, key } = {}) {
    if (!this.worker) {
      throw new Error("VM is not running.");
    }
    const requestId = this._nextRequestId++;
    this._send({ type: "cacheWrite", requestId, cache, sizeBytes, key });
    const msg = await this._awaitAck("cacheWriteResult", {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: "Timed out waiting for cache write result.",
    });
    if (msg?.requestId !== requestId) {
      throw new Error("Received mismatched cache write response.");
    }
    return msg;
  }

  shutdown() {
    if (!this.worker) return;
    const worker = this.worker;
    this._send({ type: "shutdown" });
    this._onTerminated();
    worker.terminate();
    this._setState("stopped");
  }

  reset() {
    try {
      this.shutdown();
    } catch {
      this._onTerminated();
    } finally {
      this._setState("stopped");
    }
  }

  _send(msg) {
    if (!this.worker) return;
    this.worker.postMessage(msg);
  }

  _onWorkerMessage(msg) {
    if (!msg || typeof msg !== "object") return;

    const type = tryGetStringProp(msg, "type");
    if (!type) return;

    if (type === "heartbeat") {
      this.lastHeartbeatAt = Date.now();
      const resources = tryGetProp(msg, "resources");
      const safeResources =
        resources && typeof resources === "object"
          ? {
              guestRamBytes: tryGetNumberProp(resources, "guestRamBytes") ?? 0,
              diskCacheBytes: tryGetNumberProp(resources, "diskCacheBytes") ?? 0,
              shaderCacheBytes: tryGetNumberProp(resources, "shaderCacheBytes") ?? 0,
            }
          : { guestRamBytes: 0, diskCacheBytes: 0, shaderCacheBytes: 0 };

      const mic = tryGetProp(msg, "mic");
      const safeMic =
        mic && typeof mic === "object"
          ? {
              rms: tryGetNumberProp(mic, "rms") ?? 0,
              dropped: tryGetNumberProp(mic, "dropped") ?? 0,
              sampleRate: tryGetNumberProp(mic, "sampleRate") ?? 0,
            }
          : null;

      const safeHeartbeat = {
        type: "heartbeat",
        at: tryGetNumberProp(msg, "at") ?? Date.now(),
        executed: tryGetNumberProp(msg, "executed") ?? 0,
        pc: tryGetNumberProp(msg, "pc") ?? 0,
        totalInstructions: tryGetNumberProp(msg, "totalInstructions") ?? 0,
        resources: safeResources,
        mic: safeMic,
      };

      this.lastHeartbeat = safeHeartbeat;
      this.lastSnapshot = {
        reason: "heartbeat",
        capturedAt: safeHeartbeat.at,
        cpu: { pc: safeHeartbeat.pc, totalInstructions: safeHeartbeat.totalInstructions },
        resources: safeResources,
      };
      this.dispatchEvent(createCustomEvent("heartbeat", safeHeartbeat));
      return;
    }

    if (type === "error") {
      const snapshot = tryGetProp(msg, "snapshot");
      if (snapshot) this.lastSnapshot = snapshot;
      this._emitError(tryGetProp(msg, "error"), { snapshot });
      return;
    }

    if (type === "snapshot") {
      const snapshot = tryGetProp(msg, "snapshot");
      if (snapshot) this.lastSnapshot = snapshot;
    }

    const queue = this._ackQueues.get(type);
    if (queue && queue.length > 0) {
      const next = queue.shift();
      next.resolve(msg);
      return;
    }
  }

  _onWorkerError(event) {
    const maybeError = tryGetProp(event, "error");
    const err = isInstanceOfSafe(maybeError, Error) ? maybeError : new Error(tryGetStringProp(event, "message") ?? "Worker error");
    // Preserve the underlying error message so callers (and Playwright E2E) can
    // diagnose why the worker failed to load/execute. `worker.onerror` is often
    // the only signal for module-load failures (e.g. missing chunks, syntax
    // errors, COEP/CSP issues, etc).
    const baseMessage = formatOneLineError(err, 512);
    const message = baseMessage ? `CPU worker crashed: ${baseMessage}` : "CPU worker crashed.";
    this._emitError(
      serializeError(err, {
        code: ErrorCode.WorkerCrashed,
        message,
        details: {
          ...(baseMessage ? { workerMessage: baseMessage } : {}),
          filename: tryGetStringProp(event, "filename"),
          lineno: tryGetNumberProp(event, "lineno"),
          colno: tryGetNumberProp(event, "colno"),
        },
      }),
    );
  }

  _startWatchdog() {
    this._stopWatchdog();
    const intervalMs = Math.max(25, Math.floor(this.config.cpu.watchdogTimeoutMs / 4));
    this._watchdogTimer = setInterval(() => {
      this._checkWatchdog();
    }, intervalMs);
    unrefBestEffort(this._watchdogTimer);
  }

  _checkWatchdog() {
    if (this.state !== "running") return false;
    const elapsed = Date.now() - this.lastHeartbeatAt;
    if (elapsed <= this.config.cpu.watchdogTimeoutMs) return false;

    this._emitError({
      code: ErrorCode.WatchdogTimeout,
      message: `CPU worker became unresponsive (no heartbeat for ${elapsed}ms).`,
      details: { elapsedMs: elapsed, watchdogTimeoutMs: this.config.cpu.watchdogTimeoutMs },
      suggestion: "The worker was terminated to keep the UI responsive. Reset the VM to continue.",
    });
    return true;
  }

  _stopWatchdog() {
    if (!this._watchdogTimer) return;
    clearInterval(this._watchdogTimer);
    this._watchdogTimer = null;
  }

  _waitForAck(type, options) {
    return this._waitForAckWithOptions(type, options);
  }

  _waitForAckWithOptions(type, options) {
    const timeoutMs = options?.timeoutMs ?? undefined;
    const message = options?.message ?? "Timed out waiting for worker response.";

    return new Promise((resolve, reject) => {
      let timer = null;
      const queue = this._ackQueues.get(type) ?? [];

      const entry = {
        resolve: (value) => {
          if (timer !== null) clearTimeout(timer);
          resolve(value);
        },
        reject: (err) => {
          if (timer !== null) clearTimeout(timer);
          reject(err);
        },
      };

      queue.push(entry);
      this._ackQueues.set(type, queue);

      if (timeoutMs !== undefined) {
        timer = setTimeout(() => {
          const pendingQueue = this._ackQueues.get(type);
          if (pendingQueue) {
            const idx = pendingQueue.indexOf(entry);
            if (idx >= 0) pendingQueue.splice(idx, 1);
            if (pendingQueue.length === 0) this._ackQueues.delete(type);
          }
          entry.reject(new Error(message));
        }, timeoutMs);
        unrefBestEffort(timer);
      }
    });
  }

  _rejectAllAcks(err) {
    for (const queue of this._ackQueues.values()) {
      while (queue.length) {
        const pending = queue.shift();
        pending.reject(err);
      }
    }
    this._ackQueues.clear();
  }

  _emitError(error, { snapshot } = {}) {
    let structured = null;
    if (isInstanceOfSafe(error, Error)) structured = serializeError(error);

    let code = ErrorCode.InternalError;
    let message = "Error";
    let name = "Error";
    let details = undefined;
    let suggestion = undefined;
    let stack = undefined;

    if (structured && typeof structured === "object") {
      try {
        if (typeof structured.code === "string") code = structured.code;
      } catch {
        // ignore getters throwing
      }
      try {
        if (typeof structured.name === "string") name = structured.name;
      } catch {
        // ignore getters throwing
      }
      try {
        if (typeof structured.message === "string") message = structured.message;
      } catch {
        // ignore getters throwing
      }
      try {
        details = structured.details;
      } catch {
        details = undefined;
      }
      try {
        suggestion = structured.suggestion;
      } catch {
        suggestion = undefined;
      }
      try {
        if (typeof structured.stack === "string") stack = structured.stack;
      } catch {
        stack = undefined;
      }
    } else if (error && typeof error === "object") {
      const errObj = error;
      try {
        if (typeof errObj.code === "string") code = errObj.code;
      } catch {
        // ignore getters throwing
      }
      try {
        if (typeof errObj.name === "string") name = errObj.name;
      } catch {
        // ignore getters throwing
      }
      try {
        if (typeof errObj.message === "string") message = errObj.message;
      } catch {
        // ignore getters throwing
      }
      try {
        details = errObj.details;
      } catch {
        details = undefined;
      }
      try {
        suggestion = errObj.suggestion;
      } catch {
        suggestion = undefined;
      }
      try {
        if (typeof errObj.stack === "string") stack = errObj.stack;
      } catch {
        stack = undefined;
      }
      if (message === "Error") {
        message = formatOneLineError(errObj, 512) || "Error";
      }
    } else {
      message = formatOneLineError(error, 512) || "Error";
    }

    const safeMessage = formatOneLineError(message, 512) || "Error";
    const safeName = formatOneLineError(name, 128) || "Error";

    structured = {
      name: safeName,
      code,
      message: safeMessage,
      ...(details !== undefined ? { details } : {}),
      ...(suggestion !== undefined ? { suggestion } : {}),
      ...(stack ? { stack } : {}),
    };

    this.lastError = { error: structured, snapshot };
    if (this.config.autoSaveSnapshotOnCrash) {
      this.lastSnapshot = snapshot ?? this.lastSnapshot;
      const snapshotToSave = this.lastSnapshot;
      if (snapshotToSave) {
        try {
          if (typeof localStorage !== "undefined") {
            localStorage.setItem(LOCAL_STORAGE_CRASH_SNAPSHOT_KEY, safeJsonStringify(snapshotToSave));
            this.lastSnapshotSavedTo = `localStorage:${LOCAL_STORAGE_CRASH_SNAPSHOT_KEY}`;
          }
        } catch {
          // Ignore localStorage failures (privacy mode, quota exceeded, etc).
        }

        void trySaveSnapshotToOpfs(snapshotToSave).then((savedTo) => {
          if (!savedTo) return;
          this.lastSnapshotSavedTo = savedTo;
          this.dispatchEvent(createCustomEvent("snapshotSaved", { savedTo }));
        });
      }
    }
    this.dispatchEvent(createCustomEvent("error", { error: structured, snapshot }));
    this._rejectAllAcks(new Error(safeMessage));
    this._stopWatchdog();
    const worker = this.worker;
    this._onTerminated();
    worker?.terminate();
    if (this.state !== "error") this._setState("error");
  }

  _onTerminated() {
    if (this._terminated) return;
    this._terminated = true;
    this._stopWatchdog();
    if (this.worker) {
      this.worker.onmessage = null;
      this.worker.onerror = null;
      this.worker.onmessageerror = null;
      this.worker = null;
    }
  }

  _setState(next) {
    if (this.state === next) return;
    this.state = next;
    this.dispatchEvent(createCustomEvent("statechange", { state: next }));
  }
}
