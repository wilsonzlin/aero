import blitVertSource from './shaders/blit.vert.glsl?raw';
import blitFragSource from './shaders/blit.frag.glsl?raw';
import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';
import { computeViewport } from './viewport';

type DirtyRect = { x: number; y: number; w: number; h: number };

const DEFAULT_CLEAR_COLOR: [number, number, number, number] = [0, 0, 0, 1];

function srgbEncodeChannel(x: number): number {
  const v = Math.min(1, Math.max(0, x));
  if (v <= 0.0031308) return v * 12.92;
  return 1.055 * Math.pow(v, 1.0 / 2.4) - 0.055;
}
function flipImageVertically(pixels: Uint8Array, width: number, height: number): Uint8Array {
  const rowBytes = width * 4;
  const out = new Uint8Array(pixels.length);
  for (let y = 0; y < height; y++) {
    const srcOff = (height - 1 - y) * rowBytes;
    const dstOff = y * rowBytes;
    out.set(pixels.subarray(srcOff, srcOff + rowBytes), dstOff);
  }
  return out;
}

function glEnumToString(gl: WebGL2RenderingContext, value: number): string {
  // Best-effort mapping for debugging; falls back to numeric.
  const record = gl as unknown as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (typeof record[key] === 'number' && record[key] === value) return String(key);
  }
  return `0x${value.toString(16)}`;
}

function assertWebGlOk(gl: WebGL2RenderingContext, label: string): void {
  const err = gl.getError();
  if (err !== gl.NO_ERROR) {
    throw new PresenterError('webgl_error', `${label}: WebGL error ${glEnumToString(gl, err)} (${err})`);
  }
}

export class RawWebGl2Presenter implements Presenter {
  public readonly backend = 'webgl2_raw' as const;

  private canvas: OffscreenCanvas | null = null;
  private gl: WebGL2RenderingContext | null = null;
  private opts: PresenterInitOptions = {};

  private srcWidth = 0;
  private srcHeight = 0;
  private dpr = 1;

  private program: WebGLProgram | null = null;
  private vao: WebGLVertexArrayObject | null = null;
  private frameTexture: WebGLTexture | null = null;
  private cursorTexture: WebGLTexture | null = null;
  private uFrameLoc: WebGLUniformLocation | null = null;
  private uCursorLoc: WebGLUniformLocation | null = null;
  private uSrcSizeLoc: WebGLUniformLocation | null = null;
  private uCursorEnableLoc: WebGLUniformLocation | null = null;
  private uCursorPosLoc: WebGLUniformLocation | null = null;
  private uCursorHotLoc: WebGLUniformLocation | null = null;
  private uCursorSizeLoc: WebGLUniformLocation | null = null;
  private uForceOpaqueAlphaLoc: WebGLUniformLocation | null = null;

  private isContextLost = false;
  private onContextLost: ((ev: Event) => void) | null = null;
  private onContextRestored: ((ev: Event) => void) | null = null;

  private cursorImage: Uint8Array | null = null;
  private cursorWidth = 0;
  private cursorHeight = 0;
  private cursorEnabled = false;
  private cursorRenderEnabled = true;
  private cursorX = 0;
  private cursorY = 0;
  private cursorHotX = 0;
  private cursorHotY = 0;

  public init(canvas: OffscreenCanvas, width: number, height: number, dpr: number, opts?: PresenterInitOptions): void {
    this.canvas = canvas;
    this.opts = opts ?? {};
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr;

    const outputWidth = this.opts.outputWidth ?? width;
    const outputHeight = this.opts.outputHeight ?? height;
    this.resizeCanvas(outputWidth, outputHeight, dpr);

    const gl = canvas.getContext('webgl2', {
      // Keep the default "opaque" behavior unless explicitly requested. Some diagnostics pages
      // want to visualize alpha semantics by letting the page background show through.
      alpha: this.opts.canvasAlpha === true,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer: false,
    }) as WebGL2RenderingContext | null;

    if (!gl) {
      throw new PresenterError(
        'webgl2_unavailable',
        'Failed to create a WebGL2 context. This backend requires WebGL2 support (including in workers via OffscreenCanvas).',
      );
    }

    this.gl = gl;

    // Events are dispatched on the canvas.
    const onContextLost = (ev: Event) => {
      // A context loss is recoverable only if we preventDefault.
      ev.preventDefault();
      this.isContextLost = true;
      this.opts.onError?.(new PresenterError('webgl_context_lost', 'WebGL context lost'));
    };
    const onContextRestored = (_ev: Event) => {
      this.isContextLost = false;
      try {
        this.recreateResources();
      } catch (err) {
        this.opts.onError?.(
          new PresenterError('webgl_context_restore_failed', 'WebGL context restored but presenter re-init failed', err),
        );
      }
    };
    this.onContextLost = onContextLost;
    this.onContextRestored = onContextRestored;

    try {
      const target = canvas as unknown as EventTarget;
      target.addEventListener('webglcontextlost', onContextLost, { passive: false });
      target.addEventListener('webglcontextrestored', onContextRestored);
    } catch {
      // Some OffscreenCanvas implementations do not expose these events; ignore.
    }

    this.recreateResources();
  }

  public resize(width: number, height: number, dpr: number): void {
    if (!this.canvas || !this.gl) {
      throw new PresenterError('not_initialized', 'RawWebGl2Presenter.resize() called before init()');
    }
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr;

    const outputWidth = this.opts.outputWidth ?? width;
    const outputHeight = this.opts.outputHeight ?? height;
    this.resizeCanvas(outputWidth, outputHeight, dpr);

    if (!this.isContextLost) {
      this.resizeTexture(width, height);
    }
  }

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void {
    this.presentInternal(frame, stride, null);
  }

  public presentDirtyRects(frame: number | ArrayBuffer | ArrayBufferView, stride: number, dirtyRects: DirtyRect[]): void {
    this.presentInternal(frame, stride, dirtyRects);
  }

  private presentInternal(
    frame: number | ArrayBuffer | ArrayBufferView,
    stride: number,
    dirtyRects: DirtyRect[] | null,
  ): void {
    const gl = this.gl;
    if (!this.canvas || !gl || !this.program || !this.vao || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'RawWebGl2Presenter.present() called before init()');
    }
    if (this.isContextLost) return;

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `present() stride must be > 0; got ${stride}`);
    }
    if (stride % 4 !== 0) {
      throw new PresenterError('invalid_stride', `present() stride must be divisible by 4 for RGBA8; got ${stride}`);
    }

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.frameTexture);

    // Allow tight packing regardless of stride padding.
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, stride / 4);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, 0);

    if (!dirtyRects || dirtyRects.length === 0) {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, this.srcWidth, this.srcHeight, gl.RGBA, gl.UNSIGNED_BYTE, data);
    } else {
      for (const rect of dirtyRects) {
        const x = Math.max(0, rect.x | 0);
        const y = Math.max(0, rect.y | 0);
        let w = Math.max(0, rect.w | 0);
        let h = Math.max(0, rect.h | 0);
        if (x >= this.srcWidth || y >= this.srcHeight) continue;
        if (x + w > this.srcWidth) w = Math.max(0, this.srcWidth - x);
        if (y + h > this.srcHeight) h = Math.max(0, this.srcHeight - y);
        if (w === 0 || h === 0) continue;

        gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, x);
        gl.pixelStorei(gl.UNPACK_SKIP_ROWS, y);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, w, h, gl.RGBA, gl.UNSIGNED_BYTE, data);
      }

      // Reset state that can surprise other code paths sharing the context.
      gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 0);
      gl.pixelStorei(gl.UNPACK_SKIP_ROWS, 0);
    }

    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);

    assertWebGlOk(gl, 'texSubImage2D');

    this.draw();
  }

  public setCursorImageRgba8(rgba: Uint8Array, width: number, height: number): void {
    const w = Math.max(0, width | 0);
    const h = Math.max(0, height | 0);
    if (w === 0 || h === 0) {
      throw new PresenterError('invalid_cursor_image', 'cursor width/height must be non-zero');
    }
    const required = w * h * 4;
    if (rgba.byteLength < required) {
      throw new PresenterError(
        'invalid_cursor_image',
        `cursor RGBA buffer too small: expected at least ${required} bytes, got ${rgba.byteLength}`,
      );
    }

    this.cursorImage = rgba;
    this.cursorWidth = w;
    this.cursorHeight = h;
    this.uploadCursorTexture();
  }

  public setCursorState(enabled: boolean, x: number, y: number, hotX: number, hotY: number): void {
    this.cursorEnabled = !!enabled;
    this.cursorX = x | 0;
    this.cursorY = y | 0;
    this.cursorHotX = Math.max(0, hotX | 0);
    this.cursorHotY = Math.max(0, hotY | 0);
  }

  public setCursorRenderEnabled(enabled: boolean): void {
    this.cursorRenderEnabled = !!enabled;
  }

  public redraw(): void {
    this.draw();
  }

  /**
   * Debug-only: read back the *presented* canvas pixels as RGBA8 (top-left origin).
   *
   * Unlike `screenshot()`, which is defined as a readback of the source framebuffer,
   * this captures the final post-blit output (including sRGB encode policy, opaque alpha,
   * scaling/letterboxing, and cursor composition).
   */
  public screenshotPresented(): PresenterScreenshot {
    const gl = this.gl;
    const canvas = this.canvas;
    if (!canvas || !gl || !this.program || !this.vao || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'RawWebGl2Presenter.screenshotPresented() called before init()');
    }
    if (this.isContextLost) {
      throw new PresenterError('webgl_context_lost', 'Cannot take presented screenshot while WebGL context is lost');
    }

    // Ensure we have a fresh draw reflecting the latest cursor state / uniforms.
    this.draw();

    const w = canvas.width;
    const h = canvas.height;
    const pixels = new Uint8Array(w * h * 4);
    const prevPackBuffer = gl.getParameter(gl.PIXEL_PACK_BUFFER_BINDING) as WebGLBuffer | null;
    const prevPackAlignment = gl.getParameter(gl.PACK_ALIGNMENT) as number;
    const prevPackRowLength = gl.getParameter(gl.PACK_ROW_LENGTH) as number;
    const prevPackSkipPixels = gl.getParameter(gl.PACK_SKIP_PIXELS) as number;
    const prevPackSkipRows = gl.getParameter(gl.PACK_SKIP_ROWS) as number;
    try {
      // Ensure readPixels writes into client memory with a tight, top-left-oriented packing.
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, null);
      gl.pixelStorei(gl.PACK_ALIGNMENT, 1);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, 0);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, 0);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, 0);
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      assertWebGlOk(gl, 'readPixels');
    } finally {
      gl.pixelStorei(gl.PACK_ALIGNMENT, prevPackAlignment);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, prevPackRowLength);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, prevPackSkipPixels);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, prevPackSkipRows);
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, prevPackBuffer);
    }

    // WebGL readPixels has a bottom-left origin; convert to top-left for stable hashing.
    const flipped = flipImageVertically(pixels, w, h);
    return { width: w, height: h, pixels: flipped.buffer as ArrayBuffer };
  }

  // Screenshot reads back the source frame texture (not the presented/canvas output).
  // This avoids scaling/color-space ambiguity and matches the deterministic hashing
  // contract described by `PresenterScreenshot`.
  public screenshot(): PresenterScreenshot {
    const gl = this.gl;
    const canvas = this.canvas;
    if (!canvas || !gl || !this.program || !this.vao || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'RawWebGl2Presenter.screenshot() called before init()');
    }
    if (this.isContextLost) {
      throw new PresenterError('webgl_context_lost', 'Cannot take screenshot while WebGL context is lost');
    }

    // Screenshot contract: return the *source* texture bytes (RGBA8) with a top-left
    // origin. Do not read back from the default framebuffer; the presentation shader
    // applies manual sRGB encoding + cursor composition which would break deterministic
    // screenshot hashing tests.
    const w = this.srcWidth;
    const h = this.srcHeight;

    const pixels = new Uint8Array(w * h * 4);

    // Read back from an FBO backed by the source frame texture.
    const prevReadFbo = gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) as WebGLFramebuffer | null;
    const prevDrawFbo = gl.getParameter(gl.DRAW_FRAMEBUFFER_BINDING) as WebGLFramebuffer | null;
    const prevReadBuffer = gl.getParameter(gl.READ_BUFFER) as number;
    const prevPackBuffer = gl.getParameter(gl.PIXEL_PACK_BUFFER_BINDING) as WebGLBuffer | null;
    const prevPackAlignment = gl.getParameter(gl.PACK_ALIGNMENT) as number;
    const prevPackRowLength = gl.getParameter(gl.PACK_ROW_LENGTH) as number;
    const prevPackSkipPixels = gl.getParameter(gl.PACK_SKIP_PIXELS) as number;
    const prevPackSkipRows = gl.getParameter(gl.PACK_SKIP_ROWS) as number;

    const fbo = gl.createFramebuffer();
    if (!fbo) {
      throw new PresenterError('webgl_resource_failed', 'Failed to create framebuffer for screenshot readback');
    }

    try {
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(gl.READ_FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, this.frameTexture, 0);
      gl.readBuffer(gl.COLOR_ATTACHMENT0);

      const status = gl.checkFramebufferStatus(gl.READ_FRAMEBUFFER);
      if (status !== gl.FRAMEBUFFER_COMPLETE) {
        throw new PresenterError(
          'webgl_framebuffer_incomplete',
          `Screenshot framebuffer incomplete: ${glEnumToString(gl, status)} (${status})`,
        );
      }

      // Ensure readPixels writes into client memory with a tight packing.
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, null);
      gl.pixelStorei(gl.PACK_ALIGNMENT, 1);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, 0);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, 0);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, 0);
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
      assertWebGlOk(gl, 'readPixels');
    } finally {
      // Restore state to avoid leaking FBO bindings into future draws.
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, prevReadFbo);
      gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, prevDrawFbo);
      gl.pixelStorei(gl.PACK_ALIGNMENT, prevPackAlignment);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, prevPackRowLength);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, prevPackSkipPixels);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, prevPackSkipRows);
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, prevPackBuffer);
      try {
        gl.readBuffer(prevReadBuffer);
      } catch {
        // Ignore; some browsers are strict about readBuffer when switching between
        // default framebuffers and FBOs.
      }
      gl.deleteFramebuffer(fbo);
    }

    // With our "top-left UV + UNPACK_FLIP_Y_WEBGL=0" convention, the vertical flip
    // applied during texture upload cancels out WebGL's bottom-left readPixels origin,
    // so the returned bytes already match the caller's top-to-bottom row order.
    return { width: w, height: h, pixels: pixels.buffer as ArrayBuffer };
  }

  public destroy(): void {
    const gl = this.gl;
    const canvas = this.canvas;
    if (canvas) {
      try {
        const target = canvas as unknown as EventTarget;
        if (this.onContextLost) target.removeEventListener('webglcontextlost', this.onContextLost);
        if (this.onContextRestored) target.removeEventListener('webglcontextrestored', this.onContextRestored);
      } catch {
        // Ignore.
      }
    }
    this.onContextLost = null;
    this.onContextRestored = null;

    if (gl) {
      if (this.program) gl.deleteProgram(this.program);
      if (this.vao) gl.deleteVertexArray(this.vao);
      if (this.frameTexture) gl.deleteTexture(this.frameTexture);
      if (this.cursorTexture) gl.deleteTexture(this.cursorTexture);
    }

    this.program = null;
    this.vao = null;
    this.frameTexture = null;
    this.cursorTexture = null;
    this.uFrameLoc = null;
    this.uCursorLoc = null;
    this.uSrcSizeLoc = null;
    this.uCursorEnableLoc = null;
    this.uCursorPosLoc = null;
    this.uCursorHotLoc = null;
    this.uCursorSizeLoc = null;
    this.uForceOpaqueAlphaLoc = null;
    this.gl = null;
    this.canvas = null;
  }

  /**
   * Dev-only helper: force a context loss using WEBGL_lose_context when available.
   *
   * Returns true when the extension was present and the request was issued.
   */
  public debugLoseContext(): boolean {
    const gl = this.gl;
    if (!gl) return false;
    const ext = gl.getExtension('WEBGL_lose_context') as unknown as { loseContext?: () => void } | null;
    if (!ext || typeof ext.loseContext !== 'function') return false;
    ext.loseContext();
    return true;
  }

  /**
   * Dev-only helper: attempt to restore a context previously lost via {@link debugLoseContext}.
   *
   * Returns true when the extension was present and the request was issued.
   */
  public debugRestoreContext(): boolean {
    const gl = this.gl;
    if (!gl) return false;
    const ext = gl.getExtension('WEBGL_lose_context') as unknown as { restoreContext?: () => void } | null;
    if (!ext || typeof ext.restoreContext !== 'function') return false;
    ext.restoreContext();
    return true;
  }

  private resizeCanvas(outputWidth: number, outputHeight: number, dpr: number): void {
    if (!this.canvas) return;
    const w = Math.max(1, Math.round(outputWidth * dpr));
    const h = Math.max(1, Math.round(outputHeight * dpr));
    this.canvas.width = w;
    this.canvas.height = h;
  }

  private recreateResources(): void {
    const gl = this.gl;
    if (!gl) return;

    // Clean up old resources if any (useful on context restore).
    if (this.program) gl.deleteProgram(this.program);
    if (this.vao) gl.deleteVertexArray(this.vao);
    if (this.frameTexture) gl.deleteTexture(this.frameTexture);
    if (this.cursorTexture) gl.deleteTexture(this.cursorTexture);

    this.program = this.createProgram(gl, blitVertSource, blitFragSource);
    this.vao = gl.createVertexArray();
    if (!this.vao) throw new PresenterError('webgl_resource_failed', 'Failed to create vertex array object');

    this.frameTexture = gl.createTexture();
    if (!this.frameTexture) throw new PresenterError('webgl_resource_failed', 'Failed to create frame texture');
    this.cursorTexture = gl.createTexture();
    if (!this.cursorTexture) throw new PresenterError('webgl_resource_failed', 'Failed to create cursor texture');

    gl.bindVertexArray(this.vao);

    gl.bindTexture(gl.TEXTURE_2D, this.frameTexture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    const filter = this.opts.filter ?? 'nearest';
    const glFilter = filter === 'linear' ? gl.LINEAR : gl.NEAREST;
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, glFilter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, glFilter);

    this.resizeTexture(this.srcWidth, this.srcHeight);

    gl.bindTexture(gl.TEXTURE_2D, this.cursorTexture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, glFilter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, glFilter);
    // Always allocate something so the sampler is complete.
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 1, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, new Uint8Array([0, 0, 0, 0]));

    gl.useProgram(this.program);
    this.uFrameLoc = gl.getUniformLocation(this.program, 'u_frame');
    if (!this.uFrameLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_frame uniform');
    gl.uniform1i(this.uFrameLoc, 0);

    this.uCursorLoc = gl.getUniformLocation(this.program, 'u_cursor');
    if (!this.uCursorLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_cursor uniform');
    gl.uniform1i(this.uCursorLoc, 1);

    this.uSrcSizeLoc = gl.getUniformLocation(this.program, 'u_src_size');
    if (!this.uSrcSizeLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_src_size uniform');
    this.uCursorEnableLoc = gl.getUniformLocation(this.program, 'u_cursor_enable');
    if (!this.uCursorEnableLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_cursor_enable uniform');
    this.uCursorPosLoc = gl.getUniformLocation(this.program, 'u_cursor_pos');
    if (!this.uCursorPosLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_cursor_pos uniform');
    this.uCursorHotLoc = gl.getUniformLocation(this.program, 'u_cursor_hot');
    if (!this.uCursorHotLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_cursor_hot uniform');
    this.uCursorSizeLoc = gl.getUniformLocation(this.program, 'u_cursor_size');
    if (!this.uCursorSizeLoc) throw new PresenterError('webgl_resource_failed', 'Failed to locate u_cursor_size uniform');
    this.uForceOpaqueAlphaLoc = gl.getUniformLocation(this.program, 'u_force_opaque_alpha');
    if (!this.uForceOpaqueAlphaLoc) {
      throw new PresenterError('webgl_resource_failed', 'Failed to locate u_force_opaque_alpha uniform');
    }

    // When canvas alpha is enabled, preserve the source alpha channel so diagnostics can
    // visualize transparency semantics. Otherwise, force opaque alpha to match the default
    // presentation policy.
    gl.uniform1i(this.uForceOpaqueAlphaLoc, this.opts.canvasAlpha === true ? 0 : 1);

    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.BLEND);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.DITHER);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.disable(gl.SAMPLE_ALPHA_TO_COVERAGE);
    gl.disable(gl.SAMPLE_COVERAGE);
    gl.colorMask(true, true, true, true);
    // Deterministic presentation: we do manual sRGB encoding in the blit shader, so ensure
    // fixed-function framebuffer sRGB conversion (when present) is disabled to avoid double-gamma.
    const srgbWriteControl = gl.getExtension('EXT_sRGB_write_control') as { FRAMEBUFFER_SRGB_EXT?: number } | null;
    const framebufferSrgb = (gl as unknown as { FRAMEBUFFER_SRGB?: unknown }).FRAMEBUFFER_SRGB;
    const framebufferSrgbCap =
      typeof framebufferSrgb === 'number'
        ? framebufferSrgb
        : typeof srgbWriteControl?.FRAMEBUFFER_SRGB_EXT === 'number'
          ? srgbWriteControl.FRAMEBUFFER_SRGB_EXT
          : null;
    if (typeof framebufferSrgbCap === 'number') {
      gl.disable(framebufferSrgbCap);
      const err = gl.getError();
      // Some environments do not expose sRGB framebuffer write control; avoid failing init
      // on a best-effort disable attempt.
      if (err !== gl.NO_ERROR && err !== gl.INVALID_ENUM) {
        throw new PresenterError('webgl_error', `disable FRAMEBUFFER_SRGB: WebGL error ${glEnumToString(gl, err)} (${err})`);
      }
    }

    assertWebGlOk(gl, 'recreateResources');

    this.uploadCursorTexture();
  }

  private resizeTexture(width: number, height: number): void {
    const gl = this.gl;
    if (!gl || !this.frameTexture) return;
    gl.bindTexture(gl.TEXTURE_2D, this.frameTexture);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    assertWebGlOk(gl, 'texImage2D');
  }

  private uploadCursorTexture(): void {
    const gl = this.gl;
    if (!gl || !this.cursorTexture) return;
    if (!this.cursorImage || this.cursorWidth <= 0 || this.cursorHeight <= 0) return;

    gl.bindTexture(gl.TEXTURE_2D, this.cursorTexture);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, this.cursorWidth, this.cursorHeight, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);

    // Cursor data is provided with a top-left origin. We keep `UNPACK_FLIP_Y_WEBGL = 0`
    // so the first scanline becomes the bottom row in GL texture coordinates, matching
    // the top-left math used by the cursor compositor in the fragment shader.
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, 0);

    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      this.cursorWidth,
      this.cursorHeight,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      this.cursorImage,
    );
    assertWebGlOk(gl, 'cursor texSubImage2D');
  }

  private draw(): void {
    const gl = this.gl;
    const canvas = this.canvas;
    if (
      !gl ||
      !canvas ||
      !this.program ||
      !this.vao ||
      !this.frameTexture ||
      !this.cursorTexture ||
      !this.uSrcSizeLoc ||
      !this.uCursorEnableLoc ||
      !this.uCursorPosLoc ||
      !this.uCursorHotLoc ||
      !this.uCursorSizeLoc ||
      this.isContextLost
    ) {
      return;
    }

    const canvasW = canvas.width;
    const canvasH = canvas.height;

    // Clear full canvas for non-stretch modes so letterboxing is deterministic.
    const scaleMode = this.opts.scaleMode ?? 'fit';
    if (scaleMode !== 'stretch') {
      const [r, g, b] = this.opts.clearColor ?? DEFAULT_CLEAR_COLOR;
      gl.viewport(0, 0, canvasW, canvasH);
      // The blit shader performs manual sRGB encoding (and fixed-function framebuffer sRGB is
      // disabled), so encode the letterbox clear color too so it appears consistent.
      const alpha = this.opts.canvasAlpha === true ? (this.opts.clearColor?.[3] ?? DEFAULT_CLEAR_COLOR[3]) : 1;
      gl.clearColor(srgbEncodeChannel(r), srgbEncodeChannel(g), srgbEncodeChannel(b), alpha);
      gl.clear(gl.COLOR_BUFFER_BIT);
    }

    const vp = computeViewport(canvasW, canvasH, this.srcWidth, this.srcHeight, scaleMode);
    gl.viewport(vp.x, vp.y, vp.w, vp.h);

    gl.useProgram(this.program);
    gl.uniform2i(this.uSrcSizeLoc, this.srcWidth | 0, this.srcHeight | 0);

    const cursorEnable =
      this.cursorRenderEnabled && this.cursorEnabled && this.cursorWidth > 0 && this.cursorHeight > 0 ? 1 : 0;
    gl.uniform1i(this.uCursorEnableLoc, cursorEnable);
    gl.uniform2i(this.uCursorPosLoc, this.cursorX | 0, this.cursorY | 0);
    gl.uniform2i(this.uCursorHotLoc, this.cursorHotX | 0, this.cursorHotY | 0);
    gl.uniform2i(this.uCursorSizeLoc, this.cursorWidth | 0, this.cursorHeight | 0);

    gl.bindVertexArray(this.vao);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.frameTexture);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.cursorTexture);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
    gl.activeTexture(gl.TEXTURE0);
  }

  private createProgram(gl: WebGL2RenderingContext, vertSrc: string, fragSrc: string): WebGLProgram {
    const compile = (type: number, src: string): WebGLShader => {
      const shader = gl.createShader(type);
      if (!shader) throw new PresenterError('webgl_resource_failed', 'Failed to allocate shader');
      gl.shaderSource(shader, src);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        const info = gl.getShaderInfoLog(shader) ?? '(no shader info log)';
        gl.deleteShader(shader);
        const stage = type === gl.VERTEX_SHADER ? 'vertex' : 'fragment';
        throw new PresenterError('shader_compile_failed', `Failed to compile ${stage} shader: ${info}`);
      }
      return shader;
    };

    const vs = compile(gl.VERTEX_SHADER, vertSrc);
    const fs = compile(gl.FRAGMENT_SHADER, fragSrc);

    const program = gl.createProgram();
    if (!program) throw new PresenterError('webgl_resource_failed', 'Failed to allocate program');
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);

    gl.deleteShader(vs);
    gl.deleteShader(fs);

    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      const info = gl.getProgramInfoLog(program) ?? '(no program info log)';
      gl.deleteProgram(program);
      throw new PresenterError('program_link_failed', `Failed to link shader program: ${info}`);
    }

    return program;
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
