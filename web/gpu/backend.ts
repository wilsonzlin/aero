export type GpuBackendKind = 'webgpu' | 'webgl2';
export type FilterMode = 'nearest' | 'linear';

export interface BackendInitOptions {
  readonly filter?: FilterMode;
  readonly preserveAspectRatio?: boolean;
}

export interface DirtyRect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

export interface CapturedFrame {
  readonly width: number;
  readonly height: number;
  readonly data: Uint8ClampedArray;
}

export interface BackendCapabilities {
  readonly kind: GpuBackendKind;
  readonly supportsDirtyRects: boolean;
  readonly supportsCapture: boolean;
}

export interface PresentationBackend {
  init(canvas: HTMLCanvasElement | OffscreenCanvas, options?: BackendInitOptions): Promise<void>;
  uploadFrameRGBA(
    buffer: ArrayBufferView,
    width: number,
    height: number,
    dirtyRects?: readonly DirtyRect[],
  ): void;
  present(): void | Promise<void>;
  captureFrame(): Promise<CapturedFrame>;
  getCapabilities(): BackendCapabilities;
}
