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
uniform uint u_flags;
out vec4 outColor;

const uint FLAG_APPLY_SRGB_ENCODE = 1u;
const uint FLAG_PREMULTIPLY_ALPHA = 2u;
const uint FLAG_FORCE_OPAQUE_ALPHA = 4u;
const uint FLAG_FLIP_Y = 8u;

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
  /** @type {WebGLVertexArrayObject} */
  vao;
  /** @type {WebGLUniformLocation} */
  uFlagsLoc;
  /** @type {RawWebGL2PresenterOptions} */
  opts;

  /** @type {number} */
  srcWidth = 0;
  /** @type {number} */
  srcHeight = 0;

  /**
   * @param {HTMLCanvasElement} canvas
   * @param {RawWebGL2PresenterOptions=} opts
   */
  constructor(canvas: HTMLCanvasElement, opts: any = {}) {
    this.opts = {
      framebufferColorSpace: opts.framebufferColorSpace ?? "linear",
      outputColorSpace: opts.outputColorSpace ?? "srgb",
      alphaMode: opts.alphaMode ?? "opaque",
      flipY: opts.flipY ?? false,
    };

    const ctxAttrs: WebGLContextAttributes =
      this.opts.alphaMode === "opaque"
        ? { alpha: false, premultipliedAlpha: false }
        : { alpha: true, premultipliedAlpha: true };

    const gl = canvas.getContext("webgl2", ctxAttrs);
    if (!gl) throw new Error("WebGL2 not supported");
    this.gl = gl;

    // Ensure the unpack convention is explicit; we want top-to-bottom CPU buffers to be
    // interpreted as-is and compensate via UV convention.
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, 0);

    this.program = createProgram(gl, VERT_SRC, FRAG_SRC);

    const tex = gl.createTexture();
    if (!tex) throw new Error("createTexture failed");
    this.srcTex = tex;

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
    const uFlagsLoc = gl.getUniformLocation(this.program, "u_flags");
    if (!uFlagsLoc) throw new Error("u_flags uniform missing");
    this.uFlagsLoc = uFlagsLoc;
    gl.useProgram(null);

    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
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
    const gl = this.gl;

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
  }

  present() {
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

    gl.useProgram(this.program);
    gl.uniform1ui(this.uFlagsLoc, flags);

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.srcTex);

    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    gl.bindVertexArray(null);

    gl.bindTexture(gl.TEXTURE_2D, null);
    gl.useProgram(null);
  }
}

