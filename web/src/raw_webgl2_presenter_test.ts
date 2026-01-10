import type { PresenterBackendKind, PresenterScaleMode } from "./gpu/presenter";
import type { GpuWorkerInMessage, GpuWorkerOutMessage } from "./workers/gpu-worker-protocol";

type InitArgs = {
  width: number;
  height: number;
  dpr: number;
  forceBackend?: PresenterBackendKind;
  scaleMode?: PresenterScaleMode;
};

type ScreenshotResult = { width: number; height: number; pixels: Uint8Array };

declare global {
  interface Window {
    __aeroTest?: {
      init: (args: InitArgs) => Promise<{ backend: PresenterBackendKind }>;
      present: (frame: Uint8Array, stride: number) => void;
      screenshot: () => Promise<ScreenshotResult>;
    };
  }
}

const canvas = document.getElementById("canvas");
if (!(canvas instanceof HTMLCanvasElement)) throw new Error("Expected #canvas to be a HTMLCanvasElement");

const worker = new Worker(new URL("./workers/gpu-worker.ts", import.meta.url), { type: "module" });
const offscreen = canvas.transferControlToOffscreen();

let backend: PresenterBackendKind | null = null;

let initResolver: ((value: { backend: PresenterBackendKind }) => void) | null = null;
let initRejecter: ((reason: unknown) => void) | null = null;

let nextScreenshotId = 1;
const screenshotResolvers = new Map<number, (value: ScreenshotResult) => void>();
const screenshotRejecters = new Map<number, (reason: unknown) => void>();

worker.onmessage = (ev: MessageEvent<GpuWorkerOutMessage>) => {
  const msg = ev.data;
  switch (msg.type) {
    case "inited": {
      backend = msg.backend;
      initResolver?.({ backend: msg.backend });
      initResolver = null;
      initRejecter = null;
      break;
    }
    case "error": {
      const err = new Error(msg.message);
      (err as any).code = msg.code;
      (err as any).backend = msg.backend;
      if (initRejecter) {
        initRejecter(err);
        initResolver = null;
        initRejecter = null;
        return;
      }
      for (const reject of screenshotRejecters.values()) reject(err);
      screenshotResolvers.clear();
      screenshotRejecters.clear();
      throw err;
    }
    case "screenshot": {
      const resolve = screenshotResolvers.get(msg.requestId);
      const reject = screenshotRejecters.get(msg.requestId);
      screenshotResolvers.delete(msg.requestId);
      screenshotRejecters.delete(msg.requestId);
      if (!resolve) {
        reject?.(new Error(`Unknown screenshot requestId ${msg.requestId}`));
        return;
      }
      resolve({ width: msg.width, height: msg.height, pixels: new Uint8Array(msg.pixels) });
      break;
    }
  }
};

function post(msg: GpuWorkerInMessage, transfer?: Transferable[]) {
  worker.postMessage(msg, transfer ?? []);
}

window.__aeroTest = {
  init: async (args: InitArgs) => {
    if (initResolver) throw new Error("Already initializing");
    const promise = new Promise<{ backend: PresenterBackendKind }>((resolve, reject) => {
      initResolver = resolve;
      initRejecter = reject;
    });

    const initMsg: GpuWorkerInMessage = {
      type: "init",
      canvas: offscreen,
      width: args.width,
      height: args.height,
      dpr: args.dpr,
      forceBackend: args.forceBackend,
      opts: { scaleMode: args.scaleMode },
    };
    post(initMsg, [offscreen]);
    return await promise;
  },
  present: (frame: Uint8Array, stride: number) => {
    if (!backend) throw new Error("present called before init");
    const msg: GpuWorkerInMessage = { type: "present", frame, stride };
    post(msg, [frame.buffer]);
  },
  screenshot: async (): Promise<ScreenshotResult> => {
    if (!backend) throw new Error("screenshot called before init");
    const requestId = nextScreenshotId++;
    const promise = new Promise<ScreenshotResult>((resolve, reject) => {
      screenshotResolvers.set(requestId, resolve);
      screenshotRejecters.set(requestId, reject);
    });
    post({ type: "screenshot", requestId });
    return await promise;
  },
};

