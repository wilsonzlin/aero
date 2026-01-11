import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';

import initAeroGpuWasm, {
  destroy_gpu,
  init_gpu,
  present_rgba8888,
  request_screenshot,
  resize as resize_gpu,
} from '../wasm/aero-gpu';

export class WgpuWebGl2Presenter implements Presenter {
  public readonly backend = 'webgl2_wgpu' as const;

  private opts: PresenterInitOptions = {};
  private srcWidth = 0;
  private srcHeight = 0;
  private dpr = 1;
  private initialized = false;

  public async init(canvas: OffscreenCanvas, width: number, height: number, dpr: number, opts?: PresenterInitOptions): Promise<void> {
    this.opts = opts ?? {};
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr || 1;

    await initAeroGpuWasm();

    // Ensure stale state from a previous init is cleared before creating the new surface.
    try {
      destroy_gpu();
    } catch {
      // Ignore; module may not have been initialized yet.
    }

    try {
      await init_gpu(canvas, width, height, this.dpr, {
        // Force the wgpu GL backend (WebGL2).
        preferWebGpu: false,
        disableWebGpu: true,
        ...(this.opts.outputWidth != null ? { outputWidth: this.opts.outputWidth } : {}),
        ...(this.opts.outputHeight != null ? { outputHeight: this.opts.outputHeight } : {}),
        ...(this.opts.scaleMode != null ? { scaleMode: this.opts.scaleMode } : {}),
        ...(this.opts.filter != null ? { filter: this.opts.filter } : {}),
        ...(this.opts.clearColor != null ? { clearColor: this.opts.clearColor } : {}),
      });
    } catch (err) {
      throw new PresenterError('wgpu_init_failed', 'Failed to initialize wgpu WebGL2 presenter', err);
    }

    this.initialized = true;
  }

  public resize(width: number, height: number, dpr: number): void {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.resize() called before init()');
    }

    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr || 1;

    try {
      const outputWidth = this.opts.outputWidth;
      const outputHeight = this.opts.outputHeight;
      resize_gpu(width, height, this.dpr, outputWidth, outputHeight);
    } catch (err) {
      throw new PresenterError('wgpu_resize_failed', 'Failed to resize wgpu WebGL2 presenter', err);
    }
  }

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.present() called before init()');
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `present() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError('invalid_stride', `present() stride (${stride}) smaller than width*4 (${tightRowBytes})`);
    }

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    try {
      present_rgba8888(data, stride);
    } catch (err) {
      throw new PresenterError('wgpu_present_failed', 'Failed to present frame via wgpu WebGL2 presenter', err);
    }
  }

  public async screenshot(): Promise<PresenterScreenshot> {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.screenshot() called before init()');
    }

    try {
      const bytes = await request_screenshot();
      // wasm-bindgen models typed array buffers as `ArrayBufferLike`, but the
      // screenshot contract expects an ArrayBuffer. Copy if needed.
      const pixels: ArrayBuffer =
        bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength
          ? bytes.buffer
          : (() => {
              const out = new Uint8Array(bytes.byteLength);
              out.set(new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength));
              return out.buffer;
            })();
      return { width: this.srcWidth, height: this.srcHeight, pixels };
    } catch (err) {
      throw new PresenterError('wgpu_screenshot_failed', 'Failed to capture screenshot from wgpu WebGL2 presenter', err);
    }
  }

  public destroy(): void {
    if (!this.initialized) return;
    try {
      destroy_gpu();
    } catch {
      // Ignore; best-effort cleanup.
    } finally {
      this.initialized = false;
    }
  }

  private resolveFrameData(frame: number | ArrayBuffer | ArrayBufferView, byteLength: number): Uint8Array {
    if (typeof frame === 'number') {
      const memory = this.opts.wasmMemory;
      if (!memory) {
        throw new PresenterError(
          'missing_wasm_memory',
          'present() called with a pointer but init opts did not include wasmMemory',
        );
      }
      const buf = memory.buffer;
      if (frame < 0 || frame + byteLength > buf.byteLength) {
        throw new PresenterError(
          'wasm_oob',
          `present() pointer range [${frame}, ${frame + byteLength}) is outside wasm memory (size=${buf.byteLength})`,
        );
      }
      return new Uint8Array(buf, frame, byteLength);
    }

    if (frame instanceof ArrayBuffer) {
      if (frame.byteLength < byteLength) {
        throw new PresenterError(
          'frame_too_small',
          `present() buffer too small: expected at least ${byteLength} bytes, got ${frame.byteLength}`,
        );
      }
      return new Uint8Array(frame, 0, byteLength);
    }

    const view = frame as ArrayBufferView;
    if (view.byteLength < byteLength) {
      throw new PresenterError(
        'frame_too_small',
        `present() view too small: expected at least ${byteLength} bytes, got ${view.byteLength}`,
      );
    }
    return new Uint8Array(view.buffer, view.byteOffset, byteLength);
  }
}
