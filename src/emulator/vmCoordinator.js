import { ErrorCode, serializeError } from "../errors.js";

const DEFAULT_CONFIG = Object.freeze({
  cpu: {
    watchdogTimeoutMs: 2000,
  },
  autoSaveSnapshotOnCrash: false,
});

const LOCAL_STORAGE_CRASH_SNAPSHOT_KEY = "aero:lastCrashSnapshot";

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
    const handle = await dir.getFileHandle("last-crash-snapshot.json", { create: true });
    const writable = await handle.createWritable();
    await writable.write(safeJsonStringify(snapshot, 2));
    await writable.close();
    return "opfs:state/last-crash-snapshot.json";
  } catch {
    return null;
  }
}

export class VmCoordinator extends EventTarget {
  constructor({ config = {}, workerUrl = new URL("./cpu.worker.js", import.meta.url) } = {}) {
    super();
    this.config = mergeConfig(DEFAULT_CONFIG, config);
    this.workerUrl = workerUrl;
    this.state = "stopped";
    this.worker = null;
    this.lastHeartbeatAt = 0;
    this.lastHeartbeat = null;
    this.lastSnapshot = null;
    this.lastSnapshotSavedTo = null;

    this._ackQueues = new Map();
    this._watchdogTimer = null;
    this._terminated = false;
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

    const worker = new Worker(this.workerUrl, { type: "module" });
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
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to initialize.",
    });

    this._startWatchdog();
    this._send({ type: "start", mode });

    await this._awaitAck("started", {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to start.",
    });

    this._setState("running");
  }

  async pause() {
    if (!this.worker) return;
    if (this.state !== "running") return;
    this._send({ type: "pause" });
    await this._awaitAck("paused", {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to pause.",
    });
    this._setState("paused");
  }

  async resume() {
    if (!this.worker) return;
    if (this.state !== "paused") return;
    this._send({ type: "resume" });
    await this._awaitAck("resumed", {
      timeoutMs: 2000,
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
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to step.",
    });
    await this._awaitAck("paused", {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to pause after step.",
    });
    this._setState("paused");
  }

  setBackgrounded(backgrounded) {
    if (!this.worker) return;
    this._send({ type: "setBackgrounded", backgrounded: Boolean(backgrounded) });
  }

  async requestSnapshot({ reason = "manual" } = {}) {
    if (!this.worker) return null;
    this._send({ type: "requestSnapshot", reason });
    const msg = await this._awaitAck("snapshot", {
      timeoutMs: 2000,
      message: "Timed out waiting for snapshot.",
    });
    return msg.snapshot;
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

    if (msg.type === "heartbeat") {
      this.lastHeartbeatAt = Date.now();
      this.lastHeartbeat = msg;
      this.lastSnapshot = {
        reason: "heartbeat",
        capturedAt: msg.at,
        cpu: { pc: msg.pc, totalInstructions: msg.totalInstructions },
      };
      this.dispatchEvent(new CustomEvent("heartbeat", { detail: msg }));
      return;
    }

    if (msg.type === "error") {
      if (msg.snapshot) this.lastSnapshot = msg.snapshot;
      this._emitError(msg.error, { snapshot: msg.snapshot });
      return;
    }

    if (msg.type === "snapshot") {
      this.lastSnapshot = msg.snapshot;
    }

    const queue = this._ackQueues.get(msg.type);
    if (queue && queue.length > 0) {
      const next = queue.shift();
      next.resolve(msg);
      return;
    }
  }

  _onWorkerError(event) {
    const err = event.error instanceof Error ? event.error : new Error(event.message);
    this._emitError(
      serializeError(err, {
        code: ErrorCode.WorkerCrashed,
        message: "CPU worker crashed.",
      }),
    );
  }

  _startWatchdog() {
    this._stopWatchdog();
    const intervalMs = Math.max(25, Math.floor(this.config.cpu.watchdogTimeoutMs / 4));
    this._watchdogTimer = setInterval(() => {
      if (this.state !== "running") return;
      const elapsed = Date.now() - this.lastHeartbeatAt;
      if (elapsed <= this.config.cpu.watchdogTimeoutMs) return;

      this._emitError({
        code: ErrorCode.WatchdogTimeout,
        message: `CPU worker became unresponsive (no heartbeat for ${elapsed}ms).`,
        details: { elapsedMs: elapsed, watchdogTimeoutMs: this.config.cpu.watchdogTimeoutMs },
        suggestion: "The worker was terminated to keep the UI responsive. Reset the VM to continue.",
      });
    }, intervalMs);
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
    const structured = error && typeof error === "object" ? error : { code: ErrorCode.InternalError, message: String(error) };
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
          this.dispatchEvent(new CustomEvent("snapshotSaved", { detail: { savedTo } }));
        });
      }
    }
    this.dispatchEvent(new CustomEvent("error", { detail: { error: structured, snapshot } }));
    this._rejectAllAcks(new Error(structured.message));
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
    this.dispatchEvent(new CustomEvent("statechange", { detail: { state: next } }));
  }
}
