import type {
  BackendCapabilities,
  BackendInitOptions,
  CapturedFrame,
  DirtyRect,
  FilterMode,
  PresentationBackend,
} from './backend';

type MaybeCanvas = HTMLCanvasElement | OffscreenCanvas;

function toU8View(buffer: ArrayBufferView): Uint8Array {
  return buffer instanceof Uint8Array
    ? buffer
    : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
}

function parseCombinedGlsl(source: string): { vertex: string; fragment: string } {
  const vertexTag = '#type vertex';
  const fragmentTag = '#type fragment';

  const vPos = source.indexOf(vertexTag);
  const fPos = source.indexOf(fragmentTag);
  if (vPos === -1 || fPos === -1) {
    throw new Error('Shader source missing `#type vertex` / `#type fragment` sections');
  }

  const vertexSource = source
    .slice(vPos + vertexTag.length, fPos)
    .trimStart()
    .trimEnd();
  const fragmentSource = source.slice(fPos + fragmentTag.length).trimStart().trimEnd();

  return { vertex: vertexSource, fragment: fragmentSource };
}

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error('Failed to create shader');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);

  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(shader) ?? '(no log)';
    gl.deleteShader(shader);
    throw new Error(`Shader compilation failed: ${log}`);
  }

  return shader;
}

function linkProgram(
  gl: WebGL2RenderingContext,
  vertexShader: WebGLShader,
  fragmentShader: WebGLShader,
): WebGLProgram {
  const program = gl.createProgram();
  if (!program) throw new Error('Failed to create program');

  gl.attachShader(program, vertexShader);
  gl.attachShader(program, fragmentShader);
  gl.linkProgram(program);

  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(program) ?? '(no log)';
    gl.deleteProgram(program);
    throw new Error(`Program link failed: ${log}`);
  }

  return program;
}

function computeLetterboxViewport(
  canvasWidth: number,
  canvasHeight: number,
  contentWidth: number,
  contentHeight: number,
): { x: number; y: number; width: number; height: number } {
  if (canvasWidth <= 0 || canvasHeight <= 0 || contentWidth <= 0 || contentHeight <= 0) {
    return { x: 0, y: 0, width: canvasWidth, height: canvasHeight };
  }

  const canvasAspect = canvasWidth / canvasHeight;
  const contentAspect = contentWidth / contentHeight;

  if (canvasAspect > contentAspect) {
    const height = canvasHeight;
    const width = Math.round(height * contentAspect);
    const x = Math.floor((canvasWidth - width) / 2);
    const y = 0;
    return { x, y, width, height };
  }

  const width = canvasWidth;
  const height = Math.round(width / contentAspect);
  const x = 0;
  const y = Math.floor((canvasHeight - height) / 2);
  return { x, y, width, height };
}

export class WebGL2Backend implements PresentationBackend {
  private canvas: MaybeCanvas | null = null;
  private gl: WebGL2RenderingContext | null = null;

  private program: WebGLProgram | null = null;
  private vao: WebGLVertexArrayObject | null = null;
  private texture: WebGLTexture | null = null;

  private uTextureLoc: WebGLUniformLocation | null = null;

  private frameWidth = 0;
  private frameHeight = 0;

  private filterMode: FilterMode = 'nearest';
  private preserveAspectRatio = true;

  async init(canvas: MaybeCanvas, options?: BackendInitOptions): Promise<void> {
    this.filterMode = options?.filter ?? 'nearest';
    this.preserveAspectRatio = options?.preserveAspectRatio ?? true;

    const gl = canvas.getContext('webgl2', {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer: true,
    });
    if (!gl) throw new Error('WebGL2 unavailable');

    this.canvas = canvas;
    this.gl = gl;

    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

    const shaderUrl = new URL('./shaders/blit_glsl_es300.glsl', import.meta.url);
    const shaderText = await fetch(shaderUrl).then(async (res) => {
      if (!res.ok) throw new Error(`Failed to load ${shaderUrl.pathname}: ${res.status}`);
      return await res.text();
    });
    const { vertex, fragment } = parseCombinedGlsl(shaderText);

    const vs = compileShader(gl, gl.VERTEX_SHADER, vertex);
    const fs = compileShader(gl, gl.FRAGMENT_SHADER, fragment);
    const program = linkProgram(gl, vs, fs);
    gl.deleteShader(vs);
    gl.deleteShader(fs);

    this.program = program;
    this.uTextureLoc = gl.getUniformLocation(program, 'u_texture');

    const vao = gl.createVertexArray();
    if (!vao) throw new Error('Failed to create VAO');
    gl.bindVertexArray(vao);

    const vbo = gl.createBuffer();
    if (!vbo) throw new Error('Failed to create VBO');
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);

    const verts = new Float32Array([
      -1, -1, 0, 1,
      1, -1, 1, 1,
      -1, 1, 0, 0,
      1, 1, 1, 0,
    ]);
    gl.bufferData(gl.ARRAY_BUFFER, verts, gl.STATIC_DRAW);

    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 2, gl.FLOAT, false, 16, 8);

    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    this.vao = vao;

    const texture = gl.createTexture();
    if (!texture) throw new Error('Failed to create texture');
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    const filter = this.filterMode === 'linear' ? gl.LINEAR : gl.NEAREST;
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.bindTexture(gl.TEXTURE_2D, null);

    this.texture = texture;
  }

  uploadFrameRGBA(
    buffer: ArrayBufferView,
    width: number,
    height: number,
    dirtyRects?: readonly DirtyRect[],
  ): void {
    const gl = this.gl;
    const texture = this.texture;
    if (!gl || !texture) throw new Error('Backend not initialized');

    const data = toU8View(buffer);

    gl.bindTexture(gl.TEXTURE_2D, texture);

    if (width !== this.frameWidth || height !== this.frameHeight) {
      this.frameWidth = width;
      this.frameHeight = height;
      gl.texImage2D(
        gl.TEXTURE_2D,
        0,
        gl.RGBA8,
        width,
        height,
        0,
        gl.RGBA,
        gl.UNSIGNED_BYTE,
        data,
      );
      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    if (!dirtyRects || dirtyRects.length === 0) {
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, data);
      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, width);

    for (const rect of dirtyRects) {
      gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, rect.x);
      gl.pixelStorei(gl.UNPACK_SKIP_ROWS, rect.y);
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        gl.RGBA,
        gl.UNSIGNED_BYTE,
        data,
      );
    }

    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 0);
    gl.pixelStorei(gl.UNPACK_SKIP_ROWS, 0);
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  present(): void {
    const gl = this.gl;
    const canvas = this.canvas;
    const program = this.program;
    const vao = this.vao;
    const texture = this.texture;

    if (!gl || !canvas || !program || !vao || !texture) throw new Error('Backend not initialized');

    if (canvas instanceof HTMLCanvasElement) {
      const dpr = window.devicePixelRatio || 1;
      const displayWidth = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const displayHeight = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== displayWidth || canvas.height !== displayHeight) {
        canvas.width = displayWidth;
        canvas.height = displayHeight;
      }
    }

    const canvasWidth = gl.drawingBufferWidth;
    const canvasHeight = gl.drawingBufferHeight;

    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);

    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    const viewport = this.preserveAspectRatio
      ? computeLetterboxViewport(canvasWidth, canvasHeight, this.frameWidth, this.frameHeight)
      : { x: 0, y: 0, width: canvasWidth, height: canvasHeight };
    // WebGL viewport origin is bottom-left.
    gl.viewport(viewport.x, canvasHeight - viewport.y - viewport.height, viewport.width, viewport.height);

    gl.useProgram(program);
    gl.bindVertexArray(vao);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, texture);
    if (this.uTextureLoc) gl.uniform1i(this.uTextureLoc, 0);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    gl.bindTexture(gl.TEXTURE_2D, null);
    gl.bindVertexArray(null);
    gl.useProgram(null);
  }

  async captureFrame(): Promise<CapturedFrame> {
    const gl = this.gl;
    if (!gl) throw new Error('Backend not initialized');

    const width = gl.drawingBufferWidth;
    const height = gl.drawingBufferHeight;

    const pixels = new Uint8Array(width * height * 4);
    gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

    const flipped = new Uint8ClampedArray(pixels.length);
    const rowSize = width * 4;
    for (let y = 0; y < height; y++) {
      const srcStart = (height - 1 - y) * rowSize;
      const dstStart = y * rowSize;
      flipped.set(pixels.subarray(srcStart, srcStart + rowSize), dstStart);
    }

    return { width, height, data: flipped };
  }

  getCapabilities(): BackendCapabilities {
    return {
      kind: 'webgl2',
      supportsDirtyRects: true,
      supportsCapture: true,
    };
  }
}
