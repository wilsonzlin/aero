// JavaScript copy of `web/src/display/vga_presenter.ts` for the standalone
// browser harness.

import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  HEADER_INDEX_CONFIG_COUNTER,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_FORMAT,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
  loadHeaderI32,
} from "./framebuffer_protocol.js";

function hasRaf() {
  return typeof requestAnimationFrame === "function";
}

function cancelRaf(id) {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(id);
  }
}

function clampPositiveInt(name, value) {
  if (!Number.isFinite(value) || value <= 0) {
    throw new Error(`Invalid ${name}: ${value}`);
  }
  return value;
}

function get2dContext(canvas) {
  const ctx = canvas.getContext("2d", { alpha: false });
  if (!ctx) {
    throw new Error("2D canvas context not available");
  }
  return ctx;
}

function createScratchCanvas(width, height) {
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

export class VgaPresenter {
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    this.ctx = get2dContext(canvas);

    this.options = {
      scaleMode: options.scaleMode ?? "auto",
      integerScaling: options.integerScaling ?? true,
      autoResizeToClient: options.autoResizeToClient ?? true,
      maxPresentHz: options.maxPresentHz ?? 60,
      clearColor: options.clearColor ?? "#000",
    };

    this.shared = null;
    this.copy = null;

    this.running = false;
    this.lastPresentedFrame = -1;
    this.lastConfigCounter = -1;
    this.lastPresentTime = 0;
    this.timerHandle = null;
    this.rafHandle = null;

    this.srcCanvas = null;
    this.srcCtx = null;
    this.srcImageData = null;
    this.srcImageBytes = null;
    this.srcWidth = 0;
    this.srcHeight = 0;
    this.srcStrideBytes = 0;
    this.srcFormat = 0;

    this.resizeObserver = null;
    if (typeof ResizeObserver !== "undefined" && this.isHtmlCanvas() && this.options.autoResizeToClient) {
      this.resizeObserver = new ResizeObserver(() => this.syncCanvasBackingStoreToClient());
      this.resizeObserver.observe(this.canvas);
      this.syncCanvasBackingStoreToClient();
    }
  }

  isHtmlCanvas() {
    return typeof HTMLCanvasElement !== "undefined" && this.canvas instanceof HTMLCanvasElement;
  }

  destroy() {
    this.stop();
    if (this.resizeObserver) {
      this.resizeObserver.disconnect();
      this.resizeObserver = null;
    }
  }

  start() {
    if (this.running) return;
    this.running = true;
    this.lastPresentTime = performance.now ? performance.now() : Date.now();
    this.scheduleNextTick();
  }

  stop() {
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

  setSharedFramebuffer(shared) {
    this.shared = shared;
    this.copy = null;
    this.lastPresentedFrame = -1;
    this.lastConfigCounter = -1;
  }

  pushCopyFrame(frame) {
    this.copy = frame;
    this.shared = null;
  }

  scheduleNextTick() {
    if (!this.running) return;

    const intervalMs = 1000 / clampPositiveInt("maxPresentHz", this.options.maxPresentHz);
    const now = performance.now ? performance.now() : Date.now();
    const elapsed = now - this.lastPresentTime;
    const delay = Math.max(0, intervalMs - elapsed);

    if (hasRaf()) {
      this.rafHandle = requestAnimationFrame(() => this.tick());
      return;
    }

    this.timerHandle = setTimeout(() => this.tick(), delay);
  }

  tick() {
    if (!this.running) return;

    this.lastPresentTime = performance.now ? performance.now() : Date.now();

    try {
      this.presentLatestFrame();
    } finally {
      this.scheduleNextTick();
    }
  }

  presentLatestFrame() {
    if (this.shared) {
      this.presentShared(this.shared);
      return;
    }
    if (this.copy) {
      this.presentCopy(this.copy);
    }
  }

  presentShared(shared) {
    const header = shared.header;

    const format = loadHeaderI32(header, HEADER_INDEX_FORMAT);
    if (format !== FRAMEBUFFER_FORMAT_RGBA8888) {
      return;
    }

    const configCounter = loadHeaderI32(header, HEADER_INDEX_CONFIG_COUNTER);
    if (configCounter !== this.lastConfigCounter) {
      const width = loadHeaderI32(header, HEADER_INDEX_WIDTH);
      const height = loadHeaderI32(header, HEADER_INDEX_HEIGHT);
      const strideBytes = loadHeaderI32(header, HEADER_INDEX_STRIDE_BYTES);
      this.reconfigureSource(width, height, strideBytes, format, shared.pixelsU8Clamped);
      this.lastConfigCounter = configCounter;
    }

    const frameCounter = loadHeaderI32(header, HEADER_INDEX_FRAME_COUNTER);
    if (frameCounter === this.lastPresentedFrame) {
      return;
    }

    this.blitToSourceCanvas(shared.pixelsU8Clamped);
    this.drawToCanvas();
    this.lastPresentedFrame = frameCounter;
  }

  presentCopy(frame) {
    if (frame.format !== FRAMEBUFFER_FORMAT_RGBA8888) {
      return;
    }

    if (frame.frameCounter === this.lastPresentedFrame) {
      return;
    }

    if (frame.width !== this.srcWidth || frame.height !== this.srcHeight || frame.strideBytes !== this.srcStrideBytes) {
      this.reconfigureSource(frame.width, frame.height, frame.strideBytes, frame.format, null);
    }

    const u8c =
      frame.pixelsU8 instanceof Uint8ClampedArray
        ? frame.pixelsU8
        : new Uint8ClampedArray(frame.pixelsU8.buffer, frame.pixelsU8.byteOffset, frame.pixelsU8.byteLength);
    this.blitToSourceCanvas(u8c);
    this.drawToCanvas();
    this.lastPresentedFrame = frame.frameCounter;
  }

  syncCanvasBackingStoreToClient() {
    if (!this.isHtmlCanvas()) return;

    const dpr = typeof devicePixelRatio === "number" && Number.isFinite(devicePixelRatio) ? devicePixelRatio : 1;
    const cssWidth = this.canvas.clientWidth;
    const cssHeight = this.canvas.clientHeight;
    if (cssWidth <= 0 || cssHeight <= 0) return;

    const width = Math.max(1, Math.floor(cssWidth * dpr));
    const height = Math.max(1, Math.floor(cssHeight * dpr));
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
  }

  reconfigureSource(width, height, strideBytes, format, sharedPixelsOrNull) {
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

    const tightlyPacked = strideBytes === width * 4;
    if (tightlyPacked && sharedPixelsOrNull) {
      this.srcImageBytes = sharedPixelsOrNull.subarray(0, width * height * 4);
    } else {
      this.srcImageBytes = new Uint8ClampedArray(width * height * 4);
    }

    this.srcImageData = new ImageData(this.srcImageBytes, width, height);
  }

  blitToSourceCanvas(pixels) {
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
    } else if (this.srcImageBytes.buffer !== pixels.buffer || this.srcImageBytes.byteOffset !== pixels.byteOffset) {
      this.srcImageBytes.set(pixels.subarray(0, width * height * 4));
    }

    this.srcCtx.putImageData(this.srcImageData, 0, 0);
  }

  drawToCanvas() {
    if (!this.srcCanvas) return;

    const ctx = this.ctx;
    const dstW = this.canvas.width;
    const dstH = this.canvas.height;
    if (dstW <= 0 || dstH <= 0) return;

    const srcW = this.srcWidth;
    const srcH = this.srcHeight;
    if (srcW <= 0 || srcH <= 0) return;

    const scaleMode = this.options.scaleMode ?? "auto";
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
    ctx.fillStyle = this.options.clearColor ?? "#000";
    ctx.fillRect(0, 0, dstW, dstH);
    ctx.drawImage(this.srcCanvas, 0, 0, srcW, srcH, x, y, drawW, drawH);

    if (this.isHtmlCanvas()) {
      this.canvas.style.imageRendering = usePixelated ? "pixelated" : "auto";
    }
  }
}

