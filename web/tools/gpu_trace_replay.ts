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
  const BLOB_TEXTURE_DATA = 0x02;
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

  // AeroGPU command ABI (crates/aero-gpu-device/src/abi.rs).
  // Packet layout starts with magic "AGPC" (little-endian).
  const AEROGPU_CMD_MAGIC = asciiBytes("AGPC");
  const AEROGPU_OPCODE_CREATE_BUFFER = 0x0001;
  const AEROGPU_OPCODE_DESTROY_BUFFER = 0x0002;
  const AEROGPU_OPCODE_WRITE_BUFFER = 0x0003;
  const AEROGPU_OPCODE_CREATE_TEXTURE2D = 0x0010;
  const AEROGPU_OPCODE_DESTROY_TEXTURE = 0x0011;
  const AEROGPU_OPCODE_WRITE_TEXTURE2D = 0x0012;
  const AEROGPU_OPCODE_SET_RENDER_TARGET = 0x0020;
  const AEROGPU_OPCODE_CLEAR = 0x0021;
  const AEROGPU_OPCODE_SET_VIEWPORT = 0x0022;
  const AEROGPU_OPCODE_SET_PIPELINE = 0x0030;
  const AEROGPU_OPCODE_SET_VERTEX_BUFFER = 0x0031;
  const AEROGPU_OPCODE_DRAW = 0x0032;
  const AEROGPU_OPCODE_PRESENT = 0x0040;

  const AEROGPU_TEXFMT_RGBA8_UNORM = 1; // abi::TextureFormat::Rgba8Unorm
  const AEROGPU_PIPELINE_BASIC_VERTEX_COLOR = 1; // abi::pipeline::BASIC_VERTEX_COLOR

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

  function readU16(view, off) {
    return view.getUint16(off, true);
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

  function pushU32LE(out, value) {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setUint32(0, value >>> 0, true);
    out.push(b);
  }

  function pushU16LE(out, value) {
    const b = new Uint8Array(2);
    new DataView(b.buffer).setUint16(0, value & 0xffff, true);
    out.push(b);
  }

  function pushU8(out, value) {
    out.push(Uint8Array.of(value & 0xff));
  }

  function pushU64LEBig(out, value) {
    const b = new Uint8Array(8);
    const view = new DataView(b.buffer);
    const lo = Number(value & 0xffff_ffffn);
    const hi = Number((value >> 32n) & 0xffff_ffffn);
    view.setUint32(0, lo >>> 0, true);
    view.setUint32(4, hi >>> 0, true);
    out.push(b);
  }

  function concatChunks(chunks) {
    let len = 0;
    for (const c of chunks) len += c.byteLength;
    const out = new Uint8Array(len);
    let off = 0;
    for (const c of chunks) {
      out.set(c, off);
      off += c.byteLength;
    }
    return out;
  }

  function u64BigToSafeNumber(v, label) {
    const n = Number(v);
    if (!Number.isFinite(n) || !Number.isSafeInteger(n)) {
      fail("u64 out of JS safe integer range for " + label + ": " + v.toString());
    }
    return n;
  }

  function escapeJsonString(s) {
    let out = "";
    for (let i = 0; i < s.length; i++) {
      const c = s.charCodeAt(i);
      if (c === 0x22) out += '\\"';
      else if (c === 0x5c) out += "\\\\";
      else if (c === 0x0a) out += "\\n";
      else if (c === 0x0d) out += "\\r";
      else if (c === 0x09) out += "\\t";
      else if (c < 0x20) out += "\\u" + c.toString(16).padStart(4, "0");
      else out += String.fromCharCode(c);
    }
    return out;
  }

  function encodeMetaJson(meta) {
    // Keep field order and escaping stable to make traces byte-for-byte reproducible
    // across implementations (this mirrors `TraceMeta::to_json_bytes` in Rust).
    let json =
      '{"emulator_version":"' +
      escapeJsonString(String(meta.emulator_version || "")) +
      '","command_abi_version":' +
      String(meta.command_abi_version >>> 0);
    if (meta.notes !== undefined && meta.notes !== null) {
      json += ',"notes":"' + escapeJsonString(String(meta.notes)) + '"';
    }
    json += "}";
    return new TextEncoder().encode(json);
  }

  function f32ToBits(v) {
    const b = new ArrayBuffer(4);
    const dv = new DataView(b);
    dv.setFloat32(0, v, true);
    return dv.getUint32(0, true);
  }

  function packet(opcode, payloadU32) {
    const totalDwords = 2 + payloadU32.length;
    const bytes = new Uint8Array(totalDwords * 4);
    const dv = new DataView(bytes.buffer);
    dv.setUint32(0, opcode >>> 0, true);
    dv.setUint32(4, totalDwords >>> 0, true);
    for (let i = 0; i < payloadU32.length; i++) {
      dv.setUint32(8 + i * 4, payloadU32[i] >>> 0, true);
    }
    return bytes;
  }

  function u64BigToDwords(v) {
    const lo = Number(v & 0xffff_ffffn) >>> 0;
    const hi = Number((v >> 32n) & 0xffff_ffffn) >>> 0;
    return [lo, hi];
  }

  class TraceWriter {
    constructor(meta) {
      this.chunks = [];
      this.pos = 0;
      this.toc = [];
      this.openFrame = null;
      this.nextBlobId = 1n;
      this.blobs = new Map(); // bigint -> {kind, bytes}

      const metaBytes = encodeMetaJson(meta);

      // TraceHeader (32 bytes).
      this._pushBytes(TRACE_MAGIC);
      this._pushU32(32); // header_size
      this._pushU32(1); // container_version
      this._pushU32(meta.command_abi_version >>> 0);
      this._pushU32(0); // flags
      this._pushU32(metaBytes.byteLength >>> 0);
      this._pushU32(0); // reserved
      this._pushBytes(metaBytes);
    }

    _pushBytes(bytes) {
      this.chunks.push(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes));
      this.pos += bytes.byteLength >>> 0;
    }

    _pushU8(v) {
      const b = Uint8Array.of(v & 0xff);
      this._pushBytes(b);
    }

    _pushU16(v) {
      const b = new Uint8Array(2);
      new DataView(b.buffer).setUint16(0, v & 0xffff, true);
      this._pushBytes(b);
    }

    _pushU32(v) {
      const b = new Uint8Array(4);
      new DataView(b.buffer).setUint32(0, v >>> 0, true);
      this._pushBytes(b);
    }

    _pushU64Big(v) {
      const b = new Uint8Array(8);
      const dv = new DataView(b.buffer);
      dv.setUint32(0, Number(v & 0xffff_ffffn) >>> 0, true);
      dv.setUint32(4, Number((v >> 32n) & 0xffff_ffffn) >>> 0, true);
      this._pushBytes(b);
    }

    _writeRecord(recordType, payloadBytes) {
      if (payloadBytes.byteLength > 0xffff_ffff) fail("record payload too large");
      this._pushU8(recordType);
      this._pushU8(0); // flags
      this._pushU16(0); // reserved
      this._pushU32(payloadBytes.byteLength >>> 0);
      this._pushBytes(payloadBytes);
    }

    beginFrame(frameIndex) {
      if (this.openFrame !== null) fail("beginFrame while a frame is already open");
      const startOffset = this.pos;
      const payload = new Uint8Array(4);
      new DataView(payload.buffer).setUint32(0, frameIndex >>> 0, true);
      this._writeRecord(RECORD_BEGIN_FRAME, payload);
      this.toc.push({
        frameIndex: frameIndex >>> 0,
        flags: 0,
        startOffset,
        presentOffset: 0,
        endOffset: 0,
      });
      this.openFrame = this.toc.length - 1;
    }

    writePacket(packetBytes) {
      if (this.openFrame === null) fail("writePacket outside of a frame");
      this._writeRecord(RECORD_PACKET, packetBytes);
    }

    writeBlob(kind, dataBytes) {
      const blobId = this.nextBlobId;
      this.nextBlobId += 1n;

      const bytes = dataBytes instanceof Uint8Array ? dataBytes : new Uint8Array(dataBytes);
      this.blobs.set(blobId, { kind, bytes });

      const header = [];
      pushU64LEBig(header, blobId);
      pushU32LE(header, kind >>> 0);
      pushU32LE(header, 0);
      const payload = concatChunks([...header, bytes]);
      this._writeRecord(RECORD_BLOB, payload);
      return blobId;
    }

    present(frameIndex) {
      if (this.openFrame === null) fail("present outside of a frame");
      const slot = this.openFrame;
      const entry = this.toc[slot];
      if (entry.frameIndex !== (frameIndex >>> 0)) fail("present frame_index mismatch");

      const presentOffset = this.pos;
      const payload = new Uint8Array(4);
      new DataView(payload.buffer).setUint32(0, frameIndex >>> 0, true);
      this._writeRecord(RECORD_PRESENT, payload);

      entry.presentOffset = presentOffset;
      entry.endOffset = this.pos;
      this.openFrame = null;
    }

    finish() {
      if (this.openFrame !== null) fail("finish while a frame is still open");

      const tocOffset = this.pos;
      const tocChunks = [];
      tocChunks.push(TOC_MAGIC);
      pushU32LE(tocChunks, 1); // toc_version
      pushU32LE(tocChunks, this.toc.length >>> 0);
      for (const e of this.toc) {
        pushU32LE(tocChunks, e.frameIndex >>> 0);
        pushU32LE(tocChunks, e.flags >>> 0);
        pushU64LEBig(tocChunks, BigInt(e.startOffset));
        pushU64LEBig(tocChunks, BigInt(e.presentOffset));
        pushU64LEBig(tocChunks, BigInt(e.endOffset));
      }
      const tocBytes = concatChunks(tocChunks);
      this._pushBytes(tocBytes);
      const tocLen = tocBytes.byteLength;

      // Footer (32 bytes).
      const footerChunks = [];
      footerChunks.push(FOOTER_MAGIC);
      pushU32LE(footerChunks, 32); // footer_size
      pushU32LE(footerChunks, 1); // container_version
      pushU64LEBig(footerChunks, BigInt(tocOffset));
      pushU64LEBig(footerChunks, BigInt(tocLen));
      const footerBytes = concatChunks(footerChunks);
      this._pushBytes(footerBytes);

      return concatChunks(this.chunks);
    }
  }

  async function recordTriangleTrace(canvas, opts) {
    const backendName = (opts && opts.backend) || "webgl2";
    const width = (opts && opts.width) || canvas.width || 64;
    const height = (opts && opts.height) || canvas.height || 64;
    canvas.width = width;
    canvas.height = height;

    const backend =
      backendName === "webgpu"
        ? await createWebgpuBackend(canvas)
        : createWebgl2Backend(canvas);

    const meta = { emulator_version: "0.0.0-dev", command_abi_version: 1 };
    const w = new TraceWriter(meta);
    const trace = { blobs: w.blobs };

    w.beginFrame(0);

    // Vertex buffer (fullscreen triangle), interleaved [pos.xy, color.rgba] floats.
    const vertexValues = [
      -1.0, -1.0, 1.0, 0.0, 0.0, 1.0,
      3.0, -1.0, 1.0, 0.0, 0.0, 1.0,
      -1.0, 3.0, 1.0, 0.0, 0.0, 1.0,
    ];
    const vertexBytes = new Uint8Array(vertexValues.length * 4);
    const vertexView = new DataView(vertexBytes.buffer);
    for (let i = 0; i < vertexValues.length; i++) {
      vertexView.setFloat32(i * 4, vertexValues[i], true);
    }
    const vertexBlobId = w.writeBlob(BLOB_BUFFER_DATA, vertexBytes);

    const GLSL_VS = `#version 300 es
precision highp float;
layout(location=0) in vec2 a_position;
layout(location=1) in vec4 a_color;
out vec4 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_position, 0.0, 1.0);
}
`;

    const GLSL_FS = `#version 300 es
precision highp float;
in vec4 v_color;
out vec4 o_color;
void main() {
  o_color = v_color;
}
`;

    const WGSL_VS = `
struct VsIn {
  @location(0) position: vec2<f32>,
  @location(1) color: vec4<f32>,
}
struct VsOut {
  @builtin(position) position: vec4<f32>,
  @location(0) color: vec4<f32>,
}
@vertex
fn vs_main(input: VsIn) -> VsOut {
  var out: VsOut;
  out.position = vec4<f32>(input.position, 0.0, 1.0);
  out.color = input.color;
  return out;
}
`;

    const WGSL_FS = `
@fragment
fn fs_main(@location(0) color: vec4<f32>) -> @location(0) vec4<f32> {
  return color;
}
`;

    const DXBC_STUB = new Uint8Array([68, 88, 66, 67, 83, 84, 85, 66]); // "DXBCSTUB"

    const glslVsBlob = w.writeBlob(BLOB_SHADER_GLSL_ES300, new TextEncoder().encode(GLSL_VS));
    const glslFsBlob = w.writeBlob(BLOB_SHADER_GLSL_ES300, new TextEncoder().encode(GLSL_FS));
    const wgslVsBlob = w.writeBlob(BLOB_SHADER_WGSL, new TextEncoder().encode(WGSL_VS));
    const wgslFsBlob = w.writeBlob(BLOB_SHADER_WGSL, new TextEncoder().encode(WGSL_FS));
    const dxbcVsBlob = w.writeBlob(BLOB_SHADER_DXBC, DXBC_STUB);
    const dxbcFsBlob = w.writeBlob(BLOB_SHADER_DXBC, DXBC_STUB);

    const bufferId = 1;
    const vsId = 1;
    const fsId = 2;
    const pipelineId = 1;

    const vsizeBytes = vertexBytes.byteLength >>> 0;

    async function submit(pkt) {
      w.writePacket(pkt);
      await backend.executePacket(pkt, trace);
    }

    await submit(packet(OP_CREATE_BUFFER, [bufferId, vsizeBytes, 0]));

    const [vblobLo, vblobHi] = u64BigToDwords(vertexBlobId);
    await submit(packet(OP_UPLOAD_BUFFER, [bufferId, 0, vsizeBytes, vblobLo, vblobHi]));

    const [vsGlslLo, vsGlslHi] = u64BigToDwords(glslVsBlob);
    const [vsWgslLo, vsWgslHi] = u64BigToDwords(wgslVsBlob);
    const [vsDxbcLo, vsDxbcHi] = u64BigToDwords(dxbcVsBlob);
    await submit(
      packet(OP_CREATE_SHADER, [
        vsId, 0,
        vsGlslLo, vsGlslHi,
        vsWgslLo, vsWgslHi,
        vsDxbcLo, vsDxbcHi,
      ]),
    );

    const [fsGlslLo, fsGlslHi] = u64BigToDwords(glslFsBlob);
    const [fsWgslLo, fsWgslHi] = u64BigToDwords(wgslFsBlob);
    const [fsDxbcLo, fsDxbcHi] = u64BigToDwords(dxbcFsBlob);
    await submit(
      packet(OP_CREATE_SHADER, [
        fsId, 1,
        fsGlslLo, fsGlslHi,
        fsWgslLo, fsWgslHi,
        fsDxbcLo, fsDxbcHi,
      ]),
    );

    await submit(packet(OP_CREATE_PIPELINE, [pipelineId, vsId, fsId]));
    await submit(packet(OP_SET_PIPELINE, [pipelineId]));

    const stride = 6 * 4;
    await submit(packet(OP_SET_VERTEX_BUFFER, [bufferId, stride, 0, 2 * 4]));

    await submit(packet(OP_SET_VIEWPORT, [0, 0]));
    await submit(packet(OP_CLEAR, [f32ToBits(0), f32ToBits(0), f32ToBits(0), f32ToBits(1)]));
    await submit(packet(OP_DRAW, [3, 0]));
    await submit(packet(OP_PRESENT, []));

    w.present(0);

    const bytes = w.finish();

    return {
      bytes,
      dumpScreenshotDataUrl: () => canvas.toDataURL("image/png"),
      readPixels: backend.readPixels,
    };
  }

  async function saveToOpfs(path, bytesLike) {
    if (!navigator.storage || typeof navigator.storage.getDirectory !== "function") {
      fail("OPFS unavailable (navigator.storage.getDirectory missing)");
    }
    const bytes =
      bytesLike instanceof Uint8Array ? bytesLike : new Uint8Array(bytesLike);
    const parts = String(path)
      .trim()
      .split("/")
      .filter((p) => p.length > 0);
    if (parts.length === 0) fail("OPFS path must not be empty");
    if (parts.some((p) => p === "." || p === "..")) fail('OPFS path must not contain "." or ".."');

    const filename = parts.pop();
    if (!filename) fail("OPFS path must include filename");
    let dir = await navigator.storage.getDirectory();
    for (const part of parts) {
      dir = await dir.getDirectoryHandle(part, { create: true });
    }
    const handle = await dir.getFileHandle(filename, { create: true });
    const writable = await handle.createWritable();
    await writable.write(bytes);
    await writable.close();
  }

  async function loadFromOpfs(path) {
    if (!navigator.storage || typeof navigator.storage.getDirectory !== "function") {
      fail("OPFS unavailable (navigator.storage.getDirectory missing)");
    }
    const parts = String(path)
      .trim()
      .split("/")
      .filter((p) => p.length > 0);
    if (parts.length === 0) fail("OPFS path must not be empty");
    if (parts.some((p) => p === "." || p === "..")) fail('OPFS path must not contain "." or ".."');

    const filename = parts.pop();
    if (!filename) fail("OPFS path must include filename");
    let dir = await navigator.storage.getDirectory();
    for (const part of parts) {
      dir = await dir.getDirectoryHandle(part, { create: false });
    }
    const handle = await dir.getFileHandle(filename, { create: false });
    const file = await handle.getFile();
    return new Uint8Array(await file.arrayBuffer());
  }

  function downloadBytes(filename, bytesLike) {
    const bytes =
      bytesLike instanceof Uint8Array ? bytesLike : new Uint8Array(bytesLike);
    const blob = new Blob([bytes], { type: "application/octet-stream" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = String(filename || "trace.aerogputrace");
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
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
    const textures = new Map(); // u32 -> { tex, fbo, w, h }

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

    let aerogpuBasicProgram = null;
    function getAerogpuBasicProgram() {
      if (aerogpuBasicProgram) return aerogpuBasicProgram;
      const vsSrc = `#version 300 es
precision highp float;
layout(location = 0) in vec2 a_position;
layout(location = 1) in vec4 a_color;
out vec4 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_position, 0.0, 1.0);
}
`;
      const fsSrc = `#version 300 es
precision highp float;
in vec4 v_color;
out vec4 o_color;
void main() {
  o_color = v_color;
}
`;
      aerogpuBasicProgram = linkProgram(vsSrc, fsSrc);
      return aerogpuBasicProgram;
    }

    function isAerogpuPacket(packetBytes) {
      return (
        packetBytes.byteLength >= 4 &&
        packetBytes[0] === AEROGPU_CMD_MAGIC[0] &&
        packetBytes[1] === AEROGPU_CMD_MAGIC[1] &&
        packetBytes[2] === AEROGPU_CMD_MAGIC[2] &&
        packetBytes[3] === AEROGPU_CMD_MAGIC[3]
      );
    }

    function executeAerogpuPacket(packetBytes, trace) {
      const pv = new DataView(packetBytes.buffer, packetBytes.byteOffset, packetBytes.byteLength);
      const sizeBytes = readU32(pv, 4);
      if (sizeBytes !== packetBytes.byteLength) fail("AGPC size_bytes mismatch");
      const opcode = readU16(pv, 8);

      // Common header is 24 bytes.
      const payloadOff = 24;
      function u32AtPayload(off) {
        return readU32(pv, payloadOff + off);
      }
      function u64AtPayload(off) {
        return readU64Big(pv, payloadOff + off);
      }
      function f32AtPayload(off) {
        return readF32(pv, payloadOff + off);
      }

      switch (opcode) {
        case AEROGPU_OPCODE_CREATE_BUFFER: {
          const bufferId = u32AtPayload(0);
          const size = u64BigToSafeNumber(u64AtPayload(8), "CreateBuffer.size_bytes");
          const glBuf = gl.createBuffer();
          if (!glBuf) fail("gl.createBuffer failed");
          buffers.set(bufferId, glBuf);
          gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
          gl.bufferData(gl.ARRAY_BUFFER, size, gl.DYNAMIC_DRAW);
          break;
        }
        case AEROGPU_OPCODE_DESTROY_BUFFER: {
          const bufferId = u32AtPayload(0);
          const glBuf = buffers.get(bufferId);
          if (glBuf) gl.deleteBuffer(glBuf);
          buffers.delete(bufferId);
          break;
        }
        case AEROGPU_OPCODE_WRITE_BUFFER: {
          const bufferId = u32AtPayload(0);
          const dstOffset = u64BigToSafeNumber(u64AtPayload(8), "WriteBuffer.dst_offset");
          const blobId = u64AtPayload(16);
          const size = u32AtPayload(24);
          const blob = trace.blobs.get(blobId);
          if (!blob) fail("missing blob_id=" + blobId.toString());
          if (blob.kind !== BLOB_BUFFER_DATA) fail("unexpected blob kind for WRITE_BUFFER");
          if (blob.bytes.byteLength !== size) fail("WRITE_BUFFER size_bytes mismatch");
          const glBuf = buffers.get(bufferId);
          if (!glBuf) fail("unknown buffer_id=" + bufferId);
          gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
          gl.bufferSubData(gl.ARRAY_BUFFER, dstOffset, blob.bytes);
          break;
        }
        case AEROGPU_OPCODE_CREATE_TEXTURE2D: {
          const textureId = u32AtPayload(0);
          const w = u32AtPayload(4);
          const h = u32AtPayload(8);
          const fmt = u32AtPayload(12);
          if (fmt !== AEROGPU_TEXFMT_RGBA8_UNORM) fail("unsupported texture format=" + fmt);
          const tex = gl.createTexture();
          if (!tex) fail("gl.createTexture failed");
          gl.bindTexture(gl.TEXTURE_2D, tex);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
          gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
          gl.texImage2D(
            gl.TEXTURE_2D,
            0,
            gl.RGBA8,
            w,
            h,
            0,
            gl.RGBA,
            gl.UNSIGNED_BYTE,
            null,
          );
          const fbo = gl.createFramebuffer();
          if (!fbo) fail("gl.createFramebuffer failed");
          gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
          gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
          const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
          if (status !== gl.FRAMEBUFFER_COMPLETE) {
            fail("framebuffer incomplete: 0x" + status.toString(16));
          }
          textures.set(textureId, { tex, fbo, w, h });
          break;
        }
        case AEROGPU_OPCODE_DESTROY_TEXTURE: {
          const textureId = u32AtPayload(0);
          const t = textures.get(textureId);
          if (t) {
            gl.deleteFramebuffer(t.fbo);
            gl.deleteTexture(t.tex);
          }
          textures.delete(textureId);
          break;
        }
        case AEROGPU_OPCODE_WRITE_TEXTURE2D: {
          const textureId = u32AtPayload(0);
          const mipLevel = u32AtPayload(4);
          if (mipLevel !== 0) fail("WRITE_TEXTURE2D only supports mip_level=0");
          const blobId = u64AtPayload(8);
          const bytesPerRow = u32AtPayload(16);
          const w = u32AtPayload(20);
          const h = u32AtPayload(24);
          const blob = trace.blobs.get(blobId);
          if (!blob) fail("missing blob_id=" + blobId.toString());
          if (blob.kind !== BLOB_TEXTURE_DATA) fail("unexpected blob kind for WRITE_TEXTURE2D");
          const expected = bytesPerRow * h;
          if (blob.bytes.byteLength !== expected) fail("WRITE_TEXTURE2D byte size mismatch");
          const t = textures.get(textureId);
          if (!t) fail("unknown texture_id=" + textureId);
          gl.bindTexture(gl.TEXTURE_2D, t.tex);
          if (bytesPerRow % 4 !== 0) fail("bytes_per_row must be multiple of 4 for RGBA8");
          gl.pixelStorei(gl.UNPACK_ROW_LENGTH, bytesPerRow / 4);
          gl.texSubImage2D(
            gl.TEXTURE_2D,
            0,
            0,
            0,
            w,
            h,
            gl.RGBA,
            gl.UNSIGNED_BYTE,
            blob.bytes,
          );
          gl.pixelStorei(gl.UNPACK_ROW_LENGTH, 0);
          break;
        }
        case AEROGPU_OPCODE_SET_RENDER_TARGET: {
          const textureId = u32AtPayload(0);
          if (textureId === 0) {
            gl.bindFramebuffer(gl.FRAMEBUFFER, null);
          } else {
            const t = textures.get(textureId);
            if (!t) fail("unknown texture_id=" + textureId);
            gl.bindFramebuffer(gl.FRAMEBUFFER, t.fbo);
          }
          break;
        }
        case AEROGPU_OPCODE_CLEAR: {
          const r = f32AtPayload(0);
          const g = f32AtPayload(4);
          const b = f32AtPayload(8);
          const a = f32AtPayload(12);
          gl.clearColor(r, g, b, a);
          gl.clear(gl.COLOR_BUFFER_BIT);
          break;
        }
        case AEROGPU_OPCODE_SET_VIEWPORT: {
          const x = f32AtPayload(0);
          const y = f32AtPayload(4);
          const w = f32AtPayload(8);
          const h = f32AtPayload(12);
          gl.viewport(x | 0, y | 0, w | 0, h | 0);
          break;
        }
        case AEROGPU_OPCODE_SET_PIPELINE: {
          const pipelineId = u32AtPayload(0);
          if (pipelineId !== AEROGPU_PIPELINE_BASIC_VERTEX_COLOR) {
            fail("unsupported pipeline_id=" + pipelineId);
          }
          const prog = getAerogpuBasicProgram();
          gl.useProgram(prog);
          currentProgram = prog;
          break;
        }
        case AEROGPU_OPCODE_SET_VERTEX_BUFFER: {
          if (!currentProgram) fail("SET_VERTEX_BUFFER without pipeline");
          const bufferId = u32AtPayload(0);
          const stride = u32AtPayload(4);
          const baseOffset = u64BigToSafeNumber(u64AtPayload(8), "SetVertexBuffer.offset");
          const glBuf = buffers.get(bufferId);
          if (!glBuf) fail("unknown buffer_id=" + bufferId);
          gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
          // BASIC_VERTEX_COLOR: pos @location(0) = vec2<f32>, color @location(1) = vec4<f32>
          gl.enableVertexAttribArray(0);
          gl.vertexAttribPointer(0, 2, gl.FLOAT, false, stride, baseOffset + 0);
          gl.enableVertexAttribArray(1);
          gl.vertexAttribPointer(1, 4, gl.FLOAT, false, stride, baseOffset + 8);
          break;
        }
        case AEROGPU_OPCODE_DRAW: {
          const vertexCount = u32AtPayload(0);
          const firstVertex = u32AtPayload(4);
          gl.drawArrays(gl.TRIANGLES, firstVertex, vertexCount);
          break;
        }
        case AEROGPU_OPCODE_PRESENT: {
          const textureId = u32AtPayload(0);
          const t = textures.get(textureId);
          if (!t) fail("unknown texture_id=" + textureId);
          gl.bindFramebuffer(gl.READ_FRAMEBUFFER, t.fbo);
          gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
          gl.blitFramebuffer(0, 0, t.w, t.h, 0, 0, canvas.width, canvas.height, gl.COLOR_BUFFER_BIT, gl.NEAREST);
          gl.bindFramebuffer(gl.FRAMEBUFFER, null);
          gl.finish();
          break;
        }
        default:
          fail("unknown AeroGPU opcode=0x" + opcode.toString(16));
      }
    }

    async function executePacket(packetBytes, trace) {
      if (isAerogpuPacket(packetBytes)) {
        executeAerogpuPacket(packetBytes, trace);
        return;
      }
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
    TraceWriter,
    recordTriangleTrace,
    saveToOpfs,
    loadFromOpfs,
    downloadBytes,
  };
})();
