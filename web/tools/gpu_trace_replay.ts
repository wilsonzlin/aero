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
//
// AeroGPU command stream source of truth:
//   drivers/aerogpu/protocol/aerogpu_cmd.h

(function () {
  const TRACE_MAGIC = asciiBytes("AEROGPUT");
  const TOC_MAGIC = asciiBytes("AEROTOC\0");
  const FOOTER_MAGIC = asciiBytes("AEROGPUF");

  const RECORD_BEGIN_FRAME = 0x01;
  const RECORD_PRESENT = 0x02;
  const RECORD_PACKET = 0x03;
  const RECORD_BLOB = 0x04;
  const RECORD_AEROGPU_SUBMISSION = 0x05;

  const BLOB_BUFFER_DATA = 0x01;
  const BLOB_TEXTURE_DATA = 0x02;
  const BLOB_SHADER_DXBC = 0x03;
  const BLOB_SHADER_WGSL = 0x04;
  const BLOB_SHADER_GLSL_ES300 = 0x05;
  const BLOB_AEROGPU_CMD_STREAM = 0x100;
  const BLOB_AEROGPU_ALLOC_TABLE = 0x101;
  const BLOB_AEROGPU_ALLOC_MEMORY = 0x102;

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

  // AeroGPU command stream ABI (canonical, A3A0).
  // Source of truth: drivers/aerogpu/protocol/aerogpu_cmd.h
  //
  // Command streams start with magic "ACMD" (little-endian) and contain a
  // sequence of size-prefixed packets (unknown opcodes must be skipped).
  //
  // NOTE: This file is also used as a plain `<script>` without bundling. To
  // keep it self-contained we embed fallback values here, but we opportunistically
  // dynamic-import the canonical protocol mirrors when running under the Aero
  // dev server (see `maybeInitAerogpuProtocol`).
  let AEROGPU_CMD_STREAM_MAGIC = asciiBytes("ACMD");
  let AEROGPU_CMD_STREAM_MAGIC_U32 = 0x444d4341; // "ACMD" LE
  let AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES = 24;
  let AEROGPU_CMD_HDR_SIZE_BYTES = 8;

  // Opcodes.
  let AEROGPU_CMD_CREATE_BUFFER = 0x0100;
  let AEROGPU_CMD_CREATE_TEXTURE2D = 0x0101;
  let AEROGPU_CMD_DESTROY_RESOURCE = 0x0102;
  let AEROGPU_CMD_SET_RENDER_TARGETS = 0x0400;
  let AEROGPU_CMD_SET_VIEWPORT = 0x0401;
  let AEROGPU_CMD_SET_VERTEX_BUFFERS = 0x0500;
  let AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY = 0x0502;
  let AEROGPU_CMD_CREATE_SAMPLER = 0x0520;
  let AEROGPU_CMD_DESTROY_SAMPLER = 0x0521;
  let AEROGPU_CMD_SET_SAMPLERS = 0x0522;
  let AEROGPU_CMD_SET_CONSTANT_BUFFERS = 0x0523;
  let AEROGPU_CMD_CLEAR = 0x0600;
  let AEROGPU_CMD_DRAW = 0x0601;
  let AEROGPU_CMD_PRESENT = 0x0700;
  let AEROGPU_CMD_PRESENT_EX = 0x0701;
  let AEROGPU_CMD_EXPORT_SHARED_SURFACE = 0x0710;
  let AEROGPU_CMD_IMPORT_SHARED_SURFACE = 0x0711;
  let AEROGPU_CMD_RELEASE_SHARED_SURFACE = 0x0712;

  // Packet sizes (minimum size_bytes).
  let AEROGPU_CMD_CREATE_BUFFER_SIZE_BYTES = 40;
  let AEROGPU_CMD_CREATE_TEXTURE2D_SIZE_BYTES = 56;
  let AEROGPU_CMD_DESTROY_RESOURCE_SIZE_BYTES = 16;
  let AEROGPU_CMD_SET_RENDER_TARGETS_SIZE_BYTES = 48;
  let AEROGPU_CMD_SET_VIEWPORT_SIZE_BYTES = 32;
  let AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE_BYTES = 16;
  let AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE_BYTES = 16;
  let AEROGPU_CMD_CREATE_SAMPLER_SIZE_BYTES = 28;
  let AEROGPU_CMD_DESTROY_SAMPLER_SIZE_BYTES = 16;
  let AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES = 24;
  let AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES = 24;
  let AEROGPU_CONSTANT_BUFFER_BINDING_SIZE_BYTES = 16;
  let AEROGPU_CMD_CLEAR_SIZE_BYTES = 36;
  let AEROGPU_CMD_DRAW_SIZE_BYTES = 24;
  let AEROGPU_CMD_PRESENT_SIZE_BYTES = 16;
  let AEROGPU_CMD_PRESENT_EX_SIZE_BYTES = 24;
  let AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE_BYTES = 24;
  let AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE_BYTES = 24;
  let AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE_BYTES = 24;

  let AEROGPU_CLEAR_COLOR = 1 << 0;
  let AEROGPU_CLEAR_DEPTH = 1 << 1;
  let AEROGPU_CLEAR_STENCIL = 1 << 2;

  let AEROGPU_FORMAT_R8G8B8A8_UNORM = 3;
  // ABI 1.2+ adds explicit sRGB variants. The replay tool treats sRGB as UNORM (no conversion)
  // because WebGL2 presentation is not color-managed here.
  let AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB = 9;
  let AEROGPU_TOPOLOGY_TRIANGLELIST = 4;

  let decodeCmdStreamHeader = null;
  let decodeCmdHdr = null;
  let AerogpuCmdStreamIter = null;

  let aerogpuProtocolInit = null;
  async function maybeInitAerogpuProtocol() {
    if (aerogpuProtocolInit) return aerogpuProtocolInit;
    aerogpuProtocolInit = (async () => {
      try {
        const cmd = await import("/emulator/protocol/aerogpu/aerogpu_cmd.ts");
        const pci = await import("/emulator/protocol/aerogpu/aerogpu_pci.ts");
        if (!cmd) return;

        if (typeof cmd.decodeCmdStreamHeader === "function") decodeCmdStreamHeader = cmd.decodeCmdStreamHeader;
        if (typeof cmd.decodeCmdHdr === "function") decodeCmdHdr = cmd.decodeCmdHdr;
        if (typeof cmd.AerogpuCmdStreamIter === "function") AerogpuCmdStreamIter = cmd.AerogpuCmdStreamIter;

        if (typeof cmd.AEROGPU_CMD_STREAM_MAGIC === "number") {
          AEROGPU_CMD_STREAM_MAGIC_U32 = cmd.AEROGPU_CMD_STREAM_MAGIC >>> 0;
          const b = new Uint8Array(4);
          new DataView(b.buffer).setUint32(0, AEROGPU_CMD_STREAM_MAGIC_U32, true);
          AEROGPU_CMD_STREAM_MAGIC = b;
        }
        if (typeof cmd.AEROGPU_CMD_STREAM_HEADER_SIZE === "number") {
          AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES = cmd.AEROGPU_CMD_STREAM_HEADER_SIZE >>> 0;
        }
        if (typeof cmd.AEROGPU_CMD_HDR_SIZE === "number") AEROGPU_CMD_HDR_SIZE_BYTES = cmd.AEROGPU_CMD_HDR_SIZE >>> 0;

        if (cmd.AerogpuCmdOpcode) {
          if (typeof cmd.AerogpuCmdOpcode.CreateBuffer === "number") AEROGPU_CMD_CREATE_BUFFER = cmd.AerogpuCmdOpcode.CreateBuffer >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.CreateTexture2d === "number") AEROGPU_CMD_CREATE_TEXTURE2D = cmd.AerogpuCmdOpcode.CreateTexture2d >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.DestroyResource === "number") AEROGPU_CMD_DESTROY_RESOURCE = cmd.AerogpuCmdOpcode.DestroyResource >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetRenderTargets === "number") AEROGPU_CMD_SET_RENDER_TARGETS = cmd.AerogpuCmdOpcode.SetRenderTargets >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetViewport === "number") AEROGPU_CMD_SET_VIEWPORT = cmd.AerogpuCmdOpcode.SetViewport >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetVertexBuffers === "number") AEROGPU_CMD_SET_VERTEX_BUFFERS = cmd.AerogpuCmdOpcode.SetVertexBuffers >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetPrimitiveTopology === "number") AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY = cmd.AerogpuCmdOpcode.SetPrimitiveTopology >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.CreateSampler === "number") AEROGPU_CMD_CREATE_SAMPLER = cmd.AerogpuCmdOpcode.CreateSampler >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.DestroySampler === "number") AEROGPU_CMD_DESTROY_SAMPLER = cmd.AerogpuCmdOpcode.DestroySampler >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetSamplers === "number") AEROGPU_CMD_SET_SAMPLERS = cmd.AerogpuCmdOpcode.SetSamplers >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.SetConstantBuffers === "number") AEROGPU_CMD_SET_CONSTANT_BUFFERS = cmd.AerogpuCmdOpcode.SetConstantBuffers >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.Clear === "number") AEROGPU_CMD_CLEAR = cmd.AerogpuCmdOpcode.Clear >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.Draw === "number") AEROGPU_CMD_DRAW = cmd.AerogpuCmdOpcode.Draw >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.Present === "number") AEROGPU_CMD_PRESENT = cmd.AerogpuCmdOpcode.Present >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.PresentEx === "number") AEROGPU_CMD_PRESENT_EX = cmd.AerogpuCmdOpcode.PresentEx >>> 0;
          if (typeof cmd.AerogpuCmdOpcode.ExportSharedSurface === "number") {
            AEROGPU_CMD_EXPORT_SHARED_SURFACE = cmd.AerogpuCmdOpcode.ExportSharedSurface >>> 0;
          }
          if (typeof cmd.AerogpuCmdOpcode.ImportSharedSurface === "number") {
            AEROGPU_CMD_IMPORT_SHARED_SURFACE = cmd.AerogpuCmdOpcode.ImportSharedSurface >>> 0;
          }
          if (typeof cmd.AerogpuCmdOpcode.ReleaseSharedSurface === "number") {
            AEROGPU_CMD_RELEASE_SHARED_SURFACE = cmd.AerogpuCmdOpcode.ReleaseSharedSurface >>> 0;
          }
        }

        if (typeof cmd.AEROGPU_CMD_CREATE_BUFFER_SIZE === "number") AEROGPU_CMD_CREATE_BUFFER_SIZE_BYTES = cmd.AEROGPU_CMD_CREATE_BUFFER_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_CREATE_TEXTURE2D_SIZE === "number") AEROGPU_CMD_CREATE_TEXTURE2D_SIZE_BYTES = cmd.AEROGPU_CMD_CREATE_TEXTURE2D_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_DESTROY_RESOURCE_SIZE === "number") {
          AEROGPU_CMD_DESTROY_RESOURCE_SIZE_BYTES = cmd.AEROGPU_CMD_DESTROY_RESOURCE_SIZE >>> 0;
        }
        if (typeof cmd.AEROGPU_CMD_SET_RENDER_TARGETS_SIZE === "number") AEROGPU_CMD_SET_RENDER_TARGETS_SIZE_BYTES = cmd.AEROGPU_CMD_SET_RENDER_TARGETS_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_SET_VIEWPORT_SIZE === "number") AEROGPU_CMD_SET_VIEWPORT_SIZE_BYTES = cmd.AEROGPU_CMD_SET_VIEWPORT_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE === "number") AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE_BYTES = cmd.AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE === "number") AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE_BYTES = cmd.AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_CREATE_SAMPLER_SIZE === "number") AEROGPU_CMD_CREATE_SAMPLER_SIZE_BYTES = cmd.AEROGPU_CMD_CREATE_SAMPLER_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_DESTROY_SAMPLER_SIZE === "number") AEROGPU_CMD_DESTROY_SAMPLER_SIZE_BYTES = cmd.AEROGPU_CMD_DESTROY_SAMPLER_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_SET_SAMPLERS_SIZE === "number") AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES = cmd.AEROGPU_CMD_SET_SAMPLERS_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE === "number") AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES = cmd.AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CONSTANT_BUFFER_BINDING_SIZE === "number") AEROGPU_CONSTANT_BUFFER_BINDING_SIZE_BYTES = cmd.AEROGPU_CONSTANT_BUFFER_BINDING_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_CLEAR_SIZE === "number") AEROGPU_CMD_CLEAR_SIZE_BYTES = cmd.AEROGPU_CMD_CLEAR_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_DRAW_SIZE === "number") AEROGPU_CMD_DRAW_SIZE_BYTES = cmd.AEROGPU_CMD_DRAW_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_PRESENT_SIZE === "number") AEROGPU_CMD_PRESENT_SIZE_BYTES = cmd.AEROGPU_CMD_PRESENT_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_PRESENT_EX_SIZE === "number") AEROGPU_CMD_PRESENT_EX_SIZE_BYTES = cmd.AEROGPU_CMD_PRESENT_EX_SIZE >>> 0;
        if (typeof cmd.AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE === "number") {
          AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE_BYTES = cmd.AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE >>> 0;
        }
        if (typeof cmd.AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE === "number") {
          AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE_BYTES = cmd.AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE >>> 0;
        }
        if (typeof cmd.AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE === "number") {
          AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE_BYTES = cmd.AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE >>> 0;
        }

        if (typeof cmd.AEROGPU_CLEAR_COLOR === "number") AEROGPU_CLEAR_COLOR = cmd.AEROGPU_CLEAR_COLOR >>> 0;
        if (typeof cmd.AEROGPU_CLEAR_DEPTH === "number") AEROGPU_CLEAR_DEPTH = cmd.AEROGPU_CLEAR_DEPTH >>> 0;
        if (typeof cmd.AEROGPU_CLEAR_STENCIL === "number") AEROGPU_CLEAR_STENCIL = cmd.AEROGPU_CLEAR_STENCIL >>> 0;

        if (cmd.AerogpuPrimitiveTopology && typeof cmd.AerogpuPrimitiveTopology.TriangleList === "number") {
          AEROGPU_TOPOLOGY_TRIANGLELIST = cmd.AerogpuPrimitiveTopology.TriangleList >>> 0;
        }
        if (pci && pci.AerogpuFormat && typeof pci.AerogpuFormat.R8G8B8A8Unorm === "number") {
          AEROGPU_FORMAT_R8G8B8A8_UNORM = pci.AerogpuFormat.R8G8B8A8Unorm >>> 0;
        }
        if (pci && pci.AerogpuFormat && typeof pci.AerogpuFormat.R8G8B8A8UnormSrgb === "number") {
          AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB = pci.AerogpuFormat.R8G8B8A8UnormSrgb >>> 0;
        }
      } catch (e) {
        // Ignore; this tool can run standalone without the protocol mirrors.
      }
    })();
    return aerogpuProtocolInit;
  }

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
    if (containerVersion !== 1 && containerVersion !== 2) {
      fail("unsupported container_version=" + containerVersion);
    }
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

    // Scan record stream once to collect blobs and per-frame actions.
    const blobs = new Map(); // bigint -> {kind, bytes}
    const frameActions = new Map(); // frameIndex -> {kind,...}[]
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
        if (payloadLen !== 4) fail("BEGIN_FRAME payload out of bounds");
        const frameIndex = readU32(view, payloadOff);
        currentFrame = frameIndex;
        if (!frameActions.has(frameIndex)) frameActions.set(frameIndex, []);
      } else if (rType === RECORD_PRESENT) {
        if (payloadLen !== 4) fail("PRESENT payload out of bounds");
        currentFrame = null;
      } else if (rType === RECORD_PACKET) {
        if (currentFrame === null) fail("packet outside of a frame");
        const pkt = bytes.subarray(payloadOff, payloadEnd);
        frameActions.get(currentFrame).push({ kind: "packet", bytes: pkt });
      } else if (rType === RECORD_AEROGPU_SUBMISSION) {
        if (containerVersion < 2) fail("AerogpuSubmission requires container_version >= 2");
        if (currentFrame === null) fail("AerogpuSubmission outside of a frame");
        if (payloadLen < 56) fail("AerogpuSubmission payload out of bounds");

        const recordVersion = readU32(view, payloadOff + 0);
        const submissionHeaderSize = readU32(view, payloadOff + 4);
        if (submissionHeaderSize < 56 || submissionHeaderSize > payloadLen) {
          fail("AerogpuSubmission header out of bounds");
        }

        const submitFlags = readU32(view, payloadOff + 8);
        const contextId = readU32(view, payloadOff + 12);
        const engineId = readU32(view, payloadOff + 16);
        // reserved0 at +20
        const signalFence = readU64Big(view, payloadOff + 24);
        const cmdStreamBlobId = readU64Big(view, payloadOff + 32);
        const allocTableBlobId = readU64Big(view, payloadOff + 40);
        const memoryRangeCount = readU32(view, payloadOff + 48);
        // reserved1 at +52

        const requiredLen = submissionHeaderSize + memoryRangeCount * 32;
        if (requiredLen > payloadLen) fail("AerogpuSubmission memory ranges out of bounds");

        const memoryRanges = [];
        let mOff = payloadOff + submissionHeaderSize;
        for (let i = 0; i < memoryRangeCount; i++) {
          const allocId = readU32(view, mOff + 0);
          const rangeFlags = readU32(view, mOff + 4);
          const gpa = readU64Big(view, mOff + 8);
          const sizeBytes = readU64Big(view, mOff + 16);
          const blobId = readU64Big(view, mOff + 24);
          memoryRanges.push({
            alloc_id: allocId,
            flags: rangeFlags,
            gpa,
            size_bytes: sizeBytes,
            blob_id: blobId,
          });
          mOff += 32;
        }

        frameActions.get(currentFrame).push({
          kind: "aerogpuSubmission",
          record_version: recordVersion,
          submit_flags: submitFlags,
          context_id: contextId,
          engine_id: engineId,
          signal_fence: signalFence,
          cmd_stream_blob_id: cmdStreamBlobId,
          alloc_table_blob_id: allocTableBlobId,
          memory_ranges: memoryRanges,
        });
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
      frameActions,
    };
  }

  function createWebgl2Backend(canvas) {
    // Important: request a non-MSAA default framebuffer.
    //
    // We use `blitFramebuffer` when replaying AeroGPU submission traces that
    // render into an offscreen texture and then present to the default
    // framebuffer. In WebGL2, blitting into a multisampled default framebuffer
    // (antialias=true) can fail with `INVALID_OPERATION` on some drivers (notably
    // Chromium headless / SwiftShader), which would make `readPixels()` return
    // all-zero results and break determinism tests.
    const gl = canvas.getContext("webgl2", { preserveDrawingBuffer: true, antialias: false });
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

    // A3A0 (AeroGPU command stream) replay state.
    const acmdBuffers = new Map(); // u32 handle -> WebGLBuffer
    const acmdTextures = new Map(); // u32 handle -> { texture, framebuffer, width, height, format }
    // Shared surface bookkeeping (EXPORT/IMPORT_SHARED_SURFACE).
    // Mirrors the host-side shared-surface protocol:
    // - EXPORT: share_token -> underlying handle
    // - IMPORT: alias_handle -> underlying handle
    // - DESTROY_RESOURCE refcounts alias/original handles and destroys the underlying resource on the final ref.
    const acmdSharedSurfaces = new Map(); // u64 share_token (BigInt) -> underlying handle (u32)
    const acmdRetiredSharedSurfaceTokens = new Set(); // BigInt share_token values that were released/retired
    const acmdSharedHandles = new Map(); // u32 handle -> underlying handle (u32)
    const acmdSharedRefcounts = new Map(); // u32 underlying handle -> refcount (number)

    function resolveSharedHandle(handle) {
      return acmdSharedHandles.get(handle) ?? handle;
    }

    function registerSharedHandle(handle) {
      if (handle === 0) return;
      const existing = acmdSharedHandles.get(handle);
      if (existing !== undefined) {
        if (existing !== handle) fail("ACMD shared surface handle " + handle + " is already an alias (underlying=" + existing + ")");
        return;
      }
      acmdSharedHandles.set(handle, handle);
      acmdSharedRefcounts.set(handle, (acmdSharedRefcounts.get(handle) || 0) + 1);
    }

    function retireTokensForUnderlying(underlying) {
      const toRetire = [];
      for (const [token, h] of acmdSharedSurfaces) {
        if (h === underlying) toRetire.push(token);
      }
      for (const token of toRetire) {
        acmdSharedSurfaces.delete(token);
        acmdRetiredSharedSurfaceTokens.add(token);
      }
    }

    function exportSharedSurface(resourceHandle, shareToken) {
      if (resourceHandle === 0) fail("ACMD EXPORT_SHARED_SURFACE invalid resource_handle 0");
      if (shareToken === 0n) fail("ACMD EXPORT_SHARED_SURFACE invalid share_token 0");
      if (acmdRetiredSharedSurfaceTokens.has(shareToken)) {
        fail("ACMD EXPORT_SHARED_SURFACE share_token 0x" + shareToken.toString(16) + " was previously released");
      }

      const underlying = acmdSharedHandles.get(resourceHandle);
      if (underlying === undefined) fail("ACMD EXPORT_SHARED_SURFACE unknown resource handle " + resourceHandle);

      const existing = acmdSharedSurfaces.get(shareToken);
      if (existing !== undefined) {
        if (existing !== underlying) {
          fail(
            "ACMD EXPORT_SHARED_SURFACE share_token 0x" +
              shareToken.toString(16) +
              " already exported (existing=" +
              existing +
              " new=" +
              underlying +
              ")",
          );
        }
        return;
      }

      acmdSharedSurfaces.set(shareToken, underlying);
    }

    function importSharedSurface(outHandle, shareToken) {
      if (outHandle === 0) fail("ACMD IMPORT_SHARED_SURFACE invalid out_resource_handle 0");
      if (shareToken === 0n) fail("ACMD IMPORT_SHARED_SURFACE invalid share_token 0");

      const underlying = acmdSharedSurfaces.get(shareToken);
      if (underlying === undefined) {
        fail("ACMD IMPORT_SHARED_SURFACE unknown share_token 0x" + shareToken.toString(16) + " (not exported)");
      }
      if (!acmdSharedRefcounts.has(underlying)) {
        fail(
          "ACMD IMPORT_SHARED_SURFACE share_token 0x" +
            shareToken.toString(16) +
            " refers to destroyed handle " +
            underlying,
        );
      }

      const existing = acmdSharedHandles.get(outHandle);
      if (existing !== undefined) {
        if (existing !== underlying) {
          fail(
            "ACMD IMPORT_SHARED_SURFACE out_resource_handle " +
              outHandle +
              " already bound (existing=" +
              existing +
              " new=" +
              underlying +
              ")",
          );
        }
        return;
      }

      // Do not allow aliasing a handle that is already bound to a real resource.
      if (acmdTextures.has(outHandle) || acmdBuffers.has(outHandle)) {
        fail("ACMD IMPORT_SHARED_SURFACE out_resource_handle " + outHandle + " collides with an existing resource");
      }

      acmdSharedHandles.set(outHandle, underlying);
      acmdSharedRefcounts.set(underlying, (acmdSharedRefcounts.get(underlying) || 0) + 1);
    }

    function releaseSharedSurface(shareToken) {
      if (shareToken === 0n) return;
      // Idempotent: unknown tokens are a no-op (see `aerogpu_cmd.h` contract).
      if (acmdSharedSurfaces.delete(shareToken)) {
        acmdRetiredSharedSurfaceTokens.add(shareToken);
      }
    }

    function destroySharedHandle(handle) {
      if (handle === 0) return null;
      const underlying = acmdSharedHandles.get(handle);
      if (underlying === undefined) return null;
      acmdSharedHandles.delete(handle);

      const count = acmdSharedRefcounts.get(underlying);
      if (count === undefined) {
        retireTokensForUnderlying(underlying);
        return { underlying, lastRef: true };
      }

      const next = Math.max(0, count - 1);
      if (next !== 0) {
        acmdSharedRefcounts.set(underlying, next);
        return { underlying, lastRef: false };
      }

      acmdSharedRefcounts.delete(underlying);
      retireTokensForUnderlying(underlying);
      return { underlying, lastRef: true };
    }

    let acmdFramebuffer = null; // currently bound draw framebuffer (WebGLFramebuffer | null)
    let acmdColor0 = null; // { framebuffer, width, height } | null
    let acmdPrimitiveMode = gl.TRIANGLES;

    const ACMD_GLSL_VS = `#version 300 es
precision highp float;
layout(location=0) in vec2 a_position;
layout(location=1) in vec4 a_color;
out vec4 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_position, 0.0, 1.0);
}
`;
    const ACMD_GLSL_FS = `#version 300 es
precision highp float;
in vec4 v_color;
out vec4 o_color;
void main() {
  o_color = v_color;
}
`;

    const acmdProgram = linkProgram(ACMD_GLSL_VS, ACMD_GLSL_FS);
    const acmdVao = gl.createVertexArray();
    if (!acmdVao) fail("gl.createVertexArray failed");

    function isAerogpuCmdStreamPacket(packetBytes) {
      return (
        packetBytes.byteLength >= 4 &&
        packetBytes[0] === AEROGPU_CMD_STREAM_MAGIC[0] &&
        packetBytes[1] === AEROGPU_CMD_STREAM_MAGIC[1] &&
        packetBytes[2] === AEROGPU_CMD_STREAM_MAGIC[2] &&
        packetBytes[3] === AEROGPU_CMD_STREAM_MAGIC[3]
      );
    }

    function executeAerogpuCmdStream(packetBytes, execCtx) {
      // `aerogpu_cmd_stream_header` followed by size-prefixed `aerogpu_cmd_hdr` packets.
      // Forward-compat rules: validate `size_bytes`, skip unknown opcodes.
      if (packetBytes.byteLength < AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES) fail("ACMD stream header out of bounds");
      const pv = new DataView(packetBytes.buffer, packetBytes.byteOffset, packetBytes.byteLength);

      let sizeBytes = 0;
      if (decodeCmdStreamHeader) {
        const hdr = decodeCmdStreamHeader(pv, 0);
        sizeBytes = hdr.sizeBytes >>> 0;
      } else {
        const magic = readU32(pv, 0);
        if (magic !== AEROGPU_CMD_STREAM_MAGIC_U32) fail("bad ACMD magic");

        const abiVersion = readU32(pv, 4);
        const major = (abiVersion >>> 16) & 0xffff;
        if (major !== 1) fail("unsupported ACMD ABI major: " + major);

        sizeBytes = readU32(pv, 8);
      }
      if (sizeBytes < AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES) fail("ACMD size_bytes too small: " + sizeBytes);
      if (sizeBytes > packetBytes.byteLength) fail("ACMD size_bytes out of bounds: " + sizeBytes);

      // Ignore flags and reserved fields for forward compatibility.
      const streamEnd = sizeBytes;
      let off = AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES;

      const allocMemory = execCtx && execCtx.allocMemory;

      function clampI32(v) {
        if (!Number.isFinite(v)) return 0;
        let n = Math.round(v);
        if (n < -2147483648) n = -2147483648;
        if (n > 2147483647) n = 2147483647;
        return n | 0;
      }

      function clampU31(v) {
        if (!Number.isFinite(v)) return 0;
        let n = Math.round(v);
        if (n < 0) n = 0;
        if (n > 2147483647) n = 2147483647;
        return n | 0;
      }

      function getGlPrimitiveMode(topology) {
        switch (topology >>> 0) {
          case 1:
            return gl.POINTS;
          case 2:
            return gl.LINES;
          case 3:
            return gl.LINE_STRIP;
          case AEROGPU_TOPOLOGY_TRIANGLELIST:
            return gl.TRIANGLES;
          case 5:
            return gl.TRIANGLE_STRIP;
          case 6:
            return gl.TRIANGLE_FAN;
          default:
            fail("ACMD unsupported primitive topology=" + topology);
        }
      }

      while (off < streamEnd) {
        if (off + AEROGPU_CMD_HDR_SIZE_BYTES > streamEnd) fail("ACMD command header out of bounds");
        let opcode = 0;
        let cmdSize = 0;
        if (decodeCmdHdr) {
          const hdr = decodeCmdHdr(pv, off);
          opcode = hdr.opcode >>> 0;
          cmdSize = hdr.sizeBytes >>> 0;
        } else {
          opcode = readU32(pv, off + 0);
          cmdSize = readU32(pv, off + 4);
        }
        if (cmdSize < AEROGPU_CMD_HDR_SIZE_BYTES) fail("ACMD cmd size_bytes too small: " + cmdSize);
        if ((cmdSize & 3) !== 0) fail("ACMD cmd size_bytes not 4-byte aligned: " + cmdSize);
        if (off + cmdSize > streamEnd) fail("ACMD cmd overruns stream");

        switch (opcode) {
          case AEROGPU_CMD_CREATE_BUFFER: {
            // struct aerogpu_cmd_create_buffer (40 bytes)
            if (cmdSize < 40) fail("ACMD CREATE_BUFFER size_bytes too small: " + cmdSize);
            const bufferHandle = readU32(pv, off + 8);
            const sizeBytes = readU64Big(pv, off + 16);
            const backingAllocId = readU32(pv, off + 24);
            const backingOffsetBytes = readU32(pv, off + 28);

            if (bufferHandle === 0) fail("ACMD CREATE_BUFFER invalid handle 0");
            const shared = acmdSharedHandles.get(bufferHandle);
            if (shared !== undefined && shared !== bufferHandle) {
              fail("ACMD CREATE_BUFFER handle " + bufferHandle + " is already an alias (underlying=" + shared + ")");
            }

            const glBuf = gl.createBuffer();
            if (!glBuf) fail("gl.createBuffer failed");
            acmdBuffers.set(bufferHandle, glBuf);

            gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
            if (backingAllocId !== 0) {
              if (!allocMemory) fail("ACMD CREATE_BUFFER missing alloc memory map");
              const alloc = allocMemory.get(backingAllocId);
              if (!alloc) fail("ACMD CREATE_BUFFER missing alloc_id=" + backingAllocId);
              const size = u64BigToSafeNumber(sizeBytes, "ACMD CREATE_BUFFER size_bytes");
              const end = backingOffsetBytes + size;
              if (end > alloc.bytes.byteLength) fail("ACMD CREATE_BUFFER backing range out of bounds");
              gl.bufferData(gl.ARRAY_BUFFER, alloc.bytes.subarray(backingOffsetBytes, end), gl.STATIC_DRAW);
            } else {
              const size = u64BigToSafeNumber(sizeBytes, "ACMD CREATE_BUFFER size_bytes");
              gl.bufferData(gl.ARRAY_BUFFER, size, gl.STATIC_DRAW);
            }
            registerSharedHandle(bufferHandle);
            break;
          }
          case AEROGPU_CMD_CREATE_TEXTURE2D: {
            // struct aerogpu_cmd_create_texture2d (56 bytes)
            if (cmdSize < 56) fail("ACMD CREATE_TEXTURE2D size_bytes too small: " + cmdSize);
            const textureHandle = readU32(pv, off + 8);
            const format = readU32(pv, off + 16);
            const width = readU32(pv, off + 20);
            const height = readU32(pv, off + 24);
            const mipLevels = readU32(pv, off + 28);
            const arrayLayers = readU32(pv, off + 32);
            const rowPitchBytes = readU32(pv, off + 36);
            const backingAllocId = readU32(pv, off + 40);
            const backingOffsetBytes = readU32(pv, off + 44);

            if (textureHandle === 0) fail("ACMD CREATE_TEXTURE2D invalid handle 0");
            const shared = acmdSharedHandles.get(textureHandle);
            if (shared !== undefined && shared !== textureHandle) {
              fail("ACMD CREATE_TEXTURE2D handle " + textureHandle + " is already an alias (underlying=" + shared + ")");
            }

            if (mipLevels !== 1) fail("ACMD CREATE_TEXTURE2D mip_levels not supported: " + mipLevels);
            if (arrayLayers !== 1) fail("ACMD CREATE_TEXTURE2D array_layers not supported: " + arrayLayers);

            let glInternalFormat = 0;
            let glFormat = 0;
            let glType = 0;
            if (format === AEROGPU_FORMAT_R8G8B8A8_UNORM || format === AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB) {
              glInternalFormat = gl.RGBA8;
              glFormat = gl.RGBA;
              glType = gl.UNSIGNED_BYTE;
            } else {
              const bcHint = format >= 64 && format <= 71 ? " (BC formats require GPU backend)" : "";
              fail("ACMD CREATE_TEXTURE2D unsupported format=" + format + bcHint);
            }

            const tex = gl.createTexture();
            if (!tex) fail("gl.createTexture failed");
            gl.bindTexture(gl.TEXTURE_2D, tex);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
            gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
            gl.texImage2D(gl.TEXTURE_2D, 0, glInternalFormat, width, height, 0, glFormat, glType, null);

            if (backingAllocId !== 0) {
              if (!allocMemory) fail("ACMD CREATE_TEXTURE2D missing alloc memory map");
              const alloc = allocMemory.get(backingAllocId);
              if (!alloc) fail("ACMD CREATE_TEXTURE2D missing alloc_id=" + backingAllocId);

              const rowBytes = width * 4;
              const pitch = rowPitchBytes !== 0 ? rowPitchBytes : rowBytes;
              if (pitch < rowBytes) {
                fail("ACMD CREATE_TEXTURE2D row_pitch_bytes too small: " + pitch + " < " + rowBytes);
              }

              const requiredBytes = backingOffsetBytes + pitch * height;
              if (requiredBytes > alloc.bytes.byteLength) {
                fail("ACMD CREATE_TEXTURE2D backing range out of bounds");
              }

              const packed = new Uint8Array(rowBytes * height);
              for (let y = 0; y < height; y++) {
                const srcOff = backingOffsetBytes + y * pitch;
                packed.set(alloc.bytes.subarray(srcOff, srcOff + rowBytes), y * rowBytes);
              }
              gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, glFormat, glType, packed);
            }

            const fb = gl.createFramebuffer();
            if (!fb) fail("gl.createFramebuffer failed");
            const prevFb = acmdFramebuffer;
            gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
            gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
            gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
            const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
            if (status !== gl.FRAMEBUFFER_COMPLETE) {
              fail("ACMD framebuffer incomplete: 0x" + status.toString(16));
            }
            gl.bindFramebuffer(gl.FRAMEBUFFER, prevFb);

            acmdTextures.set(textureHandle, { texture: tex, framebuffer: fb, width, height, format });
            registerSharedHandle(textureHandle);
            break;
          }
          case AEROGPU_CMD_DESTROY_RESOURCE: {
            // struct aerogpu_cmd_destroy_resource (16 bytes)
            if (cmdSize < AEROGPU_CMD_DESTROY_RESOURCE_SIZE_BYTES) fail("ACMD DESTROY_RESOURCE size_bytes too small: " + cmdSize);
            const handle = readU32(pv, off + 8);

            const shared = destroySharedHandle(handle);
            const underlying = shared ? shared.underlying : handle;
            const lastRef = shared ? shared.lastRef : true;
            if (!lastRef) break;

            const texObj = acmdTextures.get(underlying);
            if (texObj) {
              if (acmdFramebuffer === texObj.framebuffer) {
                gl.bindFramebuffer(gl.FRAMEBUFFER, null);
                acmdFramebuffer = null;
              }
              if (acmdColor0 && acmdColor0.framebuffer === texObj.framebuffer) acmdColor0 = null;
              gl.deleteFramebuffer(texObj.framebuffer);
              gl.deleteTexture(texObj.texture);
              acmdTextures.delete(underlying);
            }
            const bufObj = acmdBuffers.get(underlying);
            if (bufObj) {
              gl.deleteBuffer(bufObj);
              acmdBuffers.delete(underlying);
            }
            break;
          }
          case AEROGPU_CMD_SET_RENDER_TARGETS: {
            // struct aerogpu_cmd_set_render_targets (48 bytes)
            if (cmdSize < 48) fail("ACMD SET_RENDER_TARGETS size_bytes too small: " + cmdSize);
            const colorCount = readU32(pv, off + 8);
            if (colorCount > 8) fail("ACMD SET_RENDER_TARGETS color_count out of bounds: " + colorCount);
            const color0Raw = colorCount > 0 ? readU32(pv, off + 16) : 0;
            const color0 = color0Raw !== 0 ? resolveSharedHandle(color0Raw) : 0;

            let fb = null;
            acmdColor0 = null;
            if (color0 !== 0) {
              const texObj = acmdTextures.get(color0);
              if (!texObj) fail("ACMD SET_RENDER_TARGETS unknown texture_handle=" + color0Raw + " (resolved=" + color0 + ")");
              fb = texObj.framebuffer;
              acmdColor0 = { framebuffer: texObj.framebuffer, width: texObj.width, height: texObj.height };
            }

            gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
            acmdFramebuffer = fb;
            break;
          }
          case AEROGPU_CMD_SET_VIEWPORT: {
            // struct aerogpu_cmd_set_viewport
            if (cmdSize < AEROGPU_CMD_SET_VIEWPORT_SIZE_BYTES) fail("ACMD SET_VIEWPORT size_bytes too small: " + cmdSize);
            const x = readF32(pv, off + 8);
            const y = readF32(pv, off + 12);
            const wf = readF32(pv, off + 16);
            const hf = readF32(pv, off + 20);

            // Treat a 0/0 viewport as "use canvas size" (like the minimal ABI).
            let w = wf;
            let h = hf;
            if (w === 0 && h === 0) {
              w = canvas.width;
              h = canvas.height;
            }

            gl.viewport(clampI32(x), clampI32(y), clampU31(w), clampU31(h));
            break;
          }
          case AEROGPU_CMD_SET_VERTEX_BUFFERS: {
            // struct aerogpu_cmd_set_vertex_buffers (16 bytes) + bindings
            if (cmdSize < 16) fail("ACMD SET_VERTEX_BUFFERS size_bytes too small: " + cmdSize);
            const startSlot = readU32(pv, off + 8);
            const bufferCount = readU32(pv, off + 12);
            const requiredLen = 16 + bufferCount * 16;
            if (cmdSize < requiredLen) fail("ACMD SET_VERTEX_BUFFERS bindings out of bounds");

            for (let i = 0; i < bufferCount; i++) {
              const slot = startSlot + i;
              const bOff = off + 16 + i * 16;
              const bufferHandle = readU32(pv, bOff + 0);
              const strideBytes = readU32(pv, bOff + 4);
              const offsetBytes = readU32(pv, bOff + 8);

              if (slot === 0) {
                if (strideBytes < 24) fail("ACMD vertex stride too small: " + strideBytes);
                const resolvedBufferHandle = resolveSharedHandle(bufferHandle);
                const glBuf = acmdBuffers.get(resolvedBufferHandle);
                if (!glBuf) fail("ACMD unknown buffer_handle=" + bufferHandle + " (resolved=" + resolvedBufferHandle + ")");
                gl.bindVertexArray(acmdVao);
                gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
                // Vertex format for the triangle fixture:
                // - position: vec2<f32> at offset 0
                // - color: vec4<f32> at offset 8
                gl.enableVertexAttribArray(0);
                gl.vertexAttribPointer(0, 2, gl.FLOAT, false, strideBytes, offsetBytes + 0);
                gl.enableVertexAttribArray(1);
                gl.vertexAttribPointer(1, 4, gl.FLOAT, false, strideBytes, offsetBytes + 8);
                gl.bindVertexArray(null);
              }
            }
            break;
          }
          case AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY: {
            // struct aerogpu_cmd_set_primitive_topology (16 bytes)
            if (cmdSize < 16) fail("ACMD SET_PRIMITIVE_TOPOLOGY size_bytes too small: " + cmdSize);
            const topology = readU32(pv, off + 8);
            acmdPrimitiveMode = getGlPrimitiveMode(topology);
            break;
          }
          case AEROGPU_CMD_CREATE_SAMPLER: {
            // struct aerogpu_cmd_create_sampler (28 bytes)
            if (cmdSize < AEROGPU_CMD_CREATE_SAMPLER_SIZE_BYTES) fail("ACMD CREATE_SAMPLER size_bytes too small: " + cmdSize);
            break;
          }
          case AEROGPU_CMD_DESTROY_SAMPLER: {
            // struct aerogpu_cmd_destroy_sampler (16 bytes)
            if (cmdSize < AEROGPU_CMD_DESTROY_SAMPLER_SIZE_BYTES) fail("ACMD DESTROY_SAMPLER size_bytes too small: " + cmdSize);
            break;
          }
          case AEROGPU_CMD_SET_SAMPLERS: {
            // struct aerogpu_cmd_set_samplers (24 bytes) + handles
            if (cmdSize < AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES) fail("ACMD SET_SAMPLERS size_bytes too small: " + cmdSize);
            const samplerCount = readU32(pv, off + 16);
            const requiredLen = AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES + samplerCount * 4;
            if (cmdSize < requiredLen) fail("ACMD SET_SAMPLERS packet too small for sampler_count=" + samplerCount);
            break;
          }
          case AEROGPU_CMD_SET_CONSTANT_BUFFERS: {
            // struct aerogpu_cmd_set_constant_buffers (24 bytes) + bindings
            if (cmdSize < AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES) fail("ACMD SET_CONSTANT_BUFFERS size_bytes too small: " + cmdSize);
            const bufferCount = readU32(pv, off + 16);
            const requiredLen = AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES + bufferCount * AEROGPU_CONSTANT_BUFFER_BINDING_SIZE_BYTES;
            if (cmdSize < requiredLen) fail("ACMD SET_CONSTANT_BUFFERS packet too small for buffer_count=" + bufferCount);
            break;
          }
          case AEROGPU_CMD_CLEAR: {
            // struct aerogpu_cmd_clear
            if (cmdSize < AEROGPU_CMD_CLEAR_SIZE_BYTES) fail("ACMD CLEAR size_bytes too small: " + cmdSize);
            const flags = readU32(pv, off + 8);
            let mask = 0;
            if (flags & AEROGPU_CLEAR_COLOR) {
              const r = readF32(pv, off + 12);
              const g = readF32(pv, off + 16);
              const b = readF32(pv, off + 20);
              const a = readF32(pv, off + 24);
              gl.clearColor(r, g, b, a);
              mask |= gl.COLOR_BUFFER_BIT;
            }
            if (flags & AEROGPU_CLEAR_DEPTH) {
              const depth = readF32(pv, off + 28);
              gl.clearDepth(depth);
              mask |= gl.DEPTH_BUFFER_BIT;
            }
            if (flags & AEROGPU_CLEAR_STENCIL) {
              const stencil = readU32(pv, off + 32);
              gl.clearStencil(stencil | 0);
              mask |= gl.STENCIL_BUFFER_BIT;
            }
            if (mask !== 0) gl.clear(mask);
            break;
          }
          case AEROGPU_CMD_DRAW: {
            // struct aerogpu_cmd_draw (24 bytes)
            if (cmdSize < 24) fail("ACMD DRAW size_bytes too small: " + cmdSize);
            const vertexCount = readU32(pv, off + 8);
            const instanceCount = readU32(pv, off + 12);
            const firstVertex = readU32(pv, off + 16);
            const firstInstance = readU32(pv, off + 20);
            if (firstInstance !== 0) fail("ACMD DRAW first_instance not supported: " + firstInstance);

            gl.useProgram(acmdProgram);
            gl.bindVertexArray(acmdVao);
            if (instanceCount <= 1) {
              gl.drawArrays(acmdPrimitiveMode, firstVertex, vertexCount);
            } else {
              gl.drawArraysInstanced(acmdPrimitiveMode, firstVertex, vertexCount, instanceCount);
            }
            gl.bindVertexArray(null);
            break;
          }
          case AEROGPU_CMD_PRESENT: {
            // struct aerogpu_cmd_present
            if (cmdSize < AEROGPU_CMD_PRESENT_SIZE_BYTES) fail("ACMD PRESENT size_bytes too small: " + cmdSize);
            if (acmdColor0) {
              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, acmdColor0.framebuffer);
              gl.readBuffer(gl.COLOR_ATTACHMENT0);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
              gl.drawBuffers([gl.BACK]);
              gl.blitFramebuffer(
                0,
                0,
                acmdColor0.width,
                acmdColor0.height,
                0,
                0,
                canvas.width,
                canvas.height,
                gl.COLOR_BUFFER_BIT,
                gl.NEAREST,
              );
              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, acmdFramebuffer);
            }
            gl.finish();
            break;
          }
          case AEROGPU_CMD_PRESENT_EX: {
            // struct aerogpu_cmd_present_ex
            if (cmdSize < AEROGPU_CMD_PRESENT_EX_SIZE_BYTES) fail("ACMD PRESENT_EX size_bytes too small: " + cmdSize);
            if (acmdColor0) {
              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, acmdColor0.framebuffer);
              gl.readBuffer(gl.COLOR_ATTACHMENT0);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
              gl.drawBuffers([gl.BACK]);
              gl.blitFramebuffer(
                0,
                0,
                acmdColor0.width,
                acmdColor0.height,
                0,
                0,
                canvas.width,
                canvas.height,
                gl.COLOR_BUFFER_BIT,
                gl.NEAREST,
              );
              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, acmdFramebuffer);
            }
            gl.finish();
            break;
          }
          case AEROGPU_CMD_EXPORT_SHARED_SURFACE: {
            // struct aerogpu_cmd_export_shared_surface (24 bytes)
            if (cmdSize < AEROGPU_CMD_EXPORT_SHARED_SURFACE_SIZE_BYTES) fail("ACMD EXPORT_SHARED_SURFACE size_bytes too small: " + cmdSize);
            const resourceHandle = readU32(pv, off + 8);
            const shareToken = readU64Big(pv, off + 16);
            exportSharedSurface(resourceHandle, shareToken);
            break;
          }
          case AEROGPU_CMD_IMPORT_SHARED_SURFACE: {
            // struct aerogpu_cmd_import_shared_surface (24 bytes)
            if (cmdSize < AEROGPU_CMD_IMPORT_SHARED_SURFACE_SIZE_BYTES) fail("ACMD IMPORT_SHARED_SURFACE size_bytes too small: " + cmdSize);
            const outHandle = readU32(pv, off + 8);
            const shareToken = readU64Big(pv, off + 16);
            importSharedSurface(outHandle, shareToken);
            break;
          }
          case AEROGPU_CMD_RELEASE_SHARED_SURFACE: {
            // struct aerogpu_cmd_release_shared_surface (24 bytes)
            if (cmdSize < AEROGPU_CMD_RELEASE_SHARED_SURFACE_SIZE_BYTES) fail("ACMD RELEASE_SHARED_SURFACE size_bytes too small: " + cmdSize);
            const shareToken = readU64Big(pv, off + 8);
            releaseSharedSurface(shareToken);
            break;
          }
          default:
            // Unknown opcode: skip (forward-compat).
            break;
        }

        off += cmdSize;
      }
    }

    async function executePacket(packetBytes, trace, execCtx) {
      if (isAerogpuCmdStreamPacket(packetBytes)) {
        executeAerogpuCmdStream(packetBytes, execCtx);
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
      // Read back from the default framebuffer (canvas), even if the last replayed
      // command left an offscreen framebuffer bound.
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
      const out = new Uint8Array(canvas.width * canvas.height * 4);
      gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, out);
      return out;
    }

    return { gl, executePacket, readPixels };
  }

  function createAerogpuWebgl2Backend(canvas) {
    // See `createWebgl2Backend` for why `antialias:false` matters for determinism
    // and for `blitFramebuffer` support when presenting offscreen render targets.
    const gl = canvas.getContext("webgl2", { preserveDrawingBuffer: true, antialias: false });
    if (!gl) fail("WebGL2 is not available");

    // Reduce driver variance for screenshot comparisons.
    gl.disable(gl.DITHER);
    gl.disable(gl.BLEND);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

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
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      return prog;
    }

    const prog = linkProgram(GLSL_VS, GLSL_FS);
    gl.useProgram(prog);

    const vao = gl.createVertexArray();
    if (!vao) fail("gl.createVertexArray failed");
    gl.bindVertexArray(vao);

    const buffers = new Map(); // handle -> WebGLBuffer
    const textures = new Map(); // handle -> { tex, fb, width, height }

    let currentRenderTarget = null; // handle | null
    let currentTopology = gl.TRIANGLES;

    function currentTargetSize() {
      if (currentRenderTarget !== null && currentRenderTarget !== 0) {
        const rt = textures.get(currentRenderTarget);
        if (rt) return { w: rt.width, h: rt.height };
      }
      return { w: canvas.width, h: canvas.height };
    }

    function bindRenderTarget(rtHandle) {
      if (rtHandle === null || rtHandle === 0) {
        currentRenderTarget = null;
        gl.bindFramebuffer(gl.FRAMEBUFFER, null);
        gl.drawBuffers([gl.BACK]);
        return;
      }
      const rt = textures.get(rtHandle);
      if (!rt) fail("unknown render target handle=" + rtHandle);
      currentRenderTarget = rtHandle;
      gl.bindFramebuffer(gl.FRAMEBUFFER, rt.fb);
      gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
    }

    function executeCmdStream(cmdStreamBytes, memAllocs) {
      const dv = new DataView(
        cmdStreamBytes.buffer,
        cmdStreamBytes.byteOffset,
        cmdStreamBytes.byteLength,
      );
      const bufLen = dv.byteLength;
      if (bufLen < AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES) fail("cmd stream too small");

      let sizeBytes = 0;
      if (decodeCmdStreamHeader) {
        const hdr = decodeCmdStreamHeader(dv, 0);
        sizeBytes = hdr.sizeBytes >>> 0;
      } else {
        const magic = dv.getUint32(0, true);
        if (magic !== AEROGPU_CMD_STREAM_MAGIC_U32) fail("bad cmd stream magic");

        const abiVersion = dv.getUint32(4, true);
        const major = (abiVersion >>> 16) & 0xffff;
        if (major !== 1) fail("unsupported cmd stream ABI major: " + major);

        sizeBytes = dv.getUint32(8, true);
      }
      if (sizeBytes < AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES || sizeBytes > bufLen) {
        fail("invalid cmd stream size_bytes=" + sizeBytes);
      }

      let off = 0;
      let opcode = 0;
      let cmdSizeBytes = 0;
      let end = 0;

      let nextPacket;
      if (AerogpuCmdStreamIter) {
        let iter;
        try {
          iter = new AerogpuCmdStreamIter(cmdStreamBytes);
        } catch (err) {
          fail(err && err.message ? err.message : String(err));
        }

        nextPacket = () => {
          let res;
          try {
            res = iter.next();
          } catch (err) {
            fail(err && err.message ? err.message : String(err));
          }
          if (res.done) return false;
          off = res.value.offsetBytes >>> 0;
          end = res.value.endBytes >>> 0;
          opcode = res.value.hdr.opcode >>> 0;
          cmdSizeBytes = res.value.hdr.sizeBytes >>> 0;
          return true;
        };
      } else {
        off = AEROGPU_CMD_STREAM_HEADER_SIZE_BYTES;
        nextPacket = () => {
          if (off >= sizeBytes) return false;
          if (off + AEROGPU_CMD_HDR_SIZE_BYTES > sizeBytes) fail("truncated cmd header at offset " + off);

          if (decodeCmdHdr) {
            const hdr = decodeCmdHdr(dv, off);
            opcode = hdr.opcode >>> 0;
            cmdSizeBytes = hdr.sizeBytes >>> 0;
          } else {
            opcode = dv.getUint32(off + 0, true);
            cmdSizeBytes = dv.getUint32(off + 4, true);
            if (cmdSizeBytes < AEROGPU_CMD_HDR_SIZE_BYTES) {
              fail("invalid cmd size_bytes=" + cmdSizeBytes + " at offset " + off);
            }
            if (cmdSizeBytes % 4 !== 0) {
              fail("misaligned cmd size_bytes=" + cmdSizeBytes + " at offset " + off);
            }
          }

          end = off + cmdSizeBytes;
          if (end > sizeBytes) {
            fail("cmd overruns stream (end=" + end + ", size=" + sizeBytes + ")");
          }
          return true;
        };
      }

      while (nextPacket()) {
        switch (opcode) {
          case AEROGPU_CMD_CREATE_BUFFER: {
            if (cmdSizeBytes < AEROGPU_CMD_CREATE_BUFFER_SIZE_BYTES) fail("CREATE_BUFFER packet too small");
            const bufferHandle = dv.getUint32(off + 8, true);
            const sizeBytesU64 = readU64Big(dv, off + 16);
            const backingAllocId = dv.getUint32(off + 24, true);
            const backingOffsetBytes = dv.getUint32(off + 28, true);

            const size = u64BigToSafeNumber(sizeBytesU64, "CREATE_BUFFER.size_bytes");
            const glBuf = gl.createBuffer();
            if (!glBuf) fail("gl.createBuffer failed");
            buffers.set(bufferHandle, glBuf);
            gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);

            if (backingAllocId !== 0) {
              const allocBytes = memAllocs.get(backingAllocId);
              if (!allocBytes) fail("missing alloc_id=" + backingAllocId + " for CREATE_BUFFER");
              const endOff = backingOffsetBytes + size;
              if (endOff > allocBytes.byteLength) fail("CREATE_BUFFER backing out of bounds");
              gl.bufferData(
                gl.ARRAY_BUFFER,
                allocBytes.subarray(backingOffsetBytes, endOff),
                gl.STATIC_DRAW,
              );
            } else {
              gl.bufferData(gl.ARRAY_BUFFER, size, gl.STATIC_DRAW);
            }
            break;
          }

          case AEROGPU_CMD_CREATE_TEXTURE2D: {
            if (cmdSizeBytes < AEROGPU_CMD_CREATE_TEXTURE2D_SIZE_BYTES) fail("CREATE_TEXTURE2D packet too small");
            const textureHandle = dv.getUint32(off + 8, true);
            const format = dv.getUint32(off + 16, true);
            const width = dv.getUint32(off + 20, true);
            const height = dv.getUint32(off + 24, true);
            const mipLevels = dv.getUint32(off + 28, true);
            const arrayLayers = dv.getUint32(off + 32, true);
            const rowPitchBytes = dv.getUint32(off + 36, true);
            const backingAllocId = dv.getUint32(off + 40, true);
            const backingOffsetBytes = dv.getUint32(off + 44, true);

            if (width === 0 || height === 0) fail("CREATE_TEXTURE2D invalid dimensions");
            if (mipLevels !== 1 || arrayLayers !== 1) {
              fail("CREATE_TEXTURE2D only supports mip_levels=1, array_layers=1");
            }
            if (format !== AEROGPU_FORMAT_R8G8B8A8_UNORM && format !== AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB) {
              const bcHint = format >= 64 && format <= 71 ? " (BC formats require GPU backend)" : "";
              fail("unsupported texture format " + format + bcHint);
            }

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
              width,
              height,
              0,
              gl.RGBA,
              gl.UNSIGNED_BYTE,
              null,
            );

            if (backingAllocId !== 0) {
              const allocBytes = memAllocs.get(backingAllocId);
              if (!allocBytes) fail("missing alloc_id=" + backingAllocId + " for CREATE_TEXTURE2D");

              const rowBytes = width * 4;
              const pitch = rowPitchBytes !== 0 ? rowPitchBytes : rowBytes;
              if (pitch < rowBytes) {
                fail("CREATE_TEXTURE2D row_pitch_bytes too small: " + pitch + " < " + rowBytes);
              }
              const requiredBytes = backingOffsetBytes + pitch * height;
              if (requiredBytes > allocBytes.byteLength) {
                fail("CREATE_TEXTURE2D backing out of bounds");
              }

              const packed = new Uint8Array(rowBytes * height);
              for (let y = 0; y < height; y++) {
                const srcOff = backingOffsetBytes + y * pitch;
                packed.set(allocBytes.subarray(srcOff, srcOff + rowBytes), y * rowBytes);
              }
              gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, packed);
            }

            const fb = gl.createFramebuffer();
            if (!fb) fail("gl.createFramebuffer failed");
            gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
            gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
            gl.drawBuffers([gl.COLOR_ATTACHMENT0]);
            const status = gl.checkFramebufferStatus(gl.FRAMEBUFFER);
            if (status !== gl.FRAMEBUFFER_COMPLETE) {
              fail("incomplete framebuffer (status=0x" + status.toString(16) + ")");
            }
            textures.set(textureHandle, { tex, fb, width, height });
            // Creating resources should not implicitly switch render targets.
            bindRenderTarget(currentRenderTarget);
            break;
          }

          case AEROGPU_CMD_SET_RENDER_TARGETS: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_RENDER_TARGETS_SIZE_BYTES) fail("SET_RENDER_TARGETS packet too small");
            const colorCount = dv.getUint32(off + 8, true);
            const rt0 = dv.getUint32(off + 16, true);
            bindRenderTarget(colorCount > 0 ? rt0 : null);
            break;
          }

          case AEROGPU_CMD_SET_VIEWPORT: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_VIEWPORT_SIZE_BYTES) fail("SET_VIEWPORT packet too small");
            const x = dv.getFloat32(off + 8, true);
            const y = dv.getFloat32(off + 12, true);
            const wReq = dv.getFloat32(off + 16, true);
            const hReq = dv.getFloat32(off + 20, true);
            const size = currentTargetSize();
            const w = wReq > 0 ? wReq : size.w;
            const h = hReq > 0 ? hReq : size.h;
            gl.viewport(Math.floor(x), Math.floor(y), Math.floor(w), Math.floor(h));
            break;
          }

          case AEROGPU_CMD_CLEAR: {
            if (cmdSizeBytes < AEROGPU_CMD_CLEAR_SIZE_BYTES) fail("CLEAR packet too small");
            const flags = dv.getUint32(off + 8, true);
            if ((flags & AEROGPU_CLEAR_COLOR) !== 0) {
              const r = dv.getFloat32(off + 12, true);
              const g = dv.getFloat32(off + 16, true);
              const b = dv.getFloat32(off + 20, true);
              const a = dv.getFloat32(off + 24, true);
              gl.clearColor(r, g, b, a);
              gl.clear(gl.COLOR_BUFFER_BIT);
            }
            break;
          }

          case AEROGPU_CMD_SET_VERTEX_BUFFERS: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE_BYTES) fail("SET_VERTEX_BUFFERS packet too small");
            const startSlot = dv.getUint32(off + 8, true);
            const bufferCount = dv.getUint32(off + 12, true);
            if (startSlot !== 0) fail("SET_VERTEX_BUFFERS only supports start_slot=0");
            const neededBytes = AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE_BYTES + bufferCount * 16;
            if (cmdSizeBytes < neededBytes) fail("SET_VERTEX_BUFFERS packet too small for bindings");
            if (bufferCount === 0) break;

            // First binding.
            const bindOff = off + AEROGPU_CMD_SET_VERTEX_BUFFERS_SIZE_BYTES;
            const bufHandle = dv.getUint32(bindOff + 0, true);
            const strideBytes = dv.getUint32(bindOff + 4, true);
            const offsetBytes = dv.getUint32(bindOff + 8, true);
            const glBuf = buffers.get(bufHandle);
            if (!glBuf) fail("unknown vertex buffer handle=" + bufHandle);
            gl.bindBuffer(gl.ARRAY_BUFFER, glBuf);
            gl.enableVertexAttribArray(0);
            gl.vertexAttribPointer(0, 2, gl.FLOAT, false, strideBytes, offsetBytes + 0);
            gl.enableVertexAttribArray(1);
            gl.vertexAttribPointer(1, 4, gl.FLOAT, false, strideBytes, offsetBytes + 8);
            break;
          }

          case AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY_SIZE_BYTES) fail("SET_PRIMITIVE_TOPOLOGY packet too small");
            const topology = dv.getUint32(off + 8, true);
            if (topology !== AEROGPU_TOPOLOGY_TRIANGLELIST) {
              fail("unsupported primitive topology " + topology);
            }
            currentTopology = gl.TRIANGLES;
            break;
          }

          case AEROGPU_CMD_CREATE_SAMPLER: {
            if (cmdSizeBytes < AEROGPU_CMD_CREATE_SAMPLER_SIZE_BYTES) fail("CREATE_SAMPLER packet too small");
            break;
          }

          case AEROGPU_CMD_DESTROY_SAMPLER: {
            if (cmdSizeBytes < AEROGPU_CMD_DESTROY_SAMPLER_SIZE_BYTES) fail("DESTROY_SAMPLER packet too small");
            break;
          }

          case AEROGPU_CMD_SET_SAMPLERS: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES) fail("SET_SAMPLERS packet too small");
            const samplerCount = dv.getUint32(off + 16, true);
            const neededBytes = AEROGPU_CMD_SET_SAMPLERS_SIZE_BYTES + samplerCount * 4;
            if (cmdSizeBytes < neededBytes) fail("SET_SAMPLERS packet too small for sampler_count=" + samplerCount);
            break;
          }

          case AEROGPU_CMD_SET_CONSTANT_BUFFERS: {
            if (cmdSizeBytes < AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES) fail("SET_CONSTANT_BUFFERS packet too small");
            const bufferCount = dv.getUint32(off + 16, true);
            const neededBytes = AEROGPU_CMD_SET_CONSTANT_BUFFERS_SIZE_BYTES + bufferCount * AEROGPU_CONSTANT_BUFFER_BINDING_SIZE_BYTES;
            if (cmdSizeBytes < neededBytes) fail("SET_CONSTANT_BUFFERS packet too small for buffer_count=" + bufferCount);
            break;
          }

          case AEROGPU_CMD_DRAW: {
            if (cmdSizeBytes < AEROGPU_CMD_DRAW_SIZE_BYTES) fail("DRAW packet too small");
            const vertexCount = dv.getUint32(off + 8, true);
            const instanceCount = dv.getUint32(off + 12, true);
            const firstVertex = dv.getUint32(off + 16, true);
            const firstInstance = dv.getUint32(off + 20, true);
            if (instanceCount !== 1 || firstInstance !== 0) fail("instancing not supported");
            gl.drawArrays(currentTopology, firstVertex, vertexCount);
            break;
          }

          case AEROGPU_CMD_PRESENT: {
            if (cmdSizeBytes < AEROGPU_CMD_PRESENT_SIZE_BYTES) fail("PRESENT packet too small");
            if (currentRenderTarget !== null && currentRenderTarget !== 0) {
              const rt = textures.get(currentRenderTarget);
              if (!rt) fail("missing current render target");

              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, rt.fb);
              gl.readBuffer(gl.COLOR_ATTACHMENT0);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
              gl.drawBuffers([gl.BACK]);
              gl.blitFramebuffer(
                0,
                0,
                rt.width,
                rt.height,
                0,
                0,
                canvas.width,
                canvas.height,
                gl.COLOR_BUFFER_BIT,
                gl.NEAREST,
              );
              gl.bindFramebuffer(gl.FRAMEBUFFER, null);
            }
            gl.finish();
            break;
          }

          case AEROGPU_CMD_PRESENT_EX: {
            if (cmdSizeBytes < AEROGPU_CMD_PRESENT_EX_SIZE_BYTES) fail("PRESENT_EX packet too small");
            // PresentEx is currently treated as Present for replay purposes.
            if (currentRenderTarget !== null && currentRenderTarget !== 0) {
              const rt = textures.get(currentRenderTarget);
              if (!rt) fail("missing current render target");

              gl.bindFramebuffer(gl.READ_FRAMEBUFFER, rt.fb);
              gl.readBuffer(gl.COLOR_ATTACHMENT0);
              gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, null);
              gl.drawBuffers([gl.BACK]);
              gl.blitFramebuffer(
                0,
                0,
                rt.width,
                rt.height,
                0,
                0,
                canvas.width,
                canvas.height,
                gl.COLOR_BUFFER_BIT,
                gl.NEAREST,
              );
              gl.bindFramebuffer(gl.FRAMEBUFFER, null);
            }
            gl.finish();
            break;
          }

          default:
            // Unknown opcodes are skipped (forward-compat).
            break;
        }

        off = end;
      }
    }

    async function executeSubmission(submission, trace) {
      const cmdBlob = trace.blobs.get(submission.cmdStreamBlobId);
      if (!cmdBlob) fail("missing cmd_stream_blob_id=" + submission.cmdStreamBlobId.toString());
      if (cmdBlob.kind !== BLOB_AEROGPU_CMD_STREAM) fail("unexpected cmd stream blob kind");

      if (submission.allocTableBlobId !== 0n) {
        const allocTableBlob = trace.blobs.get(submission.allocTableBlobId);
        if (!allocTableBlob) {
          fail("missing alloc_table_blob_id=" + submission.allocTableBlobId.toString());
        }
        if (allocTableBlob.kind !== BLOB_AEROGPU_ALLOC_TABLE) {
          fail("unexpected alloc table blob kind");
        }
      }

      const memAllocs = new Map(); // alloc_id -> Uint8Array
      for (const r of submission.memoryRanges) {
        const blob = trace.blobs.get(r.blobId);
        if (!blob) fail("missing memory blob_id=" + r.blobId.toString());
        if (blob.kind !== BLOB_AEROGPU_ALLOC_MEMORY) fail("unexpected alloc memory blob kind");
        memAllocs.set(r.allocId, blob.bytes);
      }

      executeCmdStream(cmdBlob.bytes, memAllocs);
    }

    function readPixels() {
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      const out = new Uint8Array(canvas.width * canvas.height * 4);
      gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, out);
      return out;
    }

    return { gl, executeSubmission, readPixels };
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
    // `command_abi_version` is primarily for tooling; the actual packet format is
    // detected per-record. (Old traces used `RecordType::Packet` with raw ACMD
    // bytes, new canonical traces use `RecordType::AerogpuSubmission` + blobs.)
    //
    // Currently supported:
    // - 1: Minimal reference command ABI v1 (Appendix A).
    // - (major=1, minor=*): AeroGPU command stream ABI (A3A0), i.e. `0x0001_xxxx`.
    const abi = trace.commandAbiVersion >>> 0;
    const isMinimalAbiV1 = abi === 1;
    const isAerogpuAbiV1 = (abi >>> 16) === 1;
    if (!isMinimalAbiV1 && !isAerogpuAbiV1) {
      fail(
        "unsupported command_abi_version=" +
          abi +
          " (supported: 1 (minimal ABI v1) or 0x0001_xxxx (AeroGPU ABI v1))",
      );
    }
    let hasSubmissions = false;
    for (const actions of trace.frameActions.values()) {
      for (const a of actions) {
        if (a.kind === "aerogpuSubmission") {
          hasSubmissions = true;
          break;
        }
      }
      if (hasSubmissions) break;
    }
    if (hasSubmissions && !isAerogpuAbiV1) {
      fail("AerogpuSubmission records require an AeroGPU ABI v1 command_abi_version");
    }
    if (isAerogpuAbiV1) await maybeInitAerogpuProtocol();
    const backendName = (opts && opts.backend) || "webgl2";
    const backend =
      backendName === "webgpu"
        ? hasSubmissions
          ? fail(
              "backend webgpu does not support AeroGPU submission traces (use backend: 'webgl2')",
            )
          : !isMinimalAbiV1
            ? fail(
                "backend webgpu does not support AeroGPU command stream packet traces (use backend: 'webgl2')",
              )
            : await createWebgpuBackend(canvas)
        : createWebgl2Backend(canvas);
    let cursor = 0;
    let playing = false;

    async function replayFrame(frameIndex) {
      const actions = trace.frameActions.get(frameIndex);
      if (!actions) fail("no such frame " + frameIndex);
      for (const a of actions) {
        if (a.kind === "packet") {
          await backend.executePacket(a.bytes, trace);
        } else if (a.kind === "aerogpuSubmission") {
          const cmdBlob = trace.blobs.get(a.cmd_stream_blob_id);
          if (!cmdBlob) fail("missing cmd_stream_blob_id=" + a.cmd_stream_blob_id.toString());
          if (cmdBlob.kind !== BLOB_AEROGPU_CMD_STREAM) fail("unexpected blob kind for cmd_stream_blob_id");

          if (a.alloc_table_blob_id !== 0n) {
            const allocTableBlob = trace.blobs.get(a.alloc_table_blob_id);
            if (!allocTableBlob) fail("missing alloc_table_blob_id=" + a.alloc_table_blob_id.toString());
            if (allocTableBlob.kind !== BLOB_AEROGPU_ALLOC_TABLE) {
              fail("unexpected blob kind for alloc_table_blob_id");
            }
          }

          const allocMemory = new Map(); // alloc_id -> {bytes, sizeBytes, gpa, flags}
          for (const r of a.memory_ranges) {
            const memBlob = trace.blobs.get(r.blob_id);
            if (!memBlob) fail("missing memory blob_id=" + r.blob_id.toString());
            if (memBlob.kind !== BLOB_AEROGPU_ALLOC_MEMORY) fail("unexpected blob kind for alloc memory");
            if (allocMemory.has(r.alloc_id)) fail("duplicate alloc_id in memory_ranges: " + r.alloc_id);
            const sizeBytes = u64BigToSafeNumber(r.size_bytes, "memory_range.size_bytes");
            if (memBlob.bytes.byteLength !== sizeBytes) fail("alloc memory size_bytes mismatch");
            allocMemory.set(r.alloc_id, { bytes: memBlob.bytes, sizeBytes, gpa: r.gpa, flags: r.flags });
          }

          await backend.executePacket(cmdBlob.bytes, trace, { allocMemory });
        } else {
          fail("unknown action kind: " + String(a.kind));
        }
      }
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
