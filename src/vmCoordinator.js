import { Worker } from 'node:worker_threads';
import { ErrorCode, serializeError } from './errors.js';
import { createCustomEvent } from './custom_event.js';
import { tryGetNumberProp, tryGetProp, tryGetStringProp } from './safe_props.js';
import { formatOneLineError, formatOneLineUtf8 } from './text.js';
import { unrefBestEffort } from './unref_safe.js';

const DEFAULT_CONFIG = Object.freeze({
  cpu: {
    watchdogTimeoutMs: 2000,
    ackTimeoutMs: 10_000,
  },
  autoSaveSnapshotOnCrash: false,
});

const MAX_COORDINATOR_ERROR_MESSAGE_BYTES = 512;
const MAX_COORDINATOR_ERROR_NAME_BYTES = 128;

function mergeConfig(base, overrides) {
  return {
    ...base,
    ...overrides,
    cpu: { ...base.cpu, ...(overrides?.cpu ?? {}) },
  };
}

export class VmCoordinator extends EventTarget {
  constructor({ config = {}, workerUrl = new URL('./cpuWorker.js', import.meta.url) } = {}) {
    super();
    this.config = mergeConfig(DEFAULT_CONFIG, config);
    this.workerUrl = workerUrl;
    this.state = 'stopped';
    this.worker = null;
    this.lastHeartbeatAt = 0;
    this.lastHeartbeat = null;
    this.lastSnapshot = null;

    this._ackQueues = new Map();
    this._watchdogTimer = null;
    this._terminated = false;
    this._nextRequestId = 1;
  }

  async start({ mode = 'cooperativeInfiniteLoop' } = {}) {
    if (this.worker) throw new Error('VM already started');

    this._setState('starting');
    this._terminated = false;
    this.lastHeartbeatAt = Date.now();

    this.worker = new Worker(this.workerUrl, {
      type: 'module',
      workerData: { config: this.config },
    });

    this.worker.on('message', (msg) => this._onWorkerMessage(msg));
    this.worker.on('error', (err) => this._onWorkerError(err));
    this.worker.on('exit', (code) => this._onWorkerExit(code));

    await this._awaitAck('ready', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to initialize.',
    });

    this._startWatchdog();
    this._send({ type: 'start', mode });

    await this._awaitAck('started', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to start.',
    });

    this.lastHeartbeatAt = Date.now();
    this._setState('running');
  }

  async pause() {
    if (!this.worker) return;
    if (this.state !== 'running') return;
    this._send({ type: 'pause' });
    await this._awaitAck('paused', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to pause.',
    });
    this._setState('paused');
  }

  async resume() {
    if (!this.worker) return;
    if (this.state !== 'paused') return;
    this._send({ type: 'resume' });
    await this._awaitAck('resumed', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to resume.',
    });
    this.lastHeartbeatAt = Date.now();
    this._setState('running');
  }

  async step() {
    if (!this.worker) return;
    if (this.state !== 'paused') return;
    this._send({ type: 'step' });
    await this._awaitAck('stepped', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to step.',
    });
    await this._awaitAck('paused', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for CPU worker to pause after step.',
    });
    this._setState('paused');
  }

  setBackgrounded(backgrounded) {
    if (!this.worker) return;
    this._send({ type: 'setBackgrounded', backgrounded: Boolean(backgrounded) });
  }

  async requestSnapshot({ reason = 'manual' } = {}) {
    if (!this.worker) return null;
    this._send({ type: 'requestSnapshot', reason });
    const msg = await this._awaitAck('snapshot', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for snapshot.',
    });
    return msg.snapshot;
  }

  async writeCacheEntry({ cache, sizeBytes, key } = {}) {
    if (!this.worker) throw new Error('VM is not running');
    const requestId = this._nextRequestId++;
    this._send({ type: 'cacheWrite', requestId, cache, sizeBytes, key });
    const msg = await this._awaitAck('cacheWriteResult', {
      timeoutMs: this.config.cpu.ackTimeoutMs,
      message: 'Timed out waiting for cache write result.',
    });
    if (msg?.requestId !== requestId) {
      throw new Error('Received mismatched cache write response.');
    }
    return msg;
  }

  async shutdown() {
    if (!this.worker) return;
    const worker = this.worker;
    this._send({ type: 'shutdown' });
    this._onTerminated();
    await worker.terminate();
    this._setState('stopped');
  }

  async reset() {
    try {
      await this.shutdown();
    } catch {
      this._onTerminated();
    } finally {
      this._setState('stopped');
    }
  }

  _send(msg) {
    if (!this.worker) return;
    this.worker.postMessage(msg);
  }

  _onWorkerMessage(msg) {
    if (!msg || typeof msg !== 'object') return;

    const type = tryGetStringProp(msg, 'type');
    if (!type) return;

    if (type === 'heartbeat') {
      this.lastHeartbeatAt = Date.now();
      const resources = tryGetProp(msg, 'resources');
      const safeResources =
        resources && typeof resources === 'object'
          ? {
              guestRamBytes: tryGetNumberProp(resources, 'guestRamBytes') ?? 0,
              diskCacheBytes: tryGetNumberProp(resources, 'diskCacheBytes') ?? 0,
              shaderCacheBytes: tryGetNumberProp(resources, 'shaderCacheBytes') ?? 0,
            }
          : { guestRamBytes: 0, diskCacheBytes: 0, shaderCacheBytes: 0 };

      const safeHeartbeat = {
        type: 'heartbeat',
        at: tryGetNumberProp(msg, 'at') ?? Date.now(),
        executed: tryGetNumberProp(msg, 'executed') ?? 0,
        totalInstructions: tryGetNumberProp(msg, 'totalInstructions') ?? 0,
        pc: tryGetNumberProp(msg, 'pc') ?? 0,
        resources: safeResources,
      };

      this.lastHeartbeat = safeHeartbeat;
      this.lastSnapshot = {
        reason: 'heartbeat',
        capturedAt: safeHeartbeat.at,
        cpu: { pc: safeHeartbeat.pc, totalInstructions: safeHeartbeat.totalInstructions },
        resources: safeResources,
      };
      this.dispatchEvent(createCustomEvent('heartbeat', safeHeartbeat));
      return;
    }

    if (type === 'error') {
      const snapshot = tryGetProp(msg, 'snapshot');
      if (snapshot) this.lastSnapshot = snapshot;
      this._emitError(tryGetProp(msg, 'error'), { snapshot });
      return;
    }

    if (type === 'snapshot') {
      const snapshot = tryGetProp(msg, 'snapshot');
      if (snapshot) this.lastSnapshot = snapshot;
    }

    const queue = this._ackQueues.get(type);
    if (queue && queue.length > 0) {
      const next = queue.shift();
      next.resolve(msg);
      return;
    }
  }

  _onWorkerError(err) {
    this._emitError(
      serializeError(err, {
        code: ErrorCode.WorkerCrashed,
        message: 'CPU worker crashed.',
      }),
    );
  }

  _onWorkerExit(code) {
    if (this._terminated) return;
    if (code === 0) {
      this._onTerminated();
      this._setState('stopped');
      return;
    }

    this._emitError({
      code: ErrorCode.WorkerCrashed,
      message: `CPU worker exited unexpectedly (code ${code}).`,
      details: { code },
      suggestion: 'Reset the VM and try again.',
    });
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
    if (this.state !== 'running') return false;
    const elapsed = Date.now() - this.lastHeartbeatAt;
    if (elapsed <= this.config.cpu.watchdogTimeoutMs) return false;

    this._emitError({
      code: ErrorCode.WatchdogTimeout,
      message: `CPU worker became unresponsive (no heartbeat for ${elapsed}ms).`,
      details: { elapsedMs: elapsed, watchdogTimeoutMs: this.config.cpu.watchdogTimeoutMs },
      suggestion: 'The worker was terminated to keep the UI responsive. Reset the VM to continue.',
    });
    return true;
  }

  _stopWatchdog() {
    if (!this._watchdogTimer) return;
    clearInterval(this._watchdogTimer);
    this._watchdogTimer = null;
  }

  _waitForAck(type) {
    return this._waitForAckWithOptions(type, undefined);
  }

  _waitForAckWithOptions(type, options) {
    const timeoutMs = options?.timeoutMs ?? undefined;
    const message = options?.message ?? 'Timed out waiting for worker response.';

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

  async _awaitAck(type, options) {
    try {
      return await this._waitForAckWithOptions(type, options);
    } catch (err) {
      if (this.state !== 'error') {
        this._emitError(
          {
            code: ErrorCode.WorkerCrashed,
            message: options?.message ?? 'Worker became unresponsive.',
            details: { ackType: type },
            suggestion: 'The worker was terminated to keep the UI responsive. Reset the VM to continue.',
          },
          { snapshot: this.lastSnapshot },
        );
      }
      throw err;
    }
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
    const errObj = error && typeof error === 'object' ? error : null;

    let code = ErrorCode.InternalError;
    let name = 'Error';
    let message = formatOneLineError(error, MAX_COORDINATOR_ERROR_MESSAGE_BYTES);
    let details = undefined;
    let suggestion = undefined;

    if (errObj) {
      const maybeCode = tryGetStringProp(errObj, 'code');
      if (maybeCode) code = maybeCode;
      const maybeName = tryGetStringProp(errObj, 'name');
      if (maybeName) name = maybeName;
      const maybeMessage = tryGetStringProp(errObj, 'message');
      if (maybeMessage) {
        message = formatOneLineUtf8(maybeMessage, MAX_COORDINATOR_ERROR_MESSAGE_BYTES) || 'Error';
      }

      details = tryGetProp(errObj, 'details');
      suggestion = tryGetProp(errObj, 'suggestion');
    }

    const safeMessage = formatOneLineUtf8(message, MAX_COORDINATOR_ERROR_MESSAGE_BYTES) || 'Error';
    const safeName = formatOneLineUtf8(name, MAX_COORDINATOR_ERROR_NAME_BYTES) || 'Error';

    const structured = {
      name: safeName,
      code,
      message: safeMessage,
      ...(details !== undefined ? { details } : {}),
      ...(suggestion !== undefined ? { suggestion } : {}),
    };
    if (this.config.autoSaveSnapshotOnCrash) this.lastSnapshot = snapshot ?? this.lastSnapshot;
    this.dispatchEvent(createCustomEvent('error', { error: structured, snapshot }));
    this._rejectAllAcks(new Error(safeMessage));
    this._stopWatchdog();
    const worker = this.worker;
    this._onTerminated();
    worker?.terminate();
    if (this.state !== 'error') this._setState('error');
  }

  _onTerminated() {
    if (this._terminated) return;
    this._terminated = true;
    this._stopWatchdog();
    if (this.worker) {
      this.worker.removeAllListeners?.();
      this.worker = null;
    }
  }

  _setState(next) {
    if (this.state === next) return;
    this.state = next;
    this.dispatchEvent(createCustomEvent('statechange', { state: next }));
  }
}
