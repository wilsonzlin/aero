// Raw WebGL2 presenter (no framework, no abstractions) intended for:
// - deterministic validation of color space + alpha mode handling
// - a fallback presenter when WebGPU is unavailable
//
// Presentation policy is defined in `crates/aero-gpu/src/present.rs` and mirrored here.
//
// Convention notes:
// - UV origin is TOP-LEFT (0,0) to match D3D/Windows.
// - We intentionally keep `UNPACK_FLIP_Y_WEBGL = 0` so that CPU-side buffers written
//   top-to-bottom line up with the UV convention above (WebGL's pixel unpack origin is
//   bottom-left, so the mismatch cancels out).

const VERT_SRC = `#version 300 es
precision highp float;

layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;

out vec2 v_uv;

void main() {
  gl_Position = vec4(a_pos, 0.0, 1.0);
  v_uv = a_uv;
}
`;

const FRAG_SRC = `#version 300 es
precision highp float;
precision highp int;

in vec2 v_uv;
uniform sampler2D u_tex;
uniform sampler2D u_cursor_tex;
uniform uint u_flags;
uniform ivec2 u_src_size;
uniform ivec2 u_cursor_pos;
uniform ivec2 u_cursor_hot;
uniform ivec2 u_cursor_size;
out vec4 outColor;

const uint FLAG_APPLY_SRGB_ENCODE = 1u;
const uint FLAG_PREMULTIPLY_ALPHA = 2u;
const uint FLAG_FORCE_OPAQUE_ALPHA = 4u;
const uint FLAG_FLIP_Y = 8u;
const uint FLAG_CURSOR_ENABLE = 16u;

float srgbEncodeChannel(float x) {
  float v = clamp(x, 0.0, 1.0);
  if (v <= 0.0031308) return v * 12.92;
  return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

vec3 srgbEncode(vec3 rgb) {
  return vec3(
    srgbEncodeChannel(rgb.r),
    srgbEncodeChannel(rgb.g),
    srgbEncodeChannel(rgb.b)
  );
}

void main() {
  vec2 uv = v_uv;
  if ((u_flags & FLAG_FLIP_Y) != 0u) {
    uv.y = 1.0 - uv.y;
  }

  vec4 color = texture(u_tex, uv);

  if ((u_flags & FLAG_CURSOR_ENABLE) != 0u && u_cursor_size.x > 0 && u_cursor_size.y > 0) {
    ivec2 srcSize = max(u_src_size, ivec2(1, 1));
    ivec2 screenPx = ivec2(v_uv * vec2(srcSize));
    screenPx = clamp(screenPx, ivec2(0, 0), srcSize - ivec2(1, 1));

    ivec2 origin = u_cursor_pos - u_cursor_hot;
    ivec2 cursorPx = screenPx - origin;
    if (cursorPx.x >= 0 && cursorPx.y >= 0 && cursorPx.x < u_cursor_size.x && cursorPx.y < u_cursor_size.y) {
      vec2 cuv = (vec2(cursorPx) + vec2(0.5)) / vec2(u_cursor_size);
      vec4 cursorColor = texture(u_cursor_tex, cuv);
      float a = cursorColor.a;
      color.rgb = cursorColor.rgb * a + color.rgb * (1.0 - a);
      color.a = a + color.a * (1.0 - a);
    }
  }

  if ((u_flags & FLAG_PREMULTIPLY_ALPHA) != 0u) {
    color.rgb *= color.a;
  }
  if ((u_flags & FLAG_FORCE_OPAQUE_ALPHA) != 0u) {
    color.a = 1.0;
  }
  if ((u_flags & FLAG_APPLY_SRGB_ENCODE) != 0u) {
    color.rgb = srgbEncode(color.rgb);
  }

  outColor = color;
}
`;

function compileShader(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = gl.createShader(type);
  if (!shader) throw new Error("createShader failed");
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(shader) ?? "(no shader log)";
    gl.deleteShader(shader);
    throw new Error(`shader compile failed: ${log}`);
  }
  return shader;
}

function createProgram(gl: WebGL2RenderingContext, vertSrc: string, fragSrc: string): WebGLProgram {
  const vs = compileShader(gl, gl.VERTEX_SHADER, vertSrc);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, fragSrc);

  const program = gl.createProgram();
  if (!program) throw new Error("createProgram failed");

  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);

  gl.deleteShader(vs);
  gl.deleteShader(fs);

  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(program) ?? "(no program log)";
    gl.deleteProgram(program);
    throw new Error(`program link failed: ${log}`);
  }

  return program;
}

/**
 * @typedef {"linear" | "srgb"} ColorSpace
 * @typedef {"opaque" | "premultiplied"} AlphaMode
 */

/**
 * @typedef {object} RawWebGL2PresenterOptions
 * @property {ColorSpace=} framebufferColorSpace
 * @property {ColorSpace=} outputColorSpace
 * @property {AlphaMode=} alphaMode
 * @property {boolean=} flipY
 */

export class RawWebGL2Presenter {
  /** @type {WebGL2RenderingContext} */
  gl;
  /** @type {WebGLProgram} */
  program;
  /** @type {WebGLTexture} */
  srcTex;
  /** @type {WebGLTexture} */
  cursorTex;
  /** @type {WebGLVertexArrayObject} */
  vao;
  /** @type {WebGLUniformLocation} */
  uFlagsLoc;
  /** @type {WebGLUniformLocation} */
  uSrcSizeLoc;
  /** @type {WebGLUniformLocation} */
  uCursorPosLoc;
  /** @type {WebGLUniformLocation} */
  uCursorHotLoc;
  /** @type {WebGLUniformLocation} */
  uCursorSizeLoc;
  /** @type {RawWebGL2PresenterOptions} */
  opts;

  /** @type {number} */
  srcWidth = 0;
  /** @type {number} */
  srcHeight = 0;

  /** @type {boolean} */
  cursorEnabled = false;
  /** @type {number} */
  cursorX = 0;
  /** @type {number} */
  cursorY = 0;
  /** @type {number} */
  cursorHotX = 0;
  /** @type {number} */
  cursorHotY = 0;
  /** @type {number} */
  cursorWidth = 0;
  /** @type {number} */
  cursorHeight = 0;

  /**
   * @param {HTMLCanvasElement | OffscreenCanvas} canvas
   * @param {RawWebGL2PresenterOptions=} opts
   */
  constructor(canvas: HTMLCanvasElement | OffscreenCanvas, opts: any = {}) {
    this.opts = {
      framebufferColorSpace: opts.framebufferColorSpace ?? "linear",
      outputColorSpace: opts.outputColorSpace ?? "srgb",
      alphaMode: opts.alphaMode ?? "opaque",
      flipY: opts.flipY ?? false,
    };

    const commonCtxAttrs: WebGLContextAttributes = {
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
    };
    const ctxAttrs: WebGLContextAttributes =
      this.opts.alphaMode === "opaque"
        ? { ...commonCtxAttrs, alpha: false, premultipliedAlpha: false }
        : { ...commonCtxAttrs, alpha: true, premultipliedAlpha: true };

    const gl = canvas.getContext("webgl2", ctxAttrs);
    if (!gl) throw new Error("WebGL2 not supported");
    this.gl = gl;

    // Ensure the unpack convention is explicit; we want top-to-bottom CPU buffers to be
    // interpreted as-is and compensate via UV convention.
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, 0);

    // Deterministic presentation: disable sources of browser/driver variability.
    gl.disable(gl.DITHER);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.disable(gl.SAMPLE_ALPHA_TO_COVERAGE);
    gl.disable(gl.SAMPLE_COVERAGE);
    gl.colorMask(true, true, true, true);

    // Deterministic presentation: we do manual sRGB encoding in the blit shader, so ensure
    // fixed-function framebuffer sRGB conversion (when present) is disabled to avoid double-gamma.
    //
    // Note: WebGL2 sRGB write control is optional and exposed via:
    // - `EXT_sRGB_write_control` (FRAMEBUFFER_SRGB_EXT)
    // - some environments expose `gl.FRAMEBUFFER_SRGB` directly.
    const srgbWriteControl = gl.getExtension("EXT_sRGB_write_control") as { FRAMEBUFFER_SRGB_EXT?: number } | null;
    const framebufferSrgb = (gl as unknown as { FRAMEBUFFER_SRGB?: unknown }).FRAMEBUFFER_SRGB;
    const framebufferSrgbCap =
      typeof framebufferSrgb === "number"
        ? framebufferSrgb
        : typeof srgbWriteControl?.FRAMEBUFFER_SRGB_EXT === "number"
          ? srgbWriteControl.FRAMEBUFFER_SRGB_EXT
          : null;
    if (typeof framebufferSrgbCap === "number") {
      gl.disable(framebufferSrgbCap);
      const err = gl.getError();
      // Some environments do not expose sRGB framebuffer write control; avoid failing init
      // on a best-effort disable attempt.
      if (err !== gl.NO_ERROR && err !== gl.INVALID_ENUM) {
        throw new Error(`disable FRAMEBUFFER_SRGB: WebGL error ${err}`);
      }
    }

    this.program = createProgram(gl, VERT_SRC, FRAG_SRC);

    const tex = gl.createTexture();
    if (!tex) throw new Error("createTexture failed");
    this.srcTex = tex;

    const cursorTex = gl.createTexture();
    if (!cursorTex) throw new Error("createTexture failed");
    this.cursorTex = cursorTex;

    const vao = gl.createVertexArray();
    if (!vao) throw new Error("createVertexArray failed");
    this.vao = vao;

    gl.bindVertexArray(vao);

    const vbo = gl.createBuffer();
    if (!vbo) throw new Error("createBuffer failed");
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);

    // Triangle strip quad, UV origin TOP-LEFT (0,0).
    const verts = new Float32Array([
      -1, 1, 0, 0, // top-left
      -1, -1, 0, 1, // bottom-left
      1, 1, 1, 0, // top-right
      1, -1, 1, 1, // bottom-right
    ]);
    gl.bufferData(gl.ARRAY_BUFFER, verts, gl.STATIC_DRAW);

    const stride = 4 * 4; // 4 floats
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, stride, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 2, gl.FLOAT, false, stride, 2 * 4);

    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    gl.useProgram(this.program);
    const uTexLoc = gl.getUniformLocation(this.program, "u_tex");
    if (!uTexLoc) throw new Error("u_tex uniform missing");
    gl.uniform1i(uTexLoc, 0);
    const uCursorTexLoc = gl.getUniformLocation(this.program, "u_cursor_tex");
    if (!uCursorTexLoc) throw new Error("u_cursor_tex uniform missing");
    gl.uniform1i(uCursorTexLoc, 1);
    const uFlagsLoc = gl.getUniformLocation(this.program, "u_flags");
    if (!uFlagsLoc) throw new Error("u_flags uniform missing");
    this.uFlagsLoc = uFlagsLoc;
    const uSrcSizeLoc = gl.getUniformLocation(this.program, "u_src_size");
    if (!uSrcSizeLoc) throw new Error("u_src_size uniform missing");
    this.uSrcSizeLoc = uSrcSizeLoc;
    const uCursorPosLoc = gl.getUniformLocation(this.program, "u_cursor_pos");
    if (!uCursorPosLoc) throw new Error("u_cursor_pos uniform missing");
    this.uCursorPosLoc = uCursorPosLoc;
    const uCursorHotLoc = gl.getUniformLocation(this.program, "u_cursor_hot");
    if (!uCursorHotLoc) throw new Error("u_cursor_hot uniform missing");
    this.uCursorHotLoc = uCursorHotLoc;
    const uCursorSizeLoc = gl.getUniformLocation(this.program, "u_cursor_size");
    if (!uCursorSizeLoc) throw new Error("u_cursor_size uniform missing");
    this.uCursorSizeLoc = uCursorSizeLoc;
    gl.useProgram(null);

    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.bindTexture(gl.TEXTURE_2D, null);

    gl.bindTexture(gl.TEXTURE_2D, cursorTex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    const cursorInternalFormat =
      this.opts.framebufferColorSpace === "srgb" ? gl.SRGB8_ALPHA8 : gl.RGBA8;
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      cursorInternalFormat,
      1,
      1,
      0,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      new Uint8Array([0, 0, 0, 0]),
    );
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  /**
   * Upload a full RGBA8 framebuffer.
   *
   * @param {Uint8Array} rgba
   * @param {number} width
   * @param {number} height
   */
  setSourceRgba8(rgba: Uint8Array, width: number, height: number) {
    this.setSourceRgba8Strided(rgba, width, height, width * 4);
  }

  /**
   * Upload a full RGBA8 framebuffer with a caller-provided row stride.
   *
   * When `strideBytes !== width * 4`, WebGL2's `UNPACK_ROW_LENGTH` is used so we can
   * upload directly from a strided buffer without repacking on the CPU.
   */
  setSourceRgba8Strided(rgba: Uint8Array, width: number, height: number, strideBytes: number) {
    const gl = this.gl;
    if (strideBytes % 4 !== 0) {
      throw new Error(`strideBytes must be a multiple of 4 for RGBA8 uploads (got ${strideBytes})`);
    }
    const rowLengthPixels = strideBytes / 4;

    if (width !== this.srcWidth || height !== this.srcHeight) {
      this.srcWidth = width;
      this.srcHeight = height;

      gl.bindTexture(gl.TEXTURE_2D, this.srcTex);

      const internalFormat =
        this.opts.framebufferColorSpace === "srgb" ? gl.SRGB8_ALPHA8 : gl.RGBA8;

      gl.texImage2D(
        gl.TEXTURE_2D,
        0,
        internalFormat,
        width,
        height,
        0,
        gl.RGBA,
        gl.UNSIGNED_BYTE,
        null,
      );
      gl.bindTexture(gl.TEXTURE_2D, null);
    }

    // Keep unpack alignment explicit so odd widths/strides do not break uploads.
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    if (rowLengthPixels !== width) {
      gl.pixelStorei(gl.UNPACK_ROW_LENGTH, rowLengthPixels);
    }

    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);
    gl.texSubImage2D(
      gl.TEXTURE_2D,
      0,
      0,
      0,
      width,
      height,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      rgba,
    );
    gl.bindTexture(gl.TEXTURE_2D, null);

    if (rowLengthPixels !== width) {
      gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
    }
  }

  /**
   * Upload only dirty rects from a strided RGBA8 source buffer.
   *
   * This avoids allocating/copying per-rect by using WebGL2 unpack parameters
   * (`UNPACK_ROW_LENGTH`, `UNPACK_SKIP_PIXELS`, `UNPACK_SKIP_ROWS`).
   */
  setSourceRgba8StridedDirtyRects(
    rgba: Uint8Array,
    width: number,
    height: number,
    strideBytes: number,
    dirtyRects: Array<{ x: number; y: number; w: number; h: number }>,
  ) {
    const gl = this.gl;
    if (dirtyRects.length === 0) return;

    if (strideBytes % 4 !== 0) {
      throw new Error(`strideBytes must be a multiple of 4 for RGBA8 uploads (got ${strideBytes})`);
    }
    const rowLengthPixels = strideBytes / 4;

    // Ensure the destination texture is allocated to the correct size.
    if (width !== this.srcWidth || height !== this.srcHeight) {
      this.setSourceRgba8Strided(rgba, width, height, strideBytes);
      return;
    }

    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, rowLengthPixels);

    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);

    for (const rect of dirtyRects) {
      const x = Math.max(0, rect.x | 0);
      const y = Math.max(0, rect.y | 0);
      const w = Math.max(0, rect.w | 0);
      const h = Math.max(0, rect.h | 0);
      if (w === 0 || h === 0) continue;

      gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, x);
      gl.pixelStorei(gl.UNPACK_SKIP_ROWS, y);

      gl.texSubImage2D(gl.TEXTURE_2D, 0, x, y, w, h, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
    }

    gl.bindTexture(gl.TEXTURE_2D, null);

    // Reset unpack state so callers don't get surprising behavior.
    gl.pixelStorei(gl.UNPACK_SKIP_PIXELS, 0);
    gl.pixelStorei(gl.UNPACK_SKIP_ROWS, 0);
    gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
  }

  setCursorImageRgba8(rgba: Uint8Array, width: number, height: number) {
    const gl = this.gl;
    const w = Math.max(0, width | 0);
    const h = Math.max(0, height | 0);
    if (w === 0 || h === 0) {
      throw new Error("cursor width/height must be non-zero");
    }

    if (w !== this.cursorWidth || h !== this.cursorHeight) {
      this.cursorWidth = w;
      this.cursorHeight = h;

      gl.bindTexture(gl.TEXTURE_2D, this.cursorTex);
      const internalFormat =
        this.opts.framebufferColorSpace === "srgb" ? gl.SRGB8_ALPHA8 : gl.RGBA8;
      gl.texImage2D(gl.TEXTURE_2D, 0, internalFormat, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      gl.bindTexture(gl.TEXTURE_2D, null);
    }

    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.bindTexture(gl.TEXTURE_2D, this.cursorTex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  setCursorState(enabled: boolean, x: number, y: number, hotX: number, hotY: number) {
    this.cursorEnabled = !!enabled;
    this.cursorX = x | 0;
    this.cursorY = y | 0;
    this.cursorHotX = Math.max(0, hotX | 0);
    this.cursorHotY = Math.max(0, hotY | 0);
  }

  present(opts: { includeCursor?: boolean } = {}) {
    const gl = this.gl;

    gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight);
    gl.disable(gl.BLEND);

    let flags = 0;
    // WebGL2 default framebuffer behavior (sRGB conversion) is inconsistent across browsers and
    // depends on context attrs / extensions. For deterministic output we do sRGB encoding in
    // the shader when requested.
    if (this.opts.outputColorSpace === "srgb") flags |= 1;
    if (this.opts.alphaMode === "premultiplied") flags |= 2;
    if (this.opts.alphaMode === "opaque") flags |= 4;
    if (this.opts.flipY) flags |= 8;
    const includeCursor = opts.includeCursor !== false;
    if (includeCursor && this.cursorEnabled && this.cursorWidth > 0 && this.cursorHeight > 0) flags |= 16;

    gl.useProgram(this.program);
    gl.uniform1ui(this.uFlagsLoc, flags);
    gl.uniform2i(this.uSrcSizeLoc, Math.max(1, this.srcWidth | 0), Math.max(1, this.srcHeight | 0));
    gl.uniform2i(this.uCursorPosLoc, this.cursorX | 0, this.cursorY | 0);
    gl.uniform2i(this.uCursorHotLoc, this.cursorHotX | 0, this.cursorHotY | 0);
    gl.uniform2i(this.uCursorSizeLoc, this.cursorWidth | 0, this.cursorHeight | 0);

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.cursorTex);

    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    gl.bindVertexArray(null);

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, null);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, null);
    gl.activeTexture(gl.TEXTURE0);
    gl.useProgram(null);
  }
}
