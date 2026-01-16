import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type GpuRuntimeInitOptions,
  type GpuRuntimeOutMessage,
} from '../ipc/gpu-protocol';
import {
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
  SCANOUT_SOURCE_WDDM,
  SCANOUT_STATE_GENERATION_BUSY_BIT,
  ScanoutStateIndex,
  wrapScanoutState,
} from '../ipc/scanout_state';
import { perf } from '../perf/perf';
import { formatOneLineError, formatOneLineUtf8 } from "../text";

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
  /**
   * Optional shared scanout state used to wake the GPU worker even when the legacy
   * shared framebuffer is idle (e.g. once WDDM scanout takes over).
   */
  scanoutState?: SharedArrayBuffer;
  scanoutStateOffsetBytes?: number;
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
  scanoutState,
  scanoutStateOffsetBytes,
  canvas,
  initOptions,
  showDebugOverlay = true,
  overlayParent,
  debugOverlayToggleKey = 'F3',
}: FrameSchedulerOptions): FrameSchedulerHandle => {
  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  let rafId: number | null = null;
  let stopped = false;

  const metrics: FrameSchedulerMetrics = {
    framesReceived: 0,
    framesPresented: 0,
    framesDropped: 0,
  };

  let lastTelemetry: unknown = null;

  const frameState = new Int32Array(sharedFrameState);

  // Optional scanout state, used to keep ticking even when frameState is PRESENTED.
  let scanoutWords: Int32Array | null = null;
  let lastScanoutGeneration = 0;
  if (scanoutState instanceof SharedArrayBuffer) {
    try {
      scanoutWords = wrapScanoutState(scanoutState, scanoutStateOffsetBytes ?? 0);
      lastScanoutGeneration = Atomics.load(scanoutWords, ScanoutStateIndex.GENERATION) >>> 0;
      lastScanoutGeneration &= ~SCANOUT_STATE_GENERATION_BUSY_BIT;
    } catch {
      scanoutWords = null;
      lastScanoutGeneration = 0;
    }
  }
  perf.registerWorker(gpuWorker, { threadName: 'gpu-presenter' });
  try {
    gpuWorker.postMessage(
      {
        ...GPU_MESSAGE_BASE,
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
    throw err instanceof Error ? err : new Error(formatOneLineError(err, 512));
  }

  let overlay: DebugOverlay | null = null;
  if (showDebugOverlay) {
    overlay = new DebugOverlay(() => lastTelemetry, {
      parent: overlayParent,
      toggleKey: debugOverlayToggleKey,
    });
    overlay.show();
  }

  const updateTelemetry = (msg: { framesReceived: number; framesPresented: number; framesDropped: number; telemetry?: unknown }) => {
    // Preserve structured GPU events forwarded via `type:"events"` messages; these arrive
    // out-of-band relative to the regular `metrics` updates.
    const prevGpuEvents =
      lastTelemetry && typeof lastTelemetry === "object" && Array.isArray((lastTelemetry as Record<string, unknown>).gpuEvents)
        ? ((lastTelemetry as Record<string, unknown>).gpuEvents as unknown[])
        : null;
    const prevGpuStats =
      lastTelemetry && typeof lastTelemetry === "object" && (lastTelemetry as Record<string, unknown>).gpuStats !== undefined
        ? (lastTelemetry as Record<string, unknown>).gpuStats
        : null;

    const baseTelemetry = msg.telemetry;
    if (baseTelemetry && typeof baseTelemetry === 'object') {
      const next = { ...(baseTelemetry as Record<string, unknown>), ...msg } as Record<string, unknown>;
      if (prevGpuEvents && next.gpuEvents === undefined) {
        next.gpuEvents = prevGpuEvents;
      }
      if (prevGpuStats !== null && next.gpuStats === undefined) {
        next.gpuStats = prevGpuStats;
      }
      lastTelemetry = next;
    } else {
      const next = { ...msg } as Record<string, unknown>;
      if (prevGpuEvents && next.gpuEvents === undefined) {
        next.gpuEvents = prevGpuEvents;
      }
      if (prevGpuStats !== null && next.gpuStats === undefined) {
        next.gpuStats = prevGpuStats;
      }
      lastTelemetry = next;
    }
  };

  const onWorkerMessage = (event: MessageEvent<unknown>) => {
    const msg = event.data;
    if (!isGpuWorkerMessageBase(msg) || typeof (msg as { type?: unknown }).type !== "string") return;

    const typed = msg as GpuRuntimeOutMessage;
    if (typed.type === 'metrics') {
      metrics.framesReceived = typed.framesReceived;
      metrics.framesPresented = typed.framesPresented;
      metrics.framesDropped = typed.framesDropped;
      updateTelemetry(typed);
      return;
    }

    if (typed.type === "stats") {
      if (lastTelemetry && typeof lastTelemetry === "object") {
        lastTelemetry = { ...(lastTelemetry as Record<string, unknown>), gpuStats: typed };
      } else {
        lastTelemetry = { gpuStats: typed };
      }
      return;
    }

    if (typed.type === "events") {
      // Forward structured GPU worker diagnostics to the console for visibility during dev/testing.
      // (These events are also useful for telemetry pipelines; keeping them structured avoids
      // parsing fragile string logs.)
      const rawEvents = (typed as unknown as { events?: unknown }).events;
      const evs = Array.isArray(rawEvents) ? (rawEvents as unknown[]) : [];
      if (evs.length > 0) {
        if (lastTelemetry && typeof lastTelemetry === "object") {
          lastTelemetry = { ...(lastTelemetry as Record<string, unknown>), gpuEvents: evs };
        } else {
          lastTelemetry = { gpuEvents: evs };
        }

        for (const ev of evs) {
          if (!ev || typeof ev !== "object") {
            const value =
              typeof ev === "string"
                ? formatOneLineUtf8(ev, 256) || "unknown"
                : typeof ev === "number" || typeof ev === "boolean" || typeof ev === "bigint"
                  ? String(ev)
                  : "unknown";
            console.error("gpu-worker[Unknown]", value);
            continue;
          }

          const record = ev as Record<string, unknown>;
          const sev = typeof record["severity"] === "string" ? record["severity"] : "error";
          const cat = typeof record["category"] === "string" ? record["category"] : "Unknown";
          const message =
            typeof record["message"] === "string"
              ? (formatOneLineUtf8(record["message"], 256) || "gpu event")
              : formatOneLineError(record["message"] ?? "gpu event", 256, "gpu event");
          const details = record["details"];
          const safeCat = formatOneLineUtf8(cat, 64) || "Unknown";
          const prefix = `gpu-worker[${safeCat}]`;
          switch (sev) {
            case "info":
              console.info(prefix, message, details);
              break;
            case "warn":
              console.warn(prefix, message, details);
              break;
            case "fatal":
            case "error":
            default:
              console.error(prefix, message, details);
              break;
          }
        }
      }
      return;
    }

    if (typed.type === 'error') {
      console.error(`gpu-worker: ${formatOneLineError(typed.message, 512)}`);
    }
  };

  gpuWorker.addEventListener('message', onWorkerMessage);

  const shouldSendTick = () => {
    const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
    // While a present is in flight, avoid spamming ticks; the worker will flip the
    // status back to PRESENTED once it completes.
    if (status === FRAME_PRESENTING) return false;
    if (status === FRAME_DIRTY) return true;

    // When scanout is WDDM-owned (or being updated), keep the tick loop running so
    // the GPU worker can poll/present the scanout source even if the legacy shared
    // framebuffer is idle.
    if (scanoutWords) {
      const gen = Atomics.load(scanoutWords, ScanoutStateIndex.GENERATION) >>> 0;
      if ((gen & SCANOUT_STATE_GENERATION_BUSY_BIT) !== 0) return true;
      const source = Atomics.load(scanoutWords, ScanoutStateIndex.SOURCE) >>> 0;
      if (source === SCANOUT_SOURCE_LEGACY_VBE_LFB) return true;
      if (source === SCANOUT_SOURCE_WDDM) {
        // Distinguish the WDDM "placeholder" descriptor (base=0 but non-zero geometry, used by some
        // host-side AeroGPU paths) from the WDDM "disabled descriptor" (base/width/height/pitch=0),
        // which represents blank output while WDDM retains ownership.
        //
        // When scanout is disabled, do not keep ticking continuously: this matches the device-side
        // "SCANOUT0_ENABLE=0 stops vblank pacing" behavior and avoids tick spam while blank.
        const lo = Atomics.load(scanoutWords, ScanoutStateIndex.BASE_PADDR_LO) >>> 0;
        const hi = Atomics.load(scanoutWords, ScanoutStateIndex.BASE_PADDR_HI) >>> 0;
        const width = Atomics.load(scanoutWords, ScanoutStateIndex.WIDTH) >>> 0;
        const height = Atomics.load(scanoutWords, ScanoutStateIndex.HEIGHT) >>> 0;
        const pitchBytes = Atomics.load(scanoutWords, ScanoutStateIndex.PITCH_BYTES) >>> 0;
        const disabled = ((lo | hi) >>> 0) === 0 && width === 0 && height === 0 && pitchBytes === 0;
        if (!disabled) return true;
      }
      const stableGen = gen & ~SCANOUT_STATE_GENERATION_BUSY_BIT;
      if (stableGen !== lastScanoutGeneration) {
        lastScanoutGeneration = stableGen;
        return true;
      }
    }

    if (status === FRAME_PRESENTED) return false;
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
                gpuWorker.postMessage({ ...GPU_MESSAGE_BASE, type: 'tick', frameTimeMs });
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
