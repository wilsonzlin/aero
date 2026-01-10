/// <reference lib="webworker" />

import {
  FRAME_DIRTY,
  FRAME_METRICS_DROPPED_INDEX,
  FRAME_METRICS_PRESENTED_INDEX,
  FRAME_METRICS_RECEIVED_INDEX,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  type GpuWorkerMessageFromMain,
  type GpuWorkerMessageToMain,
} from '../shared/frameProtocol';

type PresentFn = () => void | boolean | Promise<void | boolean>;

const postToMain = (msg: GpuWorkerMessageToMain) => {
  self.postMessage(msg);
};

let frameState: Int32Array | null = null;

let presentFn: PresentFn | null = null;
let presenting = false;

let pendingFrames = 0;

let framesReceived = 0;
let framesPresented = 0;
let framesDropped = 0;

let lastSeenSeq = 0;
let lastPresentedSeq = 0;

let lastMetricsPostAtMs = 0;
const METRICS_POST_INTERVAL_MS = 250;

const syncSharedMetrics = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_METRICS_DROPPED_INDEX) return;

  Atomics.store(frameState, FRAME_METRICS_RECEIVED_INDEX, framesReceived);
  Atomics.store(frameState, FRAME_METRICS_PRESENTED_INDEX, framesPresented);
  Atomics.store(frameState, FRAME_METRICS_DROPPED_INDEX, framesDropped);
};

const maybePostMetrics = () => {
  const nowMs = performance.now();
  if (nowMs - lastMetricsPostAtMs < METRICS_POST_INTERVAL_MS) return;

  lastMetricsPostAtMs = nowMs;
  syncSharedMetrics();
  postToMain({
    type: 'metrics',
    framesReceived,
    framesPresented,
    framesDropped,
  });
};

const loadPresentFnFromModuleUrl = async (wasmModuleUrl: string) => {
  const mod: unknown = await import(/* @vite-ignore */ wasmModuleUrl);
  const maybePresent = (mod as { present?: unknown }).present;
  if (typeof maybePresent !== 'function') {
    throw new Error(`Module ${wasmModuleUrl} did not export a present() function`);
  }

  presentFn = maybePresent as PresentFn;
};

const maybeUpdateFramesReceivedFromSeq = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_SEQ_INDEX) return;

  const seq = Atomics.load(frameState, FRAME_SEQ_INDEX);
  if (seq === lastSeenSeq) return;

  const delta = seq - lastSeenSeq;
  if (delta > 0) framesReceived += delta;
  lastSeenSeq = seq;
};

const shouldPresentWithSharedState = () => {
  if (!frameState) return false;
  const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
  return status === FRAME_DIRTY;
};

const claimPresentWithSharedState = () => {
  if (!frameState) return false;
  const prev = Atomics.compareExchange(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY, FRAME_PRESENTING);
  return prev === FRAME_DIRTY;
};

const finishPresentWithSharedState = () => {
  if (!frameState) return;
  Atomics.compareExchange(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTING, FRAME_PRESENTED);
  Atomics.notify(frameState, FRAME_STATUS_INDEX);
};

const computeDroppedFromSeqForPresent = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_SEQ_INDEX) return;

  const seq = Atomics.load(frameState, FRAME_SEQ_INDEX);
  const dropped = Math.max(0, seq - lastPresentedSeq - 1);
  framesDropped += dropped;
  lastPresentedSeq = seq;
};

const presentOnce = async () => {
  if (!presentFn) return false;
  const result = await presentFn();
  return typeof result === 'boolean' ? result : true;
};

const handleTick = async () => {
  maybeUpdateFramesReceivedFromSeq();

  if (!presentFn) {
    maybePostMetrics();
    return;
  }

  if (presenting) {
    maybePostMetrics();
    return;
  }

  if (frameState) {
    if (!shouldPresentWithSharedState()) {
      maybePostMetrics();
      return;
    }

    if (!claimPresentWithSharedState()) {
      maybePostMetrics();
      return;
    }

    computeDroppedFromSeqForPresent();
  } else {
    if (pendingFrames === 0) {
      maybePostMetrics();
      return;
    }

    if (pendingFrames > 1) {
      framesDropped += pendingFrames - 1;
    }

    pendingFrames = 0;
  }

  presenting = true;
  try {
    const didPresent = await presentOnce();
    if (didPresent) framesPresented += 1;
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    postToMain({ type: 'error', message });
  } finally {
    presenting = false;
    finishPresentWithSharedState();
    maybePostMetrics();
  }
};

self.onmessage = (event: MessageEvent<GpuWorkerMessageFromMain>) => {
  const msg = event.data;
  if (!msg || typeof msg !== 'object' || !('type' in msg)) return;

  switch (msg.type) {
    case 'init': {
      if (msg.sharedFrameState) {
        frameState = new Int32Array(msg.sharedFrameState);
      }

      if (msg.wasmModuleUrl) {
        loadPresentFnFromModuleUrl(msg.wasmModuleUrl).catch((err) => {
          const message = err instanceof Error ? err.message : String(err);
          postToMain({ type: 'error', message });
        });
      }

      break;
    }

    case 'frame_dirty': {
      if (!frameState) {
        pendingFrames += 1;
        framesReceived += 1;
      }
      break;
    }

    case 'tick': {
      void msg.frameTimeMs;
      void handleTick();
      break;
    }
  }
};
