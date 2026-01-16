/**
 * Canonical Aero TCP multiplexing framing: `aero-tcp-mux-v1`.
 *
 * This matches the gateway implementation in `backend/aero-gateway` and the
 * public contract in `docs/backend/01-aero-gateway-api.md`.
 *
 * Transport model: all WebSocket *binary* messages are treated as an arbitrary
 * byte stream that carries one or more protocol frames. Frames may be split
 * across WebSocket messages or concatenated within a message.
 *
 * All multi-byte integers are big-endian (network byte order).
 */

import { sanitizeOneLine, truncateUtf8 } from "./text.js";

export const TCP_MUX_SUBPROTOCOL = "aero-tcp-mux-v1";

export const TCP_MUX_HEADER_BYTES = 9;

// Defensive caps: OPEN payload strings are attacker-controlled and should never be large.
// Hostnames are <=253 chars on the wire; allow some slack for IPv6 literals and future extensions.
export const MAX_TCP_MUX_OPEN_HOST_BYTES = 1024;
export const MAX_TCP_MUX_OPEN_METADATA_BYTES = 4 * 1024;
export const MAX_TCP_MUX_ERROR_MESSAGE_BYTES = 1024;

export const TcpMuxMsgType = Object.freeze({
  OPEN: 1,
  DATA: 2,
  CLOSE: 3,
  ERROR: 4,
  PING: 5,
  PONG: 6,
});

export const TcpMuxCloseFlags = Object.freeze({
  FIN: 0x01,
  RST: 0x02,
});

// Matches `backend/aero-gateway/src/protocol/tcpMux.ts`.
export const TcpMuxErrorCode = Object.freeze({
  POLICY_DENIED: 1,
  DIAL_FAILED: 2,
  PROTOCOL_ERROR: 3,
  UNKNOWN_STREAM: 4,
  STREAM_LIMIT_EXCEEDED: 5,
  STREAM_BUFFER_OVERFLOW: 6,
});

/**
 * @typedef {{ msgType: number, streamId: number, payload: Buffer }} TcpMuxFrame
 */

export function encodeTcpMuxFrame(msgType, streamId, payload) {
  const payloadBuf = payload ?? Buffer.alloc(0);
  const buf = Buffer.allocUnsafe(TCP_MUX_HEADER_BYTES + payloadBuf.length);
  buf.writeUInt8(msgType, 0);
  buf.writeUInt32BE(streamId >>> 0, 1);
  buf.writeUInt32BE(payloadBuf.length >>> 0, 5);
  payloadBuf.copy(buf, TCP_MUX_HEADER_BYTES);
  return buf;
}

export class TcpMuxFrameParser {
  /** @type {Buffer} */
  buffer = Buffer.alloc(0);
  maxFramePayloadBytes;

  constructor(maxFramePayloadBytes = 16 * 1024 * 1024) {
    if (!Number.isInteger(maxFramePayloadBytes) || maxFramePayloadBytes < 0) {
      throw new Error(`Invalid maxFramePayloadBytes: ${maxFramePayloadBytes}`);
    }
    this.maxFramePayloadBytes = maxFramePayloadBytes;
  }

  /**
   * @param {Buffer} chunk
   * @returns {TcpMuxFrame[]}
   */
  push(chunk) {
    if (chunk.length === 0) return [];
    this.buffer = this.buffer.length === 0 ? chunk : concat2(this.buffer, chunk);

    /** @type {TcpMuxFrame[]} */
    const frames = [];

    while (this.buffer.length >= TCP_MUX_HEADER_BYTES) {
      const msgType = this.buffer.readUInt8(0);
      const streamId = this.buffer.readUInt32BE(1);
      const length = this.buffer.readUInt32BE(5);
      if (length > this.maxFramePayloadBytes) {
        throw new Error(`Frame payload length ${length} exceeds max ${this.maxFramePayloadBytes}`);
      }

      const frameTotalBytes = TCP_MUX_HEADER_BYTES + length;
      if (this.buffer.length < frameTotalBytes) break;

      const payload = this.buffer.subarray(TCP_MUX_HEADER_BYTES, frameTotalBytes);
      frames.push({ msgType, streamId, payload });
      // Avoid keeping a reference to the backing allocation when fully consumed.
      this.buffer =
        frameTotalBytes === this.buffer.length ? Buffer.alloc(0) : this.buffer.subarray(frameTotalBytes);
    }

    return frames;
  }

  pendingBytes() {
    return this.buffer.length;
  }

  finish() {
    if (this.buffer.length === 0) return;
    throw new Error(`truncated tcp-mux frame stream (${this.buffer.length} pending bytes)`);
  }
}

function concat2(a, b) {
  const out = Buffer.allocUnsafe(a.length + b.length);
  a.copy(out, 0);
  b.copy(out, a.length);
  return out;
}

/**
 * @typedef {{ host: string, port: number, metadata?: string }} TcpMuxOpenPayload
 */

/**
 * @param {TcpMuxOpenPayload} payload
 * @returns {Buffer}
 */
export function encodeTcpMuxOpenPayload(payload) {
  const hostBytes = Buffer.from(payload.host, "utf8");
  const metadataBytes = payload.metadata ? Buffer.from(payload.metadata, "utf8") : Buffer.alloc(0);

  if (hostBytes.length > MAX_TCP_MUX_OPEN_HOST_BYTES) {
    throw new Error("host too long");
  }
  if (metadataBytes.length > MAX_TCP_MUX_OPEN_METADATA_BYTES) {
    throw new Error("metadata too long");
  }
  if (!Number.isInteger(payload.port) || payload.port < 1 || payload.port > 65535) {
    throw new Error("invalid port");
  }

  const buf = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2 + metadataBytes.length);
  let offset = 0;
  buf.writeUInt16BE(hostBytes.length, offset);
  offset += 2;
  hostBytes.copy(buf, offset);
  offset += hostBytes.length;
  buf.writeUInt16BE(payload.port, offset);
  offset += 2;
  buf.writeUInt16BE(metadataBytes.length, offset);
  offset += 2;
  metadataBytes.copy(buf, offset);
  return buf;
}

/**
 * @param {Buffer} buf
 * @returns {TcpMuxOpenPayload}
 */
export function decodeTcpMuxOpenPayload(buf) {
  if (buf.length < 2 + 2 + 2) {
    throw new Error("OPEN payload too short");
  }

  let offset = 0;
  const hostLen = buf.readUInt16BE(offset);
  offset += 2;
  if (hostLen > MAX_TCP_MUX_OPEN_HOST_BYTES) {
    throw new Error("host too long");
  }
  if (buf.length < offset + hostLen + 2 + 2) {
    throw new Error("OPEN payload truncated (host)");
  }
  const host = buf.subarray(offset, offset + hostLen).toString("utf8");
  offset += hostLen;
  const port = buf.readUInt16BE(offset);
  offset += 2;
  const metadataLen = buf.readUInt16BE(offset);
  offset += 2;
  if (metadataLen > MAX_TCP_MUX_OPEN_METADATA_BYTES) {
    throw new Error("metadata too long");
  }
  if (buf.length < offset + metadataLen) {
    throw new Error("OPEN payload truncated (metadata)");
  }
  const metadata = metadataLen > 0 ? buf.subarray(offset, offset + metadataLen).toString("utf8") : undefined;
  offset += metadataLen;
  if (offset !== buf.length) {
    throw new Error("OPEN payload has trailing bytes");
  }
  return { host, port, metadata };
}

export function encodeTcpMuxClosePayload(flags) {
  const buf = Buffer.allocUnsafe(1);
  buf.writeUInt8(flags & 0xff, 0);
  return buf;
}

export function decodeTcpMuxClosePayload(buf) {
  if (buf.length !== 1) {
    throw new Error("CLOSE payload must be exactly 1 byte");
  }
  return { flags: buf.readUInt8(0) };
}

function coerceTcpMuxErrorMessage(message) {
  if (message == null) return "";
  switch (typeof message) {
    case "string":
      return message;
    case "number":
    case "boolean":
    case "bigint":
      return String(message);
    case "symbol":
    case "undefined":
      return "";
    case "object": {
      try {
        const msg = message && typeof message.message === "string" ? message.message : null;
        return msg ?? "";
      } catch {
        // ignore getters throwing
        return "";
      }
    }
    case "function":
    default:
      return "";
  }
}

export function encodeTcpMuxErrorPayload(code, message) {
  const safeMessage = truncateUtf8(
    sanitizeOneLine(coerceTcpMuxErrorMessage(message)),
    MAX_TCP_MUX_ERROR_MESSAGE_BYTES,
  );
  const messageBytes = Buffer.from(safeMessage, "utf8");
  const buf = Buffer.allocUnsafe(2 + 2 + messageBytes.length);
  buf.writeUInt16BE(code & 0xffff, 0);
  buf.writeUInt16BE(messageBytes.length, 2);
  messageBytes.copy(buf, 4);
  return buf;
}

export function decodeTcpMuxErrorPayload(buf) {
  if (buf.length < 4) {
    throw new Error("ERROR payload too short");
  }
  const code = buf.readUInt16BE(0);
  const messageLen = buf.readUInt16BE(2);
  if (messageLen > MAX_TCP_MUX_ERROR_MESSAGE_BYTES) {
    throw new Error("error message too long");
  }
  if (buf.length !== 4 + messageLen) {
    throw new Error("ERROR payload length mismatch");
  }
  const message = buf.subarray(4).toString("utf8");
  return { code, message: sanitizeOneLine(message) };
}
