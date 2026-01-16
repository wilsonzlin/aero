import ipaddr from "ipaddr.js";
import { formatOneLineUtf8 } from "./text.js";

export const FrameType = Object.freeze({
  CONNECT: 0x01,
  DATA: 0x02,
  END: 0x03,
  CLOSE: 0x04,

  OPENED: 0x10,
  DATA_FROM_REMOTE: 0x11,
  END_FROM_REMOTE: 0x12,
  CLOSE_FROM_REMOTE: 0x13,
});

export const AddrType = Object.freeze({
  HOSTNAME: 0x01,
  IPV4: 0x02,
  IPV6: 0x03,
});

export const OpenStatus = Object.freeze({
  OK: 0,
  POLICY: 1,
  DNS: 2,
  CONNECT: 3,
  LIMIT: 4,
  PROTOCOL: 5,
});

export const CloseReason = Object.freeze({
  NORMAL: 0,
  REMOTE_CLOSED: 1,
  ERROR: 2,
  POLICY: 3,
  PROTOCOL: 4,
  RATE_LIMIT: 5,
});

const MAX_PROTOCOL_MESSAGE_BYTES = 1024;

function u32be(value) {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(value >>> 0, 0);
  return b;
}

function u16be(value) {
  const b = Buffer.alloc(2);
  b.writeUInt16BE(value, 0);
  return b;
}

function encodeStringWithLength(str) {
  const safe = formatOneLineUtf8(str, MAX_PROTOCOL_MESSAGE_BYTES);
  const encoded = Buffer.from(safe, "utf8");
  return Buffer.concat([u16be(encoded.length), encoded]);
}

export function encodeConnectFrame({ connId, host, port }) {
  if (!Number.isInteger(connId) || connId < 0 || connId > 0xffffffff) throw new Error("Invalid connId");
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid port");

  const hostBytes = ipaddr.isValid(host) ? Buffer.from(ipaddr.parse(host).toByteArray()) : Buffer.from(host, "utf8");
  let addrType = AddrType.HOSTNAME;
  if (ipaddr.isValid(host)) addrType = hostBytes.length === 4 ? AddrType.IPV4 : AddrType.IPV6;
  if (addrType === AddrType.HOSTNAME && hostBytes.length > 255) throw new Error("Hostname too long");

  const addrLen = hostBytes.length;
  const header = Buffer.from([FrameType.CONNECT, ...u32be(connId), addrType, addrLen]);
  return Buffer.concat([header, hostBytes, u16be(port)]);
}

export function encodeClientDataFrame({ connId, data }) {
  const payload = Buffer.isBuffer(data) ? data : Buffer.from(data);
  return Buffer.concat([Buffer.from([FrameType.DATA]), u32be(connId), payload]);
}

export function encodeClientEndFrame({ connId }) {
  return Buffer.concat([Buffer.from([FrameType.END]), u32be(connId)]);
}

export function encodeClientCloseFrame({ connId }) {
  return Buffer.concat([Buffer.from([FrameType.CLOSE]), u32be(connId)]);
}

export function encodeOpenedFrame({ connId, status, message = "" }) {
  return Buffer.concat([Buffer.from([FrameType.OPENED]), u32be(connId), Buffer.from([status]), encodeStringWithLength(message)]);
}

export function encodeServerDataFrame({ connId, data }) {
  const payload = Buffer.isBuffer(data) ? data : Buffer.from(data);
  return Buffer.concat([Buffer.from([FrameType.DATA_FROM_REMOTE]), u32be(connId), payload]);
}

export function encodeServerEndFrame({ connId }) {
  return Buffer.concat([Buffer.from([FrameType.END_FROM_REMOTE]), u32be(connId)]);
}

export function encodeServerCloseFrame({ connId, reason, message = "" }) {
  return Buffer.concat([Buffer.from([FrameType.CLOSE_FROM_REMOTE]), u32be(connId), Buffer.from([reason]), encodeStringWithLength(message)]);
}

function ensureLength(buf, required, context) {
  if (buf.length < required) throw new Error(`Frame too short for ${context}`);
}

export function decodeClientFrame(buf) {
  const frame = Buffer.isBuffer(buf) ? buf : Buffer.from(buf);
  ensureLength(frame, 5, "header");
  const type = frame.readUInt8(0);
  const connId = frame.readUInt32BE(1);

  if (type === FrameType.CONNECT) {
    ensureLength(frame, 1 + 4 + 1 + 1 + 2, "connect");
    const addrType = frame.readUInt8(5);
    const addrLen = frame.readUInt8(6);
    const addrStart = 7;
    const addrEnd = addrStart + addrLen;
    ensureLength(frame, addrEnd + 2, "connect address");
    const addrBytes = frame.subarray(addrStart, addrEnd);
    const port = frame.readUInt16BE(addrEnd);
    if (port < 1 || port > 65535) throw new Error("Invalid port");

    let host;
    if (addrType === AddrType.HOSTNAME) {
      host = addrBytes.toString("utf8");
      if (!host) throw new Error("Empty hostname");
    } else if (addrType === AddrType.IPV4 || addrType === AddrType.IPV6) {
      const expectedLen = addrType === AddrType.IPV4 ? 4 : 16;
      if (addrLen !== expectedLen) throw new Error("Invalid IP address length");
      host = ipaddr.fromByteArray([...addrBytes]).toString();
    } else {
      throw new Error("Unknown address type");
    }
    return { type: "connect", connId, host, port };
  }

  if (type === FrameType.DATA) {
    return { type: "data", connId, data: frame.subarray(5) };
  }
  if (type === FrameType.END) {
    return { type: "end", connId };
  }
  if (type === FrameType.CLOSE) {
    return { type: "close", connId };
  }

  throw new Error("Unknown frame type");
}

export function decodeServerFrame(buf) {
  const frame = Buffer.isBuffer(buf) ? buf : Buffer.from(buf);
  ensureLength(frame, 5, "header");
  const type = frame.readUInt8(0);
  const connId = frame.readUInt32BE(1);

  if (type === FrameType.OPENED) {
    ensureLength(frame, 1 + 4 + 1 + 2, "opened");
    const status = frame.readUInt8(5);
    const msgLen = frame.readUInt16BE(6);
    ensureLength(frame, 8 + msgLen, "opened msg");
    const message = formatOneLineUtf8(frame.subarray(8, 8 + msgLen).toString("utf8"), MAX_PROTOCOL_MESSAGE_BYTES);
    return { type: "opened", connId, status, message };
  }
  if (type === FrameType.DATA_FROM_REMOTE) {
    return { type: "data", connId, data: frame.subarray(5) };
  }
  if (type === FrameType.END_FROM_REMOTE) {
    return { type: "end", connId };
  }
  if (type === FrameType.CLOSE_FROM_REMOTE) {
    ensureLength(frame, 1 + 4 + 1 + 2, "close");
    const reason = frame.readUInt8(5);
    const msgLen = frame.readUInt16BE(6);
    ensureLength(frame, 8 + msgLen, "close msg");
    const message = formatOneLineUtf8(frame.subarray(8, 8 + msgLen).toString("utf8"), MAX_PROTOCOL_MESSAGE_BYTES);
    return { type: "close", connId, reason, message };
  }
  throw new Error("Unknown frame type");
}

