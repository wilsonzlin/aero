// Aero GPU trace replayer (standalone, browser-side).
//
// This file is intentionally written as "TS that is also valid JS" (no type syntax),
// so it can be injected directly into a browser page without a build step.
//
// Usage from the browser console:
//   const bytes = await fetch('trace.aerogputrace').then(r => r.arrayBuffer());
//   const canvas = document.querySelector('canvas');
//   const r = await window.AeroGpuTraceReplay.load(bytes, canvas);
//   await r.step(); // replay frame 0
//
// See docs/abi/gpu-trace-format.md.

(function () {
  const TRACE_MAGIC = asciiBytes("AEROGPUT");
  const TOC_MAGIC = asciiBytes("AEROTOC\0");
  const FOOTER_MAGIC = asciiBytes("AEROGPUF");

  const RECORD_BEGIN_FRAME = 0x01;
  const RECORD_PRESENT = 0x02;
  const RECORD_PACKET = 0x03;
  const RECORD_BLOB = 0x04;

  const BLOB_BUFFER_DATA = 0x01;
  const BLOB_SHADER_DXBC = 0x03;
  const BLOB_SHADER_WGSL = 0x04;
  const BLOB_SHADER_GLSL_ES300 = 0x05;

  // Minimal command ABI v1 opcodes (Appendix A).
  const OP_CREATE_BUFFER = 0x0001;
  const OP_UPLOAD_BUFFER = 0x0002;
  const OP_CREATE_SHADER = 0x0003;
  const OP_CREATE_PIPELINE = 0x0004;
  const OP_SET_PIPELINE = 0x0005;
  const OP_SET_VERTEX_BUFFER = 0x0006;
  const OP_SET_VIEWPORT = 0x0007;
  const OP_CLEAR = 0x0008;
  const OP_DRAW = 0x0009;
  const OP_PRESENT = 0x000a;

  function asciiBytes(s) {
    const out = new Uint8Array(s.length);
    for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 0xff;
    return out;
  }

  function bytesEqual(a, b) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }

  function readU32(view, off) {
    return view.getUint32(off, true);
  }

  function readU64Big(view, off) {
    const lo = BigInt(view.getUint32(off + 0, true));
    const hi = BigInt(view.getUint32(off + 4, true));
    return (hi << 32n) | lo;
  }

  function readF32(view, off) {
    return view.getFloat32(off, true);
  }

  function decodeUtf8(bytes) {
    return new TextDecoder("utf-8").decode(bytes);
  }

  function fail(msg) {
    throw new Error("AeroGpuTraceReplay: " + msg);
  }

  function parseTrace(bytesLike) {
    const bytes =
      bytesLike instanceof Uint8Array
        ? bytesLike
        : new Uint8Array(bytesLike);
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let off = 0;

    if (!bytesEqual(bytes.subarray(0, 8), TRACE_MAGIC)) fail("bad trace magic");
    off += 8;
    const headerSize = readU32(view, off);
    off += 4;
    if (headerSize !== 32) fail("unsupported header_size=" + headerSize);
    const containerVersion = readU32(view, off);
    off += 4;
    if (containerVersion !== 1) fail("unsupported container_version=" + containerVersion);
    const commandAbiVersion = readU32(view, off);
    off += 4;
    const flags = readU32(view, off);
    off += 4;
    const metaLen = readU32(view, off);
    off += 4;
    off += 4; // reserved

    const metaBytes = bytes.subarray(off, off + metaLen);
    off += metaLen;
    let meta = null;
    try {
      meta = JSON.parse(decodeUtf8(metaBytes));
    } catch {
      meta = null;
    }

    // Footer is 32 bytes.
    if (bytes.length < 32) fail("trace too small");
    const footerOff = bytes.length - 32;
    if (!bytesEqual(bytes.subarray(footerOff, footerOff + 8), FOOTER_MAGIC)) {
      fail("bad footer magic");
    }
    const footerView = new DataView(bytes.buffer, bytes.byteOffset + footerOff, 32);
    const footerSize = readU32(footerView, 8);
    if (footerSize !== 32) fail("unsupported footer_size=" + footerSize);
    const footerContainerVersion = readU32(footerView, 12);
    if (footerContainerVersion !== containerVersion) fail("footer/header version mismatch");
    const tocOffset = Number(readU64Big(footerView, 16));
    const tocLen = Number(readU64Big(footerView, 24));

    if (tocOffset + tocLen > bytes.length) fail("toc out of bounds");

    // Parse TOC.
    const tocView = new DataView(bytes.buffer, bytes.byteOffset + tocOffset, tocLen);
    if (!bytesEqual(bytes.subarray(tocOffset, tocOffset + 8), TOC_MAGIC)) fail("bad toc magic");
    const tocVersion = readU32(tocView, 8);
    if (tocVersion !== 1) fail("unsupported toc_version=" + tocVersion);
    const frameCount = readU32(tocView, 12);
    const expectedTocLen = 16 + frameCount * 32;
    if (tocLen !== expectedTocLen) fail("toc_len mismatch");
    const frames = [];
    let tocOff = 16;
    for (let i = 0; i < frameCount; i++) {
      const frameIndex = readU32(tocView, tocOff + 0);
      const frameFlags = readU32(tocView, tocOff + 4);
      const startOffset = Number(readU64Big(tocView, tocOff + 8));
      const presentOffset = Number(readU64Big(tocView, tocOff + 16));
      const endOffset = Number(readU64Big(tocView, tocOff + 24));
      frames.push({ frameIndex, frameFlags, startOffset, presentOffset, endOffset });
      tocOff += 32;
    }

    // Scan record stream once to collect blobs and per-frame packets.
    const blobs = new Map(); // bigint -> Uint8Array
    const framePackets = new Map(); // frameIndex -> Uint8Array[]
    let currentFrame = null;

    let recOff = off;
    while (recOff < tocOffset) {
      if (recOff + 8 > tocOffset) fail("record header out of bounds");
      const rType = view.getUint8(recOff + 0);
      const payloadLen = readU32(view, recOff + 4);
      const payloadOff = recOff + 8;
      const payloadEnd = payloadOff + payloadLen;
      if (payloadEnd > tocOffset) fail("record payload out of bounds");

      if (rType === RECORD_BEGIN_FRAME) {
        const frameIndex = readU32(view, payloadOff);
        currentFrame = frameIndex;
        if (!framePackets.has(frameIndex)) framePackets.set(frameIndex, []);
      } else if (rType === RECORD_PRESENT) {
        currentFrame = null;
      } else if (rType === RECORD_PACKET) {
        if (currentFrame === null) fail("packet outside of a frame");
        const pkt = bytes.subarray(payloadOff, payloadEnd);
        framePackets.get(currentFrame).push(pkt);
      } else if (rType === RECORD_BLOB) {
        if (payloadLen < 16) fail("malformed blob record");
        const blobId = readU64Big(view, payloadOff + 0);
        const kind = readU32(view, payloadOff + 8);
        const blobBytes = bytes.subarray(payloadOff + 16, payloadEnd);
        blobs.set(blobId, { kind, bytes: blobBytes });
      } else {
        fail("unknown record_type=" + rType);
      }

      recOff = payloadEnd;
    }

    return {
      containerVersion,
      commandAbiVersion,
      flags,
      meta,
      frames,
      blobs,
      framePackets,
    };
  }

  function createWebgl2Backend(canvas) {
    const gl = canvas.getContext("webgl2", { preserveDrawingBuffer: true });
    if (!gl) fail("WebGL2 is not available");

    // Reduce driver variance for screenshot comparisons.
    gl.disable(gl.DITHER);
    gl.disable(gl.BLEND);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

    const buffers = new Map(); // u32 -> WebGLBuffer
    const shaders = new Map(); // u32 -> { stage, glslSource }
    const programs = new Map(); // u32 -> WebGLProgram

    function compileShader(stage, src) {
      const shader = gl.createShader(stage);
      if (!shader) fail("gl.createShader failed");
      gl.shaderSource(shader, src);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        const log = gl.getShaderInfoLog(shader) || "(no log)";
        fail("shader compile failed: " + log);
      }
      return shader;
    }

    function linkProgram(vsSrc, fsSrc) {
      const vs = compileShader(gl.VERTEX_SHADER, vsSrc);
      const fs = compileShader(gl.FRAGMENT_SHADER, fsSrc);
      const prog = gl.createProgram();
      if (!prog) fail("gl.createProgram failed");
      gl.attachShader(prog, vs);
      gl.attachShader(prog, fs);
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
        const log = gl.getProgramInfoLog(prog) || "(no log)";
        fail("program link failed: " + log);
      }
      // Shaders can be deleted after link.
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      return prog;
    }

    let currentProgram = null;

    async function executePacket(packetBytes, trace) {
      const pv = new DataView(packetBytes.buffer, packetBytes.byteOffset, packetBytes.byteLength);
      const opcode = readU32(pv, 0);
      const totalDwords = readU32(pv, 4);
      if (totalDwords * 4 !== packetBytes.byteLength) {
        fail("packet dword length mismatch");
      }

      function u32AtPayload(i) {
        return readU32(pv, 8 + i * 4);
      }
      function u64AtPayload(i) {
        const lo = BigInt(u32AtPayload(i + 0));
        const hi = BigInt(u32AtPayload(i + 1));
        return (hi << 32n) | lo;
      }

      switch (opcode) {
        case OP_CREATE_BUFFER: {
          const bufferId = u32AtPayload(0);
          // size_bytes is currently advisory for WebGL2.
          const glBuf = gl.createBuffer();
          if (!glBuf) fail("gl.createBuffer failed");
          buffers.set(bufferId, glBuf);
          break;
        }
        case OP_UPLOAD_BUFFER: {
          const bufferId = u32AtPayload(0);
          const offsetBytes = u32AtPayload(1);
          const dataLenBytes = u32AtPayload(2);
          const blobId = u64AtPayload(3);
          const blob = trace.blobs.get(blobId);
          if (!blob) fail("missing blob_id=" + blobId.toString());
          if (blob.kind !== BLOB_BUFFER_DATA) fail("unexpected blob kind for UPLOAD_BUFFER");
          if (blob.bytes.byteLength !== dataLenBytes) fail("data_len_bytes mismatch");
          const glBuf = buffers.get(bufferId);
          if (!glBuf) fail("unknown buffer_id=" + bufferId);
          gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
          if (offsetBytes === 0) {
            gl.bufferData(gl.ARRAY_BUFFER, blob.bytes, gl.STATIC_DRAW);
          } else {
            gl.bufferSubData(gl.ARRAY_BUFFER, offsetBytes, blob.bytes);
          }
          break;
        }
        case OP_CREATE_SHADER: {
          const shaderId = u32AtPayload(0);
          const stage = u32AtPayload(1); // 0=VS, 1=FS
          const glslBlobId = u64AtPayload(2);
          const wgslBlobId = u64AtPayload(4);
          const dxbcBlobId = u64AtPayload(6);
          // Currently, WebGL2 replayer uses GLSL ES source.
          if (glslBlobId === 0n) fail("CREATE_SHADER missing GLSL blob");
          const glslBlob = trace.blobs.get(glslBlobId);
          if (!glslBlob) fail("missing glsl_blob_id=" + glslBlobId.toString());
          if (glslBlob.kind !== BLOB_SHADER_GLSL_ES300) fail("unexpected GLSL blob kind");
          // Parse for nicer errors early.
          const glslSrc = decodeUtf8(glslBlob.bytes);
          shaders.set(shaderId, { stage, glslSrc, wgslBlobId, dxbcBlobId });
          break;
        }
        case OP_CREATE_PIPELINE: {
          const pipelineId = u32AtPayload(0);
          const vsId = u32AtPayload(1);
          const fsId = u32AtPayload(2);
          const vs = shaders.get(vsId);
          const fs = shaders.get(fsId);
          if (!vs || !fs) fail("missing shader for pipeline");
          const prog = linkProgram(vs.glslSrc, fs.glslSrc);
          programs.set(pipelineId, prog);
          break;
        }
        case OP_SET_PIPELINE: {
          const pipelineId = u32AtPayload(0);
          const prog = programs.get(pipelineId);
          if (!prog) fail("unknown pipeline_id=" + pipelineId);
          gl.useProgram(prog);
          currentProgram = prog;
          break;
        }
        case OP_SET_VERTEX_BUFFER: {
          if (!currentProgram) fail("SET_VERTEX_BUFFER without pipeline");
          const bufferId = u32AtPayload(0);
          const stride = u32AtPayload(1);
          const posOff = u32AtPayload(2);
          const colorOff = u32AtPayload(3);
          const glBuf = buffers.get(bufferId);
          if (!glBuf) fail("unknown buffer_id=" + bufferId);
          gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
          // a_position @location(0): vec2<f32>
          gl.enableVertexAttribArray(0);
          gl.vertexAttribPointer(0, 2, gl.FLOAT, false, stride, posOff);
          // a_color @location(1): vec4<f32>
          gl.enableVertexAttribArray(1);
          gl.vertexAttribPointer(1, 4, gl.FLOAT, false, stride, colorOff);
          break;
        }
        case OP_SET_VIEWPORT: {
          const width = u32AtPayload(0);
          const height = u32AtPayload(1);
          const w = width === 0 ? canvas.width : width;
          const h = height === 0 ? canvas.height : height;
          gl.viewport(0, 0, w, h);
          break;
        }
        case OP_CLEAR: {
          const r = readF32(pv, 8 + 0);
          const g = readF32(pv, 8 + 4);
          const b = readF32(pv, 8 + 8);
          const a = readF32(pv, 8 + 12);
          gl.clearColor(r, g, b, a);
          gl.clear(gl.COLOR_BUFFER_BIT);
          break;
        }
        case OP_DRAW: {
          const vertexCount = u32AtPayload(0);
          const firstVertex = u32AtPayload(1);
          gl.drawArrays(gl.TRIANGLES, firstVertex, vertexCount);
          break;
        }
        case OP_PRESENT: {
          gl.finish();
          break;
        }
        default:
          fail("unknown opcode=0x" + opcode.toString(16));
      }
    }

    function readPixels() {
      const out = new Uint8Array(canvas.width * canvas.height * 4);
      gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, out);
      return out;
    }

    return { gl, executePacket, readPixels };
  }

  async function createWebgpuBackend(canvas) {
    const gpu = navigator.gpu;
    if (!gpu) fail("WebGPU is not available");
    const adapter = await gpu.requestAdapter();
    if (!adapter) fail("navigator.gpu.requestAdapter() returned null");
    const device = await adapter.requestDevice();

    const ctx = canvas.getContext("webgpu");
    if (!ctx) fail("canvas.getContext('webgpu') returned null");

    const format =
      typeof gpu.getPreferredCanvasFormat === "function"
        ? gpu.getPreferredCanvasFormat()
        : "bgra8unorm";
    ctx.configure({ device, format, alphaMode: "opaque" });

    const buffers = new Map(); // u32 -> GPUBuffer
    const shaders = new Map(); // u32 -> { stage, module }
    const pipelines = new Map(); // u32 -> GPURenderPipeline

    let currentPipelineId = null;
    let currentVertexBufferId = null;
    let currentViewport = null; // { w, h }
    let clearColor = { r: 0, g: 0, b: 0, a: 1 };

    let encoder = null;
    let pass = null;

    function beginPass(loadOp) {
      if (pass) return;
      encoder = device.createCommandEncoder();
      const view = ctx.getCurrentTexture().createView();
      pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view,
            loadOp,
            clearValue: clearColor,
            storeOp: "store",
          },
        ],
      });

      if (currentPipelineId !== null) {
        const p = pipelines.get(currentPipelineId);
        if (p) pass.setPipeline(p);
      }
      if (currentVertexBufferId !== null) {
        const b = buffers.get(currentVertexBufferId);
        if (b) pass.setVertexBuffer(0, b);
      }
      if (currentViewport) {
        pass.setViewport(0, 0, currentViewport.w, currentViewport.h, 0, 1);
      }
    }

    async function executePacket(packetBytes, trace) {
      const pv = new DataView(packetBytes.buffer, packetBytes.byteOffset, packetBytes.byteLength);
      const opcode = readU32(pv, 0);
      const totalDwords = readU32(pv, 4);
      if (totalDwords * 4 !== packetBytes.byteLength) fail("packet dword length mismatch");

      function u32AtPayload(i) {
        return readU32(pv, 8 + i * 4);
      }
      function u64AtPayload(i) {
        const lo = BigInt(u32AtPayload(i + 0));
        const hi = BigInt(u32AtPayload(i + 1));
        return (hi << 32n) | lo;
      }

      switch (opcode) {
        case OP_CREATE_BUFFER: {
          const bufferId = u32AtPayload(0);
          const sizeBytes = u32AtPayload(1);
          const buf = device.createBuffer({
            size: sizeBytes,
            usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
          });
          buffers.set(bufferId, buf);
          break;
        }
        case OP_UPLOAD_BUFFER: {
          const bufferId = u32AtPayload(0);
          const offsetBytes = u32AtPayload(1);
          const dataLenBytes = u32AtPayload(2);
          const blobId = u64AtPayload(3);
          const blob = trace.blobs.get(blobId);
          if (!blob) fail("missing blob_id=" + blobId.toString());
          if (blob.kind !== BLOB_BUFFER_DATA) fail("unexpected blob kind for UPLOAD_BUFFER");
          if (blob.bytes.byteLength !== dataLenBytes) fail("data_len_bytes mismatch");
          const buf = buffers.get(bufferId);
          if (!buf) fail("unknown buffer_id=" + bufferId);
          device.queue.writeBuffer(buf, offsetBytes, blob.bytes);
          break;
        }
        case OP_CREATE_SHADER: {
          const shaderId = u32AtPayload(0);
          const stage = u32AtPayload(1);
          const wgslBlobId = u64AtPayload(4);
          if (wgslBlobId === 0n) fail("CREATE_SHADER missing WGSL blob");
          const wgslBlob = trace.blobs.get(wgslBlobId);
          if (!wgslBlob) fail("missing wgsl_blob_id=" + wgslBlobId.toString());
          if (wgslBlob.kind !== BLOB_SHADER_WGSL) fail("unexpected WGSL blob kind");
          const wgslSrc = decodeUtf8(wgslBlob.bytes);
          const module = device.createShaderModule({ code: wgslSrc });
          shaders.set(shaderId, { stage, module });
          break;
        }
        case OP_CREATE_PIPELINE: {
          const pipelineId = u32AtPayload(0);
          const vsId = u32AtPayload(1);
          const fsId = u32AtPayload(2);
          const vs = shaders.get(vsId);
          const fs = shaders.get(fsId);
          if (!vs || !fs) fail("missing shader for pipeline");
          const pipeline = device.createRenderPipeline({
            layout: "auto",
            vertex: {
              module: vs.module,
              entryPoint: "vs_main",
              buffers: [
                {
                  arrayStride: 24,
                  attributes: [
                    { shaderLocation: 0, offset: 0, format: "float32x2" },
                    { shaderLocation: 1, offset: 8, format: "float32x4" },
                  ],
                },
              ],
            },
            fragment: {
              module: fs.module,
              entryPoint: "fs_main",
              targets: [{ format }],
            },
            primitive: { topology: "triangle-list", cullMode: "none" },
          });
          pipelines.set(pipelineId, pipeline);
          break;
        }
        case OP_SET_PIPELINE: {
          const pipelineId = u32AtPayload(0);
          currentPipelineId = pipelineId;
          if (pass) {
            const p = pipelines.get(pipelineId);
            if (!p) fail("unknown pipeline_id=" + pipelineId);
            pass.setPipeline(p);
          }
          break;
        }
        case OP_SET_VERTEX_BUFFER: {
          const bufferId = u32AtPayload(0);
          currentVertexBufferId = bufferId;
          if (pass) {
            const b = buffers.get(bufferId);
            if (!b) fail("unknown buffer_id=" + bufferId);
            pass.setVertexBuffer(0, b);
          }
          break;
        }
        case OP_SET_VIEWPORT: {
          const width = u32AtPayload(0);
          const height = u32AtPayload(1);
          const w = width === 0 ? canvas.width : width;
          const h = height === 0 ? canvas.height : height;
          currentViewport = { w, h };
          if (pass) pass.setViewport(0, 0, w, h, 0, 1);
          break;
        }
        case OP_CLEAR: {
          clearColor = {
            r: readF32(pv, 8 + 0),
            g: readF32(pv, 8 + 4),
            b: readF32(pv, 8 + 8),
            a: readF32(pv, 8 + 12),
          };
          beginPass("clear");
          break;
        }
        case OP_DRAW: {
          const vertexCount = u32AtPayload(0);
          const firstVertex = u32AtPayload(1);
          beginPass("load");
          pass.draw(vertexCount, 1, firstVertex, 0);
          break;
        }
        case OP_PRESENT: {
          if (pass) {
            pass.end();
            pass = null;
          }
          if (encoder) {
            device.queue.submit([encoder.finish()]);
            encoder = null;
            await device.queue.onSubmittedWorkDone();
          }
          break;
        }
        default:
          fail("unknown opcode=0x" + opcode.toString(16));
      }
    }

    function dumpScreenshotDataUrl() {
      return canvas.toDataURL("image/png");
    }

    return { device, executePacket, dumpScreenshotDataUrl };
  }

  async function loadTrace(bytesLike, canvas, opts) {
    const trace = parseTrace(bytesLike);
    const backendName = (opts && opts.backend) || "webgl2";
    const backend =
      backendName === "webgpu"
        ? await createWebgpuBackend(canvas)
        : createWebgl2Backend(canvas);
    let cursor = 0;
    let playing = false;

    async function replayFrame(frameIndex) {
      const packets = trace.framePackets.get(frameIndex);
      if (!packets) fail("no such frame " + frameIndex);
      for (const pkt of packets) await backend.executePacket(pkt, trace);
    }

    async function step() {
      if (cursor >= trace.frames.length) return false;
      const frame = trace.frames[cursor];
      await replayFrame(frame.frameIndex);
      cursor++;
      return true;
    }

    async function play(opts) {
      const fps = (opts && opts.fps) || 60;
      const delayMs = Math.max(1, Math.floor(1000 / fps));
      if (playing) return;
      playing = true;
      while (playing) {
        const ok = await step();
        if (!ok) {
          playing = false;
          break;
        }
        await new Promise((r) => setTimeout(r, delayMs));
      }
    }

    function pause() {
      playing = false;
    }

    function gotoFrame(frameIndex) {
      const idx = trace.frames.findIndex((f) => f.frameIndex === frameIndex);
      if (idx < 0) fail("no such frameIndex=" + frameIndex);
      cursor = idx;
    }

    function dumpScreenshotDataUrl() {
      if (backendName === "webgpu") return backend.dumpScreenshotDataUrl();
      return canvas.toDataURL("image/png");
    }

    return {
      trace,
      backend,
      replayFrame,
      step,
      play,
      pause,
      gotoFrame,
      dumpScreenshotDataUrl,
      readPixels: backend.readPixels,
    };
  }

  // Expose a console-friendly API.
  // (We don't use ESM exports so this can be injected into arbitrary pages/tests.)
  window.AeroGpuTraceReplay = {
    parseTrace,
    load: loadTrace,
  };
})();
