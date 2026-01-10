import { ErrorCode, serializeError } from "../errors.js";

const DEFAULT_CONFIG = Object.freeze({
  cpu: {
    watchdogTimeoutMs: 2000,
  },
  autoSaveSnapshotOnCrash: false,
});

function mergeConfig(base, overrides) {
  return {
    ...base,
    ...overrides,
    cpu: { ...base.cpu, ...(overrides?.cpu ?? {}) },
  };
}

function withTimeout(promise, { timeoutMs, message }) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), timeoutMs);
  });

  const wrapped = promise.then(
    (value) => {
      clearTimeout(timer);
      return value;
    },
    (err) => {
      clearTimeout(timer);
      throw err;
    },
  );

  return Promise.race([wrapped, timeout]);
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

    this._ackQueues = new Map();
    this._watchdogTimer = null;
    this._terminated = false;
  }

  async start({ mode = "cooperativeInfiniteLoop" } = {}) {
    if (this.worker) throw new Error("VM already started");

    this._setState("starting");
    this._terminated = false;
    this.lastHeartbeatAt = Date.now();

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

    await withTimeout(this._waitForAck("ready"), {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to initialize.",
    });

    this._startWatchdog();
    this._send({ type: "start", mode });

    await withTimeout(this._waitForAck("started"), {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to start.",
    });

    this._setState("running");
  }

  async pause() {
    if (!this.worker) return;
    if (this.state !== "running") return;
    this._send({ type: "pause" });
    await withTimeout(this._waitForAck("paused"), {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to pause.",
    });
    this._setState("paused");
  }

  async resume() {
    if (!this.worker) return;
    if (this.state !== "paused") return;
    this._send({ type: "resume" });
    await withTimeout(this._waitForAck("resumed"), {
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
    await withTimeout(this._waitForAck("stepped"), {
      timeoutMs: 2000,
      message: "Timed out waiting for CPU worker to step.",
    });
    await withTimeout(this._waitForAck("paused"), {
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
    const msg = await withTimeout(this._waitForAck("snapshot"), {
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

  _waitForAck(type) {
    return new Promise((resolve, reject) => {
      const queue = this._ackQueues.get(type) ?? [];
      queue.push({ resolve, reject });
      this._ackQueues.set(type, queue);
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
    if (this.config.autoSaveSnapshotOnCrash) this.lastSnapshot = snapshot ?? this.lastSnapshot;
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

