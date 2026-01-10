import type { PresenterBackendKind, PresenterInitOptions } from '../gpu/presenter';

export type GpuWorkerInMessage =
  | {
      type: 'init';
      canvas: OffscreenCanvas;
      width: number;
      height: number;
      dpr: number;
      opts?: PresenterInitOptions;
      forceBackend?: PresenterBackendKind;
    }
  | { type: 'resize'; width: number; height: number; dpr: number }
  | { type: 'present'; frame: number | ArrayBuffer | ArrayBufferView; stride: number }
  | { type: 'screenshot'; requestId: number };

export type GpuWorkerOutMessage =
  | { type: 'inited'; backend: PresenterBackendKind }
  | { type: 'error'; message: string; code?: string; backend?: PresenterBackendKind }
  | { type: 'screenshot'; requestId: number; width: number; height: number; pixels: ArrayBuffer };

