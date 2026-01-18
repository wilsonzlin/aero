import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_FORMAT,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  loadHeaderI32,
  type FramebufferCopyFrame,
  type SharedFramebufferView,
} from "./framebuffer_protocol";
import { perf } from "../perf/perf";
import { unrefBestEffort } from "../unrefSafe";
import { encodeLinearRgba8ToSrgbInPlace } from "../utils/srgb";

export type VgaScaleMode = "auto" | "pixelated" | "smooth";

export type VgaPresenterOptions = {
  scaleMode?: VgaScaleMode;
  integerScaling?: boolean;
  autoResizeToClient?: boolean;
  maxPresentHz?: number;
  clearColor?: string;
};

type PresenterCanvas = HTMLCanvasElement | OffscreenCanvas;
type PresenterCanvasRenderingContext2D = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

function hasRaf(): boolean {
  return typeof requestAnimationFrame === "function";
}

function cancelRaf(id: number): void {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(id);
  }
}

function clampPositiveInt(name: string, value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`Invalid ${name}: ${value}`);
  }
  return value;
}

function get2dContext(canvas: PresenterCanvas): PresenterCanvasRenderingContext2D {
  const ctx = canvas.getContext("2d", { alpha: false }) as PresenterCanvasRenderingContext2D | null;
  if (!ctx) {
    throw new Error("2D canvas context not available");
  }
  return ctx;
}

function createScratchCanvas(width: number, height: number): PresenterCanvas {
  if (typeof OffscreenCanvas !== "undefined") {
    return new OffscreenCanvas(width, height);
  }
  if (typeof document !== "undefined") {
    const c = document.createElement("canvas");
    c.width = width;
    c.height = height;
    return c;
  }
  throw new Error("No canvas implementation available for scratch blits");
}

/**
 * Presents an RGBA8888 framebuffer to a canvas at a fixed present rate
 * (usually ~60Hz). The emulation core can produce frames at any rate; the
 * presenter will drop frames if they are produced faster than it presents.
 */
export class VgaPresenter {
  private canvas: PresenterCanvas;
  private ctx: PresenterCanvasRenderingContext2D;
  private options: Required<VgaPresenterOptions>;

  private shared: SharedFramebufferView | null = null;
  private copy: FramebufferCopyFrame | null = null;

  private running = false;
  private lastPresentedFrame = -1;
  private lastConfigCounter = -1;
  private nextPresentTimeMs = 0;
  private timerHandle: ReturnType<typeof setTimeout> | null = null;
  private rafHandle: number | null = null;

  private srcCanvas: PresenterCanvas | null = null;
  private srcCtx: PresenterCanvasRenderingContext2D | null = null;
  private srcImageData: ImageData | null = null;
  private srcImageBytes: Uint8ClampedArray<ArrayBuffer> | null = null;
  private srcWidth = 0;
  private srcHeight = 0;
  private srcStrideBytes = 0;
  private srcFormat = 0;

  private resizeObserver: ResizeObserver | null = null;

  constructor(canvas: PresenterCanvas, options: VgaPresenterOptions = {}) {
    this.canvas = canvas;
    this.ctx = get2dContext(canvas);

    this.options = {
      scaleMode: options.scaleMode ?? "auto",
      integerScaling: options.integerScaling ?? true,
      autoResizeToClient: options.autoResizeToClient ?? true,
      maxPresentHz: options.maxPresentHz ?? 60,
      clearColor: options.clearColor ?? "#000",
    };

    if (
      typeof ResizeObserver !== "undefined" &&
      this.options.autoResizeToClient &&
      typeof HTMLCanvasElement !== "undefined" &&
      this.canvas instanceof HTMLCanvasElement
    ) {
      this.resizeObserver = new ResizeObserver(() => this.syncCanvasBackingStoreToClient());
      this.resizeObserver.observe(this.canvas);
      this.syncCanvasBackingStoreToClient();
    }
  }

  private isHtmlCanvas(): boolean {
    return typeof HTMLCanvasElement !== "undefined" && this.canvas instanceof HTMLCanvasElement;
  }

  destroy(): void {
    this.stop();
    if (this.resizeObserver) {
      this.resizeObserver.disconnect();
      this.resizeObserver = null;
    }
    if (this.srcCanvas && "width" in this.srcCanvas) {
      // No explicit destroy API; let GC reclaim.
    }
    if (this.ctx && this.isHtmlCanvas()) {
      // Nothing.
    }
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    // Schedule an immediate present attempt on `start()`. We store the *next*
    // eligible present time instead of the last time we presented so that we
    // can achieve a stable average present rate even when
    // `requestAnimationFrame` runs at an arbitrary refresh rate (e.g. 75/144Hz).
    this.nextPresentTimeMs = 0;
    this.scheduleNextTick();
  }

  stop(): void {
    this.running = false;
    if (this.rafHandle != null) {
      cancelRaf(this.rafHandle);
      this.rafHandle = null;
    }
    if (this.timerHandle != null) {
      clearTimeout(this.timerHandle);
      this.timerHandle = null;
    }
  }

  setSharedFramebuffer(shared: SharedFramebufferView | null): void {
    this.shared = shared;
    this.copy = null;
    this.lastPresentedFrame = -1;
    this.lastConfigCounter = -1;
  }

  pushCopyFrame(frame: FramebufferCopyFrame): void {
    this.copy = frame;
    this.shared = null;
  }

  scheduleNextTick(): void {
    if (!this.running) return;

    const intervalMs = 1000 / clampPositiveInt("maxPresentHz", this.options.maxPresentHz);
    const now = performance.now ? performance.now() : Date.now();
    const delay = Math.max(0, this.nextPresentTimeMs - now);

    // Prefer `requestAnimationFrame` for vsync-aligned presents when available,
    // but use a timeout to avoid spinning the worker/main thread when the next
    // eligible present is far in the future.
    if (hasRaf()) {
      if (delay <= 0) {
        this.rafHandle = requestAnimationFrame(() => this.tick());
      } else {
        const timer = setTimeout(() => {
          this.rafHandle = requestAnimationFrame(() => this.tick());
        }, delay);
        unrefBestEffort(timer);
        this.timerHandle = timer;
      }
      return;
    }

    const timer = setTimeout(() => this.tick(), delay <= 0 ? 0 : Math.min(delay, intervalMs));
    unrefBestEffort(timer);
    this.timerHandle = timer;
  }

  tick(): void {
    if (!this.running) return;

    perf.spanBegin("frame");
    try {
      const intervalMs = 1000 / clampPositiveInt("maxPresentHz", this.options.maxPresentHz);
      const now = performance.now ? performance.now() : Date.now();

      if (this.nextPresentTimeMs !== 0 && now < this.nextPresentTimeMs) {
        return;
      }

      // Advance the next-present schedule. If we've fallen behind by more than
      // one interval (tab suspended / debugger break), drop frames and resync.
      if (this.nextPresentTimeMs === 0 || now - this.nextPresentTimeMs > intervalMs) {
        this.nextPresentTimeMs = now + intervalMs;
      } else {
        this.nextPresentTimeMs += intervalMs;
      }

      perf.spanBegin("present");
      try {
        this.presentLatestFrame();
      } finally {
        perf.spanEnd("present");
      }
    } finally {
      perf.spanEnd("frame");
      this.scheduleNextTick();
    }
  }

  presentLatestFrame(): void {
    if (this.shared) {
      this.presentShared(this.shared);
      return;
    }
    if (this.copy) {
      this.presentCopy(this.copy);
    }
  }

  presentShared(shared: SharedFramebufferView): void {
    const header = shared.header;

    const format = loadHeaderI32(header, HEADER_INDEX_FORMAT);
    if (format !== FRAMEBUFFER_FORMAT_RGBA8888) {
      return;
    }

    const configCounter = loadHeaderI32(header, HEADER_INDEX_CONFIG_COUNTER);
    if (configCounter === 0) {
      // Not initialized yet.
      return;
    }
    if (configCounter !== this.lastConfigCounter) {
      const width = loadHeaderI32(header, HEADER_INDEX_WIDTH);
      const height = loadHeaderI32(header, HEADER_INDEX_HEIGHT);
      const strideBytes = loadHeaderI32(header, HEADER_INDEX_STRIDE_BYTES);
      if (width <= 0 || height <= 0 || strideBytes < width * 4) {
        return;
      }
      this.reconfigureSource(width, height, strideBytes, format);
      this.lastConfigCounter = configCounter;
    }

    const frameCounter = loadHeaderI32(header, HEADER_INDEX_FRAME_COUNTER);
    if (frameCounter === this.lastPresentedFrame) {
      return;
    }

    // If the buffer is shared, we can avoid an extra copy when the stride is
    // tightly packed. Otherwise, we copy row-by-row into a contiguous buffer.
    this.blitToSourceCanvas(shared.pixelsU8Clamped);
    this.drawToCanvas();
    this.lastPresentedFrame = frameCounter;
  }

  presentCopy(frame: FramebufferCopyFrame): void {
    if (frame.format !== FRAMEBUFFER_FORMAT_RGBA8888) {
      return;
    }

    if (frame.frameCounter === this.lastPresentedFrame) {
      return;
    }

    if (frame.width !== this.srcWidth || frame.height !== this.srcHeight || frame.strideBytes !== this.srcStrideBytes) {
      this.reconfigureSource(frame.width, frame.height, frame.strideBytes, frame.format);
    }

    const u8c = frame.pixelsU8 instanceof Uint8ClampedArray ? frame.pixelsU8 : new Uint8ClampedArray(frame.pixelsU8.buffer, frame.pixelsU8.byteOffset, frame.pixelsU8.byteLength);
    this.blitToSourceCanvas(u8c);
    this.drawToCanvas();
    this.lastPresentedFrame = frame.frameCounter;
  }

  syncCanvasBackingStoreToClient(): void {
    if (!this.isHtmlCanvas()) return;
    const canvas = this.canvas as HTMLCanvasElement;

    const dpr = typeof devicePixelRatio === "number" && Number.isFinite(devicePixelRatio) ? devicePixelRatio : 1;
    const cssWidth = canvas.clientWidth;
    const cssHeight = canvas.clientHeight;
    if (cssWidth <= 0 || cssHeight <= 0) return;

    const width = Math.max(1, Math.floor(cssWidth * dpr));
    const height = Math.max(1, Math.floor(cssHeight * dpr));
    if (canvas.width !== width) canvas.width = width;
    if (canvas.height !== height) canvas.height = height;
  }

  reconfigureSource(width: number, height: number, strideBytes: number, format: number): void {
    width = clampPositiveInt("width", width);
    height = clampPositiveInt("height", height);
    strideBytes = clampPositiveInt("strideBytes", strideBytes);

    if (strideBytes < width * 4) {
      throw new Error(`Invalid strideBytes ${strideBytes} for width ${width}`);
    }

    this.srcWidth = width;
    this.srcHeight = height;
    this.srcStrideBytes = strideBytes;
    this.srcFormat = format;

    this.srcCanvas = createScratchCanvas(width, height);
    this.srcCtx = get2dContext(this.srcCanvas);

    // `ImageData` does not accept SharedArrayBuffer-backed views, so always keep
    // a private, tightly-packed ArrayBuffer-backed copy here.
    const imageBytes = new Uint8ClampedArray(width * height * 4) as Uint8ClampedArray<ArrayBuffer>;
    this.srcImageBytes = imageBytes;
    this.srcImageData = new ImageData(imageBytes, width, height);
  }

  blitToSourceCanvas(pixels: Uint8ClampedArray): void {
    if (!this.srcCanvas || !this.srcCtx || !this.srcImageData || !this.srcImageBytes) {
      return;
    }

    const width = this.srcWidth;
    const height = this.srcHeight;
    const strideBytes = this.srcStrideBytes;
    if (width <= 0 || height <= 0) return;

    const tightlyPacked = strideBytes === width * 4;
    if (!tightlyPacked) {
      const rowBytes = width * 4;
      for (let y = 0; y < height; y++) {
        const srcStart = y * strideBytes;
        const dstStart = y * rowBytes;
        this.srcImageBytes.set(pixels.subarray(srcStart, srcStart + rowBytes), dstStart);
      }
    } else {
      this.srcImageBytes.set(pixels.subarray(0, width * height * 4));
    }

    // `ImageData` / Canvas2D expects sRGB-encoded bytes. Treat the VGA framebuffer bytes as
    // linear RGBA8 and encode to sRGB for presentation.
    encodeLinearRgba8ToSrgbInPlace(
      new Uint8Array(this.srcImageBytes.buffer, this.srcImageBytes.byteOffset, this.srcImageBytes.byteLength),
    );

    this.srcCtx.putImageData(this.srcImageData, 0, 0);
  }

  drawToCanvas(): void {
    if (!this.srcCanvas) return;

    const ctx = this.ctx;
    const dstW = this.canvas.width;
    const dstH = this.canvas.height;
    if (dstW <= 0 || dstH <= 0) return;

    const srcW = this.srcWidth;
    const srcH = this.srcHeight;
    if (srcW <= 0 || srcH <= 0) return;

    const scaleMode = this.options.scaleMode;
    const usePixelated = scaleMode === "pixelated" || (scaleMode === "auto" && srcW <= 640 && srcH <= 480);
    ctx.imageSmoothingEnabled = !usePixelated;

    if (typeof ctx.imageSmoothingQuality === "string") {
      ctx.imageSmoothingQuality = usePixelated ? "low" : "high";
    }

    let scale = Math.min(dstW / srcW, dstH / srcH);
    if (usePixelated && this.options.integerScaling && dstW >= srcW && dstH >= srcH) {
      scale = Math.max(1, Math.floor(scale));
    }

    const drawW = Math.floor(srcW * scale);
    const drawH = Math.floor(srcH * scale);
    const x = Math.floor((dstW - drawW) / 2);
    const y = Math.floor((dstH - drawH) / 2);

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = this.options.clearColor;
    ctx.fillRect(0, 0, dstW, dstH);
    ctx.drawImage(this.srcCanvas, 0, 0, srcW, srcH, x, y, drawW, drawH);

    if (this.isHtmlCanvas()) {
      (this.canvas as HTMLCanvasElement).style.imageRendering = usePixelated ? "pixelated" : "auto";
    }
  }
}

/**
 * Creates a presenter in a worker by accepting an OffscreenCanvas and a
 * SharedArrayBuffer-backed framebuffer view.
 *
 * @param {OffscreenCanvas} canvas
 * @param {SharedFramebufferView} shared
 * @param {VgaPresenterOptions} [options]
 */
export function createWorkerPresenter(
  canvas: OffscreenCanvas,
  shared: SharedFramebufferView,
  options?: VgaPresenterOptions,
): VgaPresenter {
  const presenter = new VgaPresenter(canvas, options);
  presenter.setSharedFramebuffer(shared);
  presenter.start();
  return presenter;
}
