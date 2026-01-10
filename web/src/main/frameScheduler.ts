import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_STATUS_INDEX,
  type GpuWorkerMessageToMain,
} from '../shared/frameProtocol';

export type FrameSchedulerMetrics = {
  framesReceived: number;
  framesPresented: number;
  framesDropped: number;
};

export type FrameSchedulerOptions = {
  gpuWorker: Worker;
  sharedFrameState?: SharedArrayBuffer;
  showDebugOverlay?: boolean;
  overlayParent?: HTMLElement;
};

export type FrameSchedulerHandle = {
  stop: () => void;
  getMetrics: () => FrameSchedulerMetrics;
};

const formatRate = (rate: number) => (Number.isFinite(rate) ? rate.toFixed(1) : '0.0');

export const startFrameScheduler = ({
  gpuWorker,
  sharedFrameState,
  showDebugOverlay = true,
  overlayParent,
}: FrameSchedulerOptions): FrameSchedulerHandle => {
  let rafId: number | null = null;
  let stopped = false;

  const metrics: FrameSchedulerMetrics = {
    framesReceived: 0,
    framesPresented: 0,
    framesDropped: 0,
  };

  const frameState = sharedFrameState ? new Int32Array(sharedFrameState) : null;
  gpuWorker.postMessage({ type: 'init', sharedFrameState });

  let overlay: HTMLDivElement | null = null;
  if (showDebugOverlay && typeof document !== 'undefined') {
    overlay = document.createElement('div');
    overlay.style.position = 'fixed';
    overlay.style.top = '0';
    overlay.style.left = '0';
    overlay.style.padding = '6px 8px';
    overlay.style.background = 'rgba(0, 0, 0, 0.75)';
    overlay.style.color = '#d1f7ff';
    overlay.style.fontFamily = 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace';
    overlay.style.fontSize = '12px';
    overlay.style.lineHeight = '1.35';
    overlay.style.pointerEvents = 'none';
    overlay.style.whiteSpace = 'pre';
    overlay.style.zIndex = '999999';
    (overlayParent ?? document.body).appendChild(overlay);
  }

  let lastOverlayUpdateMs = 0;
  let lastSampleMs = performance.now();
  let lastSampleReceived = 0;
  let lastSamplePresented = 0;
  let lastSampleDropped = 0;

  const updateOverlay = (nowMs: number) => {
    if (!overlay) return;
    if (nowMs - lastOverlayUpdateMs < 200) return;

    const dtMs = Math.max(1, nowMs - lastSampleMs);
    const recvDelta = metrics.framesReceived - lastSampleReceived;
    const presDelta = metrics.framesPresented - lastSamplePresented;
    const dropDelta = metrics.framesDropped - lastSampleDropped;

    const recvRate = (recvDelta * 1000) / dtMs;
    const presRate = (presDelta * 1000) / dtMs;
    const dropRate = (dropDelta * 1000) / dtMs;

    overlay.textContent = [
      `frames received : ${metrics.framesReceived} (${formatRate(recvRate)}/s)`,
      `frames presented: ${metrics.framesPresented} (${formatRate(presRate)}/s)`,
      `frames dropped  : ${metrics.framesDropped} (${formatRate(dropRate)}/s)`,
    ].join('\n');

    lastOverlayUpdateMs = nowMs;
    lastSampleMs = nowMs;
    lastSampleReceived = metrics.framesReceived;
    lastSamplePresented = metrics.framesPresented;
    lastSampleDropped = metrics.framesDropped;
  };

  const onWorkerMessage = (event: MessageEvent<GpuWorkerMessageToMain>) => {
    const msg = event.data;
    if (msg.type === 'metrics') {
      metrics.framesReceived = msg.framesReceived;
      metrics.framesPresented = msg.framesPresented;
      metrics.framesDropped = msg.framesDropped;
      return;
    }

    if (msg.type === 'error') {
      console.error(`gpu-worker: ${msg.message}`);
    }
  };

  gpuWorker.addEventListener('message', onWorkerMessage);

  const shouldSendTick = () => {
    if (!frameState) return true;
    const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
    if (status === FRAME_DIRTY) return true;
    if (status === FRAME_PRESENTING || status === FRAME_PRESENTED) return false;
    return true;
  };

  const loop = (frameTimeMs: number) => {
    if (stopped) return;

    if (shouldSendTick()) {
      gpuWorker.postMessage({ type: 'tick', frameTimeMs });
    }

    updateOverlay(performance.now());
    rafId = requestAnimationFrame(loop);
  };

  rafId = requestAnimationFrame(loop);

  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (rafId !== null) cancelAnimationFrame(rafId);
    gpuWorker.removeEventListener('message', onWorkerMessage);
    overlay?.remove();
  };

  const getMetrics = () => ({ ...metrics });

  return { stop, getMetrics };
};
