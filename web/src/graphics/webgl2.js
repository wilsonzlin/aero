import { bgra8ToRgba8, indexed8ToRgba8, rgb565ToRgba8 } from './convert.js';

function assertNonNull(value, msg) {
  if (value == null) throw new Error(msg);
  return value;
}

/**
 * @param {ArrayBufferView} view
 * @returns {Uint8Array}
 */
function asU8(view) {
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

/**
 * @param {ArrayBufferView} view
 * @returns {Uint16Array}
 */
function asU16(view) {
  if (view instanceof Uint16Array) return view;
  if (view.byteOffset % 2 !== 0 || view.byteLength % 2 !== 0) {
    throw new Error('rgb565 buffers must be 2-byte aligned');
  }
  return new Uint16Array(view.buffer, view.byteOffset, view.byteLength / 2);
}

/**
 * @param {WebGL2RenderingContext} gl
 * @param {number} type
 * @param {string} source
 * @returns {WebGLShader}
 */
function compileShader(gl, type, source) {
  const shader = assertNonNull(gl.createShader(type), 'createShader failed');
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    const info = gl.getShaderInfoLog(shader);
    gl.deleteShader(shader);
    throw new Error(`Shader compile failed: ${info ?? 'unknown error'}`);
  }
  return shader;
}

/**
 * @param {WebGL2RenderingContext} gl
 * @param {string} vsSource
 * @param {string} fsSource
 * @returns {WebGLProgram}
 */
function createProgram(gl, vsSource, fsSource) {
  const vs = compileShader(gl, gl.VERTEX_SHADER, vsSource);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, fsSource);
  const program = assertNonNull(gl.createProgram(), 'createProgram failed');
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(program);
    gl.deleteProgram(program);
    throw new Error(`Program link failed: ${info ?? 'unknown error'}`);
  }
  return program;
}

/**
 * WebGL2 fallback backend for environments without WebGPU.
 */
export class WebGl2Backend {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {WebGL2RenderingContext} gl
   */
  constructor(canvas, gl) {
    this.kind = 'webgl2';
    this.canvas = canvas;
    this.gl = gl;

    this._bgraExt = gl.getExtension('EXT_texture_format_BGRA8888');

    // A dynamic quad buffer (6 vertices, vec2 pos + vec2 uv).
    this._quadVbo = assertNonNull(gl.createBuffer(), 'createBuffer failed');
    this._quadVao = assertNonNull(gl.createVertexArray(), 'createVertexArray failed');

    gl.bindVertexArray(this._quadVao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this._quadVbo);
    gl.bufferData(gl.ARRAY_BUFFER, 6 * 4 * 4, gl.DYNAMIC_DRAW);

    const vs = `#version 300 es
      layout(location = 0) in vec2 a_pos;
      layout(location = 1) in vec2 a_uv;
      out vec2 v_uv;
      void main() {
        v_uv = a_uv;
        gl_Position = vec4(a_pos, 0.0, 1.0);
      }`;

    const fsRgba = `#version 300 es
      precision mediump float;
      in vec2 v_uv;
      uniform sampler2D u_tex;
      out vec4 outColor;
      void main() {
        outColor = texture(u_tex, v_uv);
      }`;

    const fsIndexed = `#version 300 es
      precision mediump float;
      in vec2 v_uv;
      uniform highp usampler2D u_indexTex;
      uniform sampler2D u_paletteTex;
      out vec4 outColor;
      void main() {
        uint idx = texture(u_indexTex, v_uv).r;
        outColor = texelFetch(u_paletteTex, ivec2(int(idx), 0), 0);
      }`;

    this._programRgba = createProgram(gl, vs, fsRgba);
    this._programIndexed = createProgram(gl, vs, fsIndexed);

    this._locRgba = {
      aPos: gl.getAttribLocation(this._programRgba, 'a_pos'),
      aUv: gl.getAttribLocation(this._programRgba, 'a_uv'),
      uTex: assertNonNull(gl.getUniformLocation(this._programRgba, 'u_tex'), 'u_tex loc'),
    };

    this._locIndexed = {
      aPos: gl.getAttribLocation(this._programIndexed, 'a_pos'),
      aUv: gl.getAttribLocation(this._programIndexed, 'a_uv'),
      uIndexTex: assertNonNull(
        gl.getUniformLocation(this._programIndexed, 'u_indexTex'),
        'u_indexTex loc',
      ),
      uPaletteTex: assertNonNull(
        gl.getUniformLocation(this._programIndexed, 'u_paletteTex'),
        'u_paletteTex loc',
      ),
    };

    // RGBA pipeline VAO layout (shared across programs).
    gl.enableVertexAttribArray(this._locRgba.aPos);
    gl.vertexAttribPointer(this._locRgba.aPos, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(this._locRgba.aUv);
    gl.vertexAttribPointer(this._locRgba.aUv, 2, gl.FLOAT, false, 16, 8);

    // Indexed pipeline uses the same attribute names/locations.
    gl.enableVertexAttribArray(this._locIndexed.aPos);
    gl.vertexAttribPointer(this._locIndexed.aPos, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(this._locIndexed.aUv);
    gl.vertexAttribPointer(this._locIndexed.aUv, 2, gl.FLOAT, false, 16, 8);

    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    this._frameTex = null;
    this._indexTex = null;
    this._paletteTex = null;
    this._frameInfo = null;

    this._blitTex = null;
    this._blitInfo = null;

    this._triangleProgram = this._createTriangleProgram();
  }

  /**
   * @param {HTMLCanvasElement} canvas
   * @returns {Promise<WebGl2Backend>}
   */
  static async create(canvas) {
    const gl = canvas.getContext('webgl2', {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: false,
      // Keep the default framebuffer contents intact after presenting so
      // automation (Playwright) can sample pixels via `drawImage()` on a 2D
      // canvas. This is a small demo backend, so the perf cost is acceptable.
      preserveDrawingBuffer: true,
    });
    if (!gl) throw new Error('WebGL2 not available');
    // Reduce driver variance and avoid unexpected dithering when presenting 8-bit content.
    gl.disable(gl.DITHER);
    return new WebGl2Backend(canvas, gl);
  }

  _createTriangleProgram() {
    const gl = this.gl;

    const vs = `#version 300 es
      layout(location = 0) in vec2 a_pos;
      layout(location = 1) in vec3 a_color;
      out vec3 v_color;
      void main() {
        v_color = a_color;
        gl_Position = vec4(a_pos, 0.0, 1.0);
      }`;

    const fs = `#version 300 es
      precision mediump float;
      in vec3 v_color;
      out vec4 outColor;
      void main() {
        outColor = vec4(v_color, 1.0);
      }`;

    const program = createProgram(gl, vs, fs);
    const vbo = assertNonNull(gl.createBuffer(), 'triangle vbo');
    const vao = assertNonNull(gl.createVertexArray(), 'triangle vao');

    gl.bindVertexArray(vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
    // 3 vertices: vec2 pos + vec3 color
    const verts = new Float32Array([
      0.0,
      0.8,
      1.0,
      0.2,
      0.2,
      -0.8,
      -0.8,
      0.2,
      1.0,
      0.2,
      0.8,
      -0.8,
      0.2,
      0.2,
      1.0,
    ]);
    gl.bufferData(gl.ARRAY_BUFFER, verts, gl.STATIC_DRAW);

    const aPos = gl.getAttribLocation(program, 'a_pos');
    const aColor = gl.getAttribLocation(program, 'a_color');
    gl.enableVertexAttribArray(aPos);
    gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 20, 0);
    gl.enableVertexAttribArray(aColor);
    gl.vertexAttribPointer(aColor, 3, gl.FLOAT, false, 20, 8);

    gl.bindVertexArray(null);
    gl.bindBuffer(gl.ARRAY_BUFFER, null);

    return { program, vao };
  }

  /**
   * @param {number} x
   * @param {number} y
   * @param {number} w
   * @param {number} h
   * @returns {Float32Array}
   */
  _quadVertsForRect(x, y, w, h) {
    const cw = this.canvas.width;
    const ch = this.canvas.height;

    const l = (x / cw) * 2 - 1;
    const r = ((x + w) / cw) * 2 - 1;
    const t = 1 - (y / ch) * 2;
    const b = 1 - ((y + h) / ch) * 2;

    // Two triangles.
    return new Float32Array([
      l,
      b,
      0,
      1,
      r,
      b,
      1,
      1,
      l,
      t,
      0,
      0,
      l,
      t,
      0,
      0,
      r,
      b,
      1,
      1,
      r,
      t,
      1,
      0,
    ]);
  }

  /**
   * @param {number} width
   * @param {number} height
   */
  _ensureCanvasSize(width, height) {
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }

  /**
   * @param {import('./index.js').Framebuffer} framebuffer
   */
  _ensureFramebufferResources(framebuffer) {
    const gl = this.gl;
    const { width, height, format } = framebuffer;

    if (
      this._frameInfo &&
      this._frameInfo.width === width &&
      this._frameInfo.height === height &&
      this._frameInfo.format === format
    ) {
      return;
    }

    if (this._frameTex) gl.deleteTexture(this._frameTex);
    if (this._indexTex) gl.deleteTexture(this._indexTex);
    if (this._paletteTex) gl.deleteTexture(this._paletteTex);

    this._frameTex = null;
    this._indexTex = null;
    this._paletteTex = null;

    if (format === 'indexed8') {
      const indexTex = assertNonNull(gl.createTexture(), 'createTexture index');
      gl.bindTexture(gl.TEXTURE_2D, indexTex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(
        gl.TEXTURE_2D,
        0,
        gl.R8UI,
        width,
        height,
        0,
        gl.RED_INTEGER,
        gl.UNSIGNED_BYTE,
        null,
      );

      const paletteTex = assertNonNull(gl.createTexture(), 'createTexture palette');
      gl.bindTexture(gl.TEXTURE_2D, paletteTex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);

      this._indexTex = indexTex;
      this._paletteTex = paletteTex;
    } else {
      const tex = assertNonNull(gl.createTexture(), 'createTexture framebuffer');
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

      if (format === 'rgb565') {
        gl.texImage2D(
          gl.TEXTURE_2D,
          0,
          gl.RGB565,
          width,
          height,
          0,
          gl.RGB,
          gl.UNSIGNED_SHORT_5_6_5,
          null,
        );
      } else {
        // rgba8 and bgra8 (via extension or CPU conversion).
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      }

      this._frameTex = tex;
    }

    gl.bindTexture(gl.TEXTURE_2D, null);
    this._frameInfo = { width, height, format };
  }

  /**
   * @param {import('./index.js').Framebuffer} framebuffer
   */
  _uploadFramebuffer(framebuffer) {
    const gl = this.gl;
    const { width, height, format } = framebuffer;

    if (format === 'indexed8') {
      const indices = asU8(framebuffer.data);
      gl.bindTexture(gl.TEXTURE_2D, this._indexTex);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        0,
        0,
        width,
        height,
        gl.RED_INTEGER,
        gl.UNSIGNED_BYTE,
        indices,
      );

      if (framebuffer.paletteRgba8) {
        gl.bindTexture(gl.TEXTURE_2D, this._paletteTex);
        gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
        gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, 256, 1, gl.RGBA, gl.UNSIGNED_BYTE, framebuffer.paletteRgba8);
      }

      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    gl.bindTexture(gl.TEXTURE_2D, this._frameTex);

    if (format === 'rgb565') {
      const src = asU16(framebuffer.data);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 2);
      gl.texSubImage2D(
        gl.TEXTURE_2D,
        0,
        0,
        0,
        width,
        height,
        gl.RGB,
        gl.UNSIGNED_SHORT_5_6_5,
        src,
      );
      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    if (format === 'bgra8') {
      const src = asU8(framebuffer.data);
      if (this._bgraExt && this._bgraExt.BGRA_EXT) {
        gl.texSubImage2D(
          gl.TEXTURE_2D,
          0,
          0,
          0,
          width,
          height,
          this._bgraExt.BGRA_EXT,
          gl.UNSIGNED_BYTE,
          src,
        );
        gl.bindTexture(gl.TEXTURE_2D, null);
        return;
      }

      // CPU fallback if BGRA extension isn't available.
      const rgba = bgra8ToRgba8(src);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    if (format === 'rgba8') {
      const src = asU8(framebuffer.data);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, src);
      gl.bindTexture(gl.TEXTURE_2D, null);
      return;
    }

    // Defensive fallback for any unexpected format: convert to RGBA8 on CPU.
    let rgba;
    if (format === 'indexed8') {
      if (!framebuffer.paletteRgba8) throw new Error('indexed8 framebuffer missing paletteRgba8');
      rgba = indexed8ToRgba8(asU8(framebuffer.data), framebuffer.paletteRgba8);
    } else if (format === 'rgb565') {
      rgba = rgb565ToRgba8(asU16(framebuffer.data));
    } else if (format === 'bgra8') {
      rgba = bgra8ToRgba8(asU8(framebuffer.data));
    } else {
      throw new Error(`Unsupported framebuffer format: ${format}`);
    }

    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, rgba);
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  /**
   * Present a framebuffer to the canvas.
   *
   * @param {import('./index.js').Framebuffer} framebuffer
   * @param {import('./index.js').Blit[]} [blits]
   */
  present(framebuffer, blits = []) {
    this._ensureCanvasSize(framebuffer.width, framebuffer.height);
    this._ensureFramebufferResources(framebuffer);
    this._uploadFramebuffer(framebuffer);

    const gl = this.gl;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.bindVertexArray(this._quadVao);

    // Fullscreen quad vertices.
    const verts = this._quadVertsForRect(0, 0, this.canvas.width, this.canvas.height);
    gl.bindBuffer(gl.ARRAY_BUFFER, this._quadVbo);
    gl.bufferSubData(gl.ARRAY_BUFFER, 0, verts);

    if (framebuffer.format === 'indexed8') {
      gl.useProgram(this._programIndexed);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this._indexTex);
      gl.uniform1i(this._locIndexed.uIndexTex, 0);
      gl.activeTexture(gl.TEXTURE1);
      gl.bindTexture(gl.TEXTURE_2D, this._paletteTex);
      gl.uniform1i(this._locIndexed.uPaletteTex, 1);
    } else {
      gl.useProgram(this._programRgba);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, this._frameTex);
      gl.uniform1i(this._locRgba.uTex, 0);
    }

    gl.drawArrays(gl.TRIANGLES, 0, 6);

    if (blits.length > 0) {
      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
      gl.useProgram(this._programRgba);
      gl.uniform1i(this._locRgba.uTex, 0);

      if (!this._blitTex) {
        this._blitTex = assertNonNull(gl.createTexture(), 'createTexture blit');
        gl.bindTexture(gl.TEXTURE_2D, this._blitTex);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.bindTexture(gl.TEXTURE_2D, null);
      }

      for (const blit of blits) {
        let rgba;
        if (blit.format === 'rgba8') {
          rgba = asU8(blit.data);
        } else if (blit.format === 'bgra8') {
          rgba = bgra8ToRgba8(asU8(blit.data));
        } else if (blit.format === 'rgb565') {
          rgba = rgb565ToRgba8(asU16(blit.data));
        } else if (blit.format === 'indexed8') {
          if (!blit.paletteRgba8) throw new Error('indexed8 blit missing paletteRgba8');
          rgba = indexed8ToRgba8(asU8(blit.data), blit.paletteRgba8);
        } else {
          throw new Error(`Unsupported blit format: ${blit.format}`);
        }

        if (!this._blitInfo || this._blitInfo.width !== blit.width || this._blitInfo.height !== blit.height) {
          gl.bindTexture(gl.TEXTURE_2D, this._blitTex);
          gl.texImage2D(
            gl.TEXTURE_2D,
            0,
            gl.RGBA8,
            blit.width,
            blit.height,
            0,
            gl.RGBA,
            gl.UNSIGNED_BYTE,
            null,
          );
          gl.bindTexture(gl.TEXTURE_2D, null);
          this._blitInfo = { width: blit.width, height: blit.height };
        }

        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this._blitTex);
        gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
        gl.texSubImage2D(
          gl.TEXTURE_2D,
          0,
          0,
          0,
          blit.width,
          blit.height,
          gl.RGBA,
          gl.UNSIGNED_BYTE,
          rgba,
        );

        const rectVerts = this._quadVertsForRect(blit.x, blit.y, blit.width, blit.height);
        gl.bindBuffer(gl.ARRAY_BUFFER, this._quadVbo);
        gl.bufferSubData(gl.ARRAY_BUFFER, 0, rectVerts);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
      }

      gl.disable(gl.BLEND);
    }

    gl.bindVertexArray(null);
    gl.bindTexture(gl.TEXTURE_2D, null);
  }

  drawTestTriangle() {
    const gl = this.gl;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);
    gl.clearColor(0.05, 0.05, 0.08, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.useProgram(this._triangleProgram.program);
    gl.bindVertexArray(this._triangleProgram.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
  }

  destroy() {
    const gl = this.gl;
    if (this._frameTex) gl.deleteTexture(this._frameTex);
    if (this._indexTex) gl.deleteTexture(this._indexTex);
    if (this._paletteTex) gl.deleteTexture(this._paletteTex);
    if (this._blitTex) gl.deleteTexture(this._blitTex);
    gl.deleteBuffer(this._quadVbo);
    gl.deleteVertexArray(this._quadVao);
    gl.deleteProgram(this._programRgba);
    gl.deleteProgram(this._programIndexed);
    gl.deleteProgram(this._triangleProgram.program);
    gl.deleteVertexArray(this._triangleProgram.vao);
  }
}
