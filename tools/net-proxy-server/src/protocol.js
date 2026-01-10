/**
 * Aero TCP proxy framing (v1).
 *
 * Each WebSocket message is exactly one frame. This intentionally avoids
 * streaming/partial framing complexity and keeps browser code simple.
 *
 * All multi-byte integers are big-endian (network byte order).
 */

export const FrameType = Object.freeze({
  OPEN: 1,
  DATA: 2,
  CLOSE: 3,
  ERROR: 4,
});

export const ErrorCode = Object.freeze({
  AUTH_REQUIRED: 1,
  AUTH_INVALID: 2,
  POLICY_DENIED: 3,
  RATE_LIMITED: 4,
  INVALID_FRAME: 5,
  CONNECT_FAILED: 6,
  SOCKET_ERROR: 7,
  BACKPRESSURE: 8,
  INTERNAL_ERROR: 9,
});

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

function asU8(data) {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  // ws library delivers Buffer (which is a Uint8Array subclass), so this covers it.
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  throw new TypeError(`Unsupported frame data type: ${Object.prototype.toString.call(data)}`);
}

function writeHeader(buf, type, connectionId) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  dv.setUint8(0, type);
  dv.setUint32(1, connectionId >>> 0, false);
}

export function encodeOpenRequest(connectionId, dstIpV4, dstPort) {
  if (!(dstIpV4 instanceof Uint8Array) || dstIpV4.length !== 4) {
    throw new TypeError("dstIpV4 must be Uint8Array(4)");
  }
  if (!Number.isInteger(dstPort) || dstPort < 0 || dstPort > 65535) {
    throw new RangeError("dstPort must be u16");
  }
  const buf = new Uint8Array(5 + 1 + 4 + 2);
  writeHeader(buf, FrameType.OPEN, connectionId);
  buf[5] = 4; // ip_version
  buf.set(dstIpV4, 6);
  new DataView(buf.buffer, buf.byteOffset, buf.byteLength).setUint16(10, dstPort, false);
  return buf;
}

export function encodeOpenAck(connectionId) {
  const buf = new Uint8Array(5);
  writeHeader(buf, FrameType.OPEN, connectionId);
  return buf;
}

export function encodeData(connectionId, payload) {
  const p = asU8(payload);
  const buf = new Uint8Array(5 + p.byteLength);
  writeHeader(buf, FrameType.DATA, connectionId);
  buf.set(p, 5);
  return buf;
}

export function encodeClose(connectionId) {
  const buf = new Uint8Array(5);
  writeHeader(buf, FrameType.CLOSE, connectionId);
  return buf;
}

export function encodeError(connectionId, code, message) {
  if (!Number.isInteger(code) || code < 0 || code > 65535) throw new RangeError("code must be u16");
  const msgBytes = textEncoder.encode(String(message ?? ""));
  if (msgBytes.byteLength > 65535) throw new RangeError("message too long");
  const buf = new Uint8Array(5 + 2 + 2 + msgBytes.byteLength);
  writeHeader(buf, FrameType.ERROR, connectionId);
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  dv.setUint16(5, code, false);
  dv.setUint16(7, msgBytes.byteLength, false);
  buf.set(msgBytes, 9);
  return buf;
}

export function decodeFrame(data) {
  const u8 = asU8(data);
  if (u8.byteLength < 5) throw new Error("Frame too short");
  const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  const type = dv.getUint8(0);
  const connectionId = dv.getUint32(1, false);
  const payload = u8.subarray(5);

  switch (type) {
    case FrameType.OPEN: {
      // OPEN ack has empty payload; OPEN request has ip_version+ip+port.
      if (payload.byteLength === 0) return { type, connectionId, kind: "ack" };
      if (payload.byteLength < 1) throw new Error("OPEN frame missing ip_version");
      const ipVersion = payload[0];
      if (ipVersion !== 4) throw new Error(`Unsupported ip_version ${ipVersion}`);
      if (payload.byteLength !== 1 + 4 + 2) throw new Error("Invalid OPEN frame length");
      const dstIp = payload.subarray(1, 5);
      const dstPort = new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint16(5, false);
      return { type, connectionId, kind: "request", ipVersion, dstIp, dstPort };
    }
    case FrameType.DATA:
      return { type, connectionId, data: payload };
    case FrameType.CLOSE:
      if (payload.byteLength !== 0) throw new Error("Invalid CLOSE frame length");
      return { type, connectionId };
    case FrameType.ERROR: {
      if (payload.byteLength < 4) throw new Error("Invalid ERROR frame length");
      const code = new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint16(0, false);
      const msgLen = new DataView(payload.buffer, payload.byteOffset, payload.byteLength).getUint16(2, false);
      if (payload.byteLength !== 4 + msgLen) throw new Error("Invalid ERROR msg_len");
      const msgBytes = payload.subarray(4, 4 + msgLen);
      const message = textDecoder.decode(msgBytes);
      return { type, connectionId, code, message };
    }
    default:
      throw new Error(`Unknown frame type ${type}`);
  }
}

