import { sanitizeOneLine, truncateUtf8 } from "./text";

export const TCP_MUX_SUBPROTOCOL = "aero-tcp-mux-v1";

export const TCP_MUX_HEADER_BYTES = 9;

// Defensive caps: OPEN payload strings are attacker-controlled and should never be large.
// Hostnames are <=253 chars on the wire; allow some slack for IPv6 literals and future extensions.
export const MAX_TCP_MUX_OPEN_HOST_BYTES = 1024;
export const MAX_TCP_MUX_OPEN_METADATA_BYTES = 4 * 1024;
export const MAX_TCP_MUX_ERROR_MESSAGE_BYTES = 1024;

export enum TcpMuxMsgType {
  OPEN = 1,
  DATA = 2,
  CLOSE = 3,
  ERROR = 4,
  PING = 5,
  PONG = 6
}

export enum TcpMuxCloseFlags {
  FIN = 0x01,
  RST = 0x02
}

export enum TcpMuxErrorCode {
  POLICY_DENIED = 1,
  DIAL_FAILED = 2,
  PROTOCOL_ERROR = 3,
  UNKNOWN_STREAM = 4,
  STREAM_LIMIT_EXCEEDED = 5,
  STREAM_BUFFER_OVERFLOW = 6
}

const utf8DecoderFatal = new TextDecoder("utf-8", { fatal: true });

function decodeUtf8Exact(bytes: Buffer, context: string): string {
  try {
    return utf8DecoderFatal.decode(bytes);
  } catch {
    throw new Error(`${context} is not valid UTF-8`);
  }
}

function hasControlOrWhitespace(value: string): boolean {
  for (const ch of value) {
    const code = ch.codePointAt(0) ?? 0;
    const forbidden =
      code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
    if (forbidden || /\s/u.test(ch)) return true;
  }
  return false;
}

export type TcpMuxFrame = {
  msgType: TcpMuxMsgType;
  streamId: number;
  payload: Buffer;
};

export function encodeTcpMuxFrame(msgType: TcpMuxMsgType, streamId: number, payload?: Buffer): Buffer {
  const payloadBuf = payload ?? Buffer.alloc(0);
  const buf = Buffer.allocUnsafe(TCP_MUX_HEADER_BYTES + payloadBuf.length);
  buf.writeUInt8(msgType, 0);
  buf.writeUInt32BE(streamId >>> 0, 1);
  buf.writeUInt32BE(payloadBuf.length >>> 0, 5);
  payloadBuf.copy(buf, TCP_MUX_HEADER_BYTES);
  return buf;
}

export class TcpMuxFrameParser {
  private buffer: Buffer = Buffer.alloc(0);
  private readonly maxFramePayloadBytes: number;

  constructor(maxFramePayloadBytes = 16 * 1024 * 1024) {
    if (!Number.isInteger(maxFramePayloadBytes) || maxFramePayloadBytes < 0) {
      throw new Error(`Invalid maxFramePayloadBytes: ${maxFramePayloadBytes}`);
    }
    this.maxFramePayloadBytes = maxFramePayloadBytes;
  }

  push(chunk: Buffer): TcpMuxFrame[] {
    if (chunk.length === 0) return [];
    this.buffer = this.buffer.length === 0 ? chunk : Buffer.concat([this.buffer, chunk]);

    const frames: TcpMuxFrame[] = [];

    while (this.buffer.length >= TCP_MUX_HEADER_BYTES) {
      const msgType = this.buffer.readUInt8(0) as TcpMuxMsgType;
      const streamId = this.buffer.readUInt32BE(1);
      const length = this.buffer.readUInt32BE(5);

      if (length > this.maxFramePayloadBytes) {
        throw new Error(`Frame payload length ${length} exceeds max ${this.maxFramePayloadBytes}`);
      }

      const frameTotalBytes = TCP_MUX_HEADER_BYTES + length;
      if (this.buffer.length < frameTotalBytes) break;

      const payload = this.buffer.subarray(TCP_MUX_HEADER_BYTES, frameTotalBytes);
      frames.push({ msgType, streamId, payload });

      // Avoid retaining a potentially large backing allocation when fully consumed.
      this.buffer = frameTotalBytes === this.buffer.length ? Buffer.alloc(0) : this.buffer.subarray(frameTotalBytes);
    }

    return frames;
  }

  pendingBytes(): number {
    return this.buffer.length;
  }

  finish(): void {
    if (this.buffer.length === 0) return;
    throw new Error(`truncated tcp-mux frame stream (${this.buffer.length} pending bytes)`);
  }
}

export type TcpMuxOpenPayload = {
  host: string;
  port: number;
  metadata?: string;
};

export function encodeTcpMuxOpenPayload(payload: TcpMuxOpenPayload): Buffer {
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

export function decodeTcpMuxOpenPayload(buf: Buffer): TcpMuxOpenPayload {
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
  const host = decodeUtf8Exact(buf.subarray(offset, offset + hostLen), "host");
  if (!host) throw new Error("host is empty");
  if (hasControlOrWhitespace(host)) throw new Error("invalid host");
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
  const metadata = metadataLen > 0 ? decodeUtf8Exact(buf.subarray(offset, offset + metadataLen), "metadata") : undefined;
  offset += metadataLen;
  if (offset !== buf.length) {
    throw new Error("OPEN payload has trailing bytes");
  }
  return { host, port, metadata };
}

export function encodeTcpMuxClosePayload(flags: number): Buffer {
  const buf = Buffer.allocUnsafe(1);
  buf.writeUInt8(flags & 0xff, 0);
  return buf;
}

export function decodeTcpMuxClosePayload(buf: Buffer): { flags: number } {
  if (buf.length !== 1) {
    throw new Error("CLOSE payload must be exactly 1 byte");
  }
  return { flags: buf.readUInt8(0) };
}

export function encodeTcpMuxErrorPayload(code: TcpMuxErrorCode | number, message: string): Buffer {
  const safeMessage = truncateUtf8(sanitizeOneLine(message), MAX_TCP_MUX_ERROR_MESSAGE_BYTES);
  const messageBytes = Buffer.from(safeMessage, "utf8");
  const buf = Buffer.allocUnsafe(2 + 2 + messageBytes.length);
  buf.writeUInt16BE(code & 0xffff, 0);
  buf.writeUInt16BE(messageBytes.length, 2);
  messageBytes.copy(buf, 4);
  return buf;
}

export function decodeTcpMuxErrorPayload(buf: Buffer): { code: number; message: string } {
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
  const message = buf.subarray(4, 4 + messageLen).toString("utf8");
  return { code, message: truncateUtf8(sanitizeOneLine(message), MAX_TCP_MUX_ERROR_MESSAGE_BYTES) };
}
