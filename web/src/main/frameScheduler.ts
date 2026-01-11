import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_STATUS_INDEX,
} from '../shared/frameProtocol';
import { perf } from '../perf/perf';
import type { GpuRuntimeInitOptions, GpuRuntimeOutMessage } from "../workers/gpu_runtime_protocol";

import { DebugOverlay } from '../../ui/debug_overlay.ts';

export type FrameSchedulerMetrics = {
  framesReceived: number;
  framesPresented: number;
  framesDropped: number;
};

export type FrameSchedulerOptions = {
  gpuWorker: Worker;
  sharedFrameState: SharedArrayBuffer;
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes?: number;
  canvas?: OffscreenCanvas;
  initOptions?: GpuRuntimeInitOptions;
  showDebugOverlay?: boolean;
  overlayParent?: HTMLElement;
  debugOverlayToggleKey?: string;
};

export type FrameSchedulerHandle = {
  stop: () => void;
  getMetrics: () => FrameSchedulerMetrics;
};

export const startFrameScheduler = ({
  gpuWorker,
  sharedFrameState,
  sharedFramebuffer,
  sharedFramebufferOffsetBytes,
  canvas,
  initOptions,
  showDebugOverlay = true,
  overlayParent,
  debugOverlayToggleKey = 'F3',
}: FrameSchedulerOptions): FrameSchedulerHandle => {
  let rafId: number | null = null;
  let stopped = false;

  const metrics: FrameSchedulerMetrics = {
    framesReceived: 0,
    framesPresented: 0,
    framesDropped: 0,
  };

  let lastTelemetry: unknown = null;

  const frameState = new Int32Array(sharedFrameState);
  perf.registerWorker(gpuWorker, { threadName: 'gpu-presenter' });
  try {
    gpuWorker.postMessage(
      {
        type: "init",
        canvas,
        sharedFrameState,
        sharedFramebuffer,
        sharedFramebufferOffsetBytes: sharedFramebufferOffsetBytes ?? 0,
        options: initOptions,
      },
      canvas ? [canvas] : [],
    );
  } catch (err) {
    throw err instanceof Error ? err : new Error(String(err));
  }

  let overlay: DebugOverlay | null = null;
  if (showDebugOverlay) {
    overlay = new DebugOverlay(() => lastTelemetry as any, {
      parent: overlayParent,
      toggleKey: debugOverlayToggleKey,
    });
    overlay.show();
  }

  const updateTelemetry = (msg: { framesReceived: number; framesPresented: number; framesDropped: number; telemetry?: unknown }) => {
    const baseTelemetry = msg.telemetry;
    if (baseTelemetry && typeof baseTelemetry === 'object') {
      lastTelemetry = { ...(baseTelemetry as Record<string, unknown>), ...msg };
    } else {
      lastTelemetry = { ...msg };
    }
  };

  const onWorkerMessage = (event: MessageEvent<GpuRuntimeOutMessage>) => {
    const msg = event.data;
    if (msg.type === 'metrics') {
      metrics.framesReceived = msg.framesReceived;
      metrics.framesPresented = msg.framesPresented;
      metrics.framesDropped = msg.framesDropped;
      updateTelemetry(msg);
      return;
    }

    if (msg.type === 'error') {
      console.error(`gpu-worker: ${msg.message}`);
    }
  };

  gpuWorker.addEventListener('message', onWorkerMessage);

  const shouldSendTick = () => {
    const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
    if (status === FRAME_DIRTY) return true;
    if (status === FRAME_PRESENTING || status === FRAME_PRESENTED) return false;
    return true;
  };

  const loop = (frameTimeMs: number) => {
    if (stopped) return;

    perf.spanBegin('frame');
    try {
      perf.spanBegin('render');
      try {
        if (shouldSendTick()) {
        perf.spanBegin('present');
        try {
          try {
            gpuWorker.postMessage({ type: 'tick', frameTimeMs });
          } catch (err) {
            console.error('[frameScheduler] Failed to post tick to GPU worker; stopping scheduler.', err);
            stop();
            return;
          }
        } finally {
          perf.spanEnd('present');
        }
      }
      } finally {
        perf.spanEnd('render');
      }

      rafId = requestAnimationFrame(loop);
    } finally {
      perf.spanEnd('frame');
    }
  };

  rafId = requestAnimationFrame(loop);

  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (rafId !== null) cancelAnimationFrame(rafId);
    gpuWorker.removeEventListener('message', onWorkerMessage);
    overlay?.detach();
  };

  const getMetrics = () => ({ ...metrics });

  return { stop, getMetrics };
};
