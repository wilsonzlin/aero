import {
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from "../ipc/shared-layout";

export type SharedLayoutPresenterOptions = {
  /**
   * Maximum present rate when driven by requestAnimationFrame.
   *
   * Defaults to 60Hz.
   */
  maxPresentHz?: number;
};

function hasRaf(): boolean {
  return typeof requestAnimationFrame === "function";
}

function cancelRaf(id: number): void {
  if (typeof cancelAnimationFrame === "function") {
    cancelAnimationFrame(id);
  }
}

/**
 * Minimal main-thread presenter for the tile-based shared-layout framebuffer protocol.
 *
 * This is used as a fallback when OffscreenCanvas transfer is unavailable and the GPU worker
 * cannot own presentation.
 */
export class SharedLayoutPresenter {
  private readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly options: Required<SharedLayoutPresenterOptions>;

  private running = false;
  private rafHandle: number | null = null;
  private nextPresentTimeMs = 0;

  private sab: SharedArrayBuffer | null = null;
  private offsetBytes = 0;

  private header: Int32Array | null = null;
  private layout: SharedFramebufferLayout | null = null;
  private slot0: Uint8Array | null = null;
  private slot1: Uint8Array | null = null;
  private imageData: ImageData | null = null;

  private lastPresentedSeq = -1;

  constructor(canvas: HTMLCanvasElement, options: SharedLayoutPresenterOptions = {}) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d", { alpha: false }) as CanvasRenderingContext2D | null;
    if (!ctx) {
      throw new Error("2D canvas context not available");
    }
    this.ctx = ctx;
    this.ctx.imageSmoothingEnabled = false;
    this.options = {
      maxPresentHz: options.maxPresentHz ?? 60,
    };
  }

  destroy(): void {
    this.stop();
  }

  setSharedFramebuffer(framebuffer: { sab: SharedArrayBuffer; offsetBytes: number } | null): void {
    this.sab = framebuffer?.sab ?? null;
    this.offsetBytes = framebuffer?.offsetBytes ?? 0;
    this.header = null;
    this.layout = null;
    this.slot0 = null;
    this.slot1 = null;
    this.imageData = null;
    this.lastPresentedSeq = -1;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.nextPresentTimeMs = 0;
    this.scheduleNextTick();
  }

  stop(): void {
    this.running = false;
    if (this.rafHandle != null) {
      cancelRaf(this.rafHandle);
      this.rafHandle = null;
    }
  }

  private scheduleNextTick(): void {
    if (!this.running) return;
    if (!hasRaf()) return;
    this.rafHandle = requestAnimationFrame((t) => this.tick(t));
  }

  private tick(frameTimeMs: number): void {
    if (!this.running) return;

    const maxHz = this.options.maxPresentHz;
    const intervalMs = maxHz > 0 ? 1000 / maxHz : 0;

    if (intervalMs > 0) {
      if (this.nextPresentTimeMs !== 0 && frameTimeMs < this.nextPresentTimeMs) {
        this.scheduleNextTick();
        return;
      }
      if (this.nextPresentTimeMs === 0 || frameTimeMs - this.nextPresentTimeMs > intervalMs) {
        this.nextPresentTimeMs = frameTimeMs + intervalMs;
      } else {
        this.nextPresentTimeMs += intervalMs;
      }
    }

    this.presentLatestFrame();
    this.scheduleNextTick();
  }

  private refreshViews(): void {
    const sab = this.sab;
    if (!sab) return;
    const offsetBytes = this.offsetBytes | 0;
    if (offsetBytes < 0 || offsetBytes + 8 > sab.byteLength) return;

    const header2 = new Int32Array(sab, offsetBytes, 2);
    const magic = Atomics.load(header2, 0);
    const version = Atomics.load(header2, 1);
    if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) return;

    const header = new Int32Array(sab, offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    try {
      const layout = layoutFromHeader(header);

      const key = `${layout.width},${layout.height},${layout.strideBytes},${layout.tileSize},${layout.dirtyWordsPerBuffer}`;
      const prev = this.layout;
      const prevKey =
        prev === null ? null : `${prev.width},${prev.height},${prev.strideBytes},${prev.tileSize},${prev.dirtyWordsPerBuffer}`;
      if (this.header && this.layout && prevKey === key) return;

      this.header = header;
      this.layout = layout;
      this.slot0 = new Uint8Array(sab, offsetBytes + layout.framebufferOffsets[0], layout.strideBytes * layout.height);
      this.slot1 = new Uint8Array(sab, offsetBytes + layout.framebufferOffsets[1], layout.strideBytes * layout.height);
      this.lastPresentedSeq = -1;

      if (this.canvas.width !== layout.width) this.canvas.width = layout.width;
      if (this.canvas.height !== layout.height) this.canvas.height = layout.height;
      this.ctx.imageSmoothingEnabled = false;

      if (!this.imageData || this.imageData.width !== layout.width || this.imageData.height !== layout.height) {
        this.imageData = new ImageData(layout.width, layout.height);
      }
    } catch {
      // Header likely not initialized yet.
      return;
    }
  }

  private presentLatestFrame(): void {
    this.refreshViews();
    const header = this.header;
    const layout = this.layout;
    const slot0 = this.slot0;
    const slot1 = this.slot1;
    const image = this.imageData;
    if (!header || !layout || !slot0 || !slot1 || !image) return;

    const seq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
    if (seq === this.lastPresentedSeq) return;

    const active = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
    const src = active === 0 ? slot0 : slot1;

    const rowBytes = layout.width * 4;
    const dst = image.data;

    if (layout.strideBytes === rowBytes) {
      dst.set(src.subarray(0, rowBytes * layout.height));
    } else {
      for (let y = 0; y < layout.height; y += 1) {
        const srcOff = y * layout.strideBytes;
        const dstOff = y * rowBytes;
        dst.set(src.subarray(srcOff, srcOff + rowBytes), dstOff);
      }
    }

    this.ctx.putImageData(image, 0, 0);
    this.lastPresentedSeq = seq;
  }
}

