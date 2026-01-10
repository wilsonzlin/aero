import type {
  GpuWorkerGpuErrorMessage,
  GpuWorkerInitOptions,
  GpuWorkerOutgoingMessage,
  GpuWorkerReadyMessage,
  GpuWorkerScreenshotMessage,
} from '../ipc/gpu-messages';

export interface CreateGpuWorkerParams {
  canvas: HTMLCanvasElement;
  width: number;
  height: number;
  devicePixelRatio: number;
  gpuOptions?: GpuWorkerInitOptions;
  onGpuError?: (msg: GpuWorkerGpuErrorMessage) => void;
}

export interface GpuWorkerHandle {
  worker: Worker;
  ready: Promise<GpuWorkerReadyMessage>;
  resize(width: number, height: number, devicePixelRatio: number): void;
  presentTestPattern(): void;
  requestScreenshot(): Promise<GpuWorkerScreenshotMessage>;
  shutdown(): void;
}

export function createGpuWorker(params: CreateGpuWorkerParams): GpuWorkerHandle {
  if (!('transferControlToOffscreen' in params.canvas)) {
    throw new Error('OffscreenCanvas is not supported in this browser.');
  }

  const worker = new Worker(new URL('../workers/aero-gpu-worker.ts', import.meta.url), { type: 'module' });

  const offscreen = params.canvas.transferControlToOffscreen();

  let readyResolve: (msg: GpuWorkerReadyMessage) => void;
  let readyReject: (err: unknown) => void;
  let readySettled = false;

  const ready = new Promise<GpuWorkerReadyMessage>((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });

  let nextRequestId = 1;
  const screenshotRequests = new Map<
    number,
    { resolve: (msg: GpuWorkerScreenshotMessage) => void; reject: (err: unknown) => void }
  >();

  function rejectAllPending(err: unknown): void {
    for (const [, pending] of screenshotRequests) {
      pending.reject(err);
    }
    screenshotRequests.clear();
  }

  worker.addEventListener('message', (event) => {
    const msg = event.data as GpuWorkerOutgoingMessage;
    if (!msg || typeof msg !== 'object' || typeof (msg as { type?: unknown }).type !== 'string') return;

    switch (msg.type) {
      case 'ready':
        readySettled = true;
        readyResolve(msg);
        break;
      case 'screenshot': {
        const pending = screenshotRequests.get(msg.requestId);
        if (!pending) return;
        screenshotRequests.delete(msg.requestId);
        pending.resolve(msg);
        break;
      }
      case 'gpu_error': {
        params.onGpuError?.(msg);
        if (msg.fatal) {
          const err = new Error(`aero-gpu-worker fatal error: ${msg.error.kind}: ${msg.error.message}`);
          if (!readySettled) {
            readySettled = true;
            readyReject(err);
          }
          rejectAllPending(err);
        }
        break;
      }
      default:
        break;
    }
  });

  worker.addEventListener('error', (event) => {
    const err = (event as ErrorEvent).error ?? event;
    params.onGpuError?.({
      type: 'gpu_error',
      fatal: true,
      error: { kind: 'unexpected', message: String(err) },
    });
    if (!readySettled) {
      readySettled = true;
      readyReject(err);
    }
    rejectAllPending(err);
  });

  worker.postMessage(
    {
      type: 'init',
      canvas: offscreen,
      width: params.width,
      height: params.height,
      devicePixelRatio: params.devicePixelRatio,
      gpuOptions: params.gpuOptions,
    },
    [offscreen],
  );

  function resize(width: number, height: number, devicePixelRatio: number): void {
    worker.postMessage({ type: 'resize', width, height, devicePixelRatio });
  }

  function presentTestPattern(): void {
    worker.postMessage({ type: 'present_test_pattern' });
  }

  function requestScreenshot(): Promise<GpuWorkerScreenshotMessage> {
    const requestId = nextRequestId++;
    worker.postMessage({ type: 'request_screenshot', requestId });

    return new Promise<GpuWorkerScreenshotMessage>((resolve, reject) => {
      screenshotRequests.set(requestId, { resolve, reject });
    });
  }

  function shutdown(): void {
    worker.postMessage({ type: 'shutdown' });
    worker.terminate();
  }

  return {
    worker,
    ready,
    resize,
    presentTestPattern,
    requestScreenshot,
    shutdown,
  };
}
