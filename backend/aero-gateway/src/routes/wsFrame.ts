export type WsFrame = Readonly<{
  opcode: number;
  fin: boolean;
  payload: Buffer;
}>;

export type TryReadWsFrameResult = Readonly<{ frame: WsFrame; remaining: Buffer }>;

export function encodeWsClosePayload(code: number): Buffer {
  // `code` is a 16-bit unsigned int in network byte order.
  const c = Number.isInteger(code) ? code : 1002;
  const clamped = Math.max(0, Math.min(0xffff, c));
  return Buffer.from([(clamped >> 8) & 0xff, clamped & 0xff]);
}

export function tryReadWsFrame(buffer: Buffer, maxPayloadBytes: number): TryReadWsFrameResult | null {
  if (buffer.length < 2) return null;

  const first = buffer[0];
  const second = buffer[1];

  const fin = (first & 0x80) !== 0;
  const rsv = first & 0x70;
  const opcode = first & 0x0f;

  const masked = (second & 0x80) !== 0;
  let length = second & 0x7f;
  let offset = 2;

  // Server-side parser: clients must mask frames (RFC 6455) and RSV bits must be 0 unless
  // extensions are negotiated (we don't support extensions on raw-upgrade sockets here).
  if (rsv !== 0 || !masked) {
    return { frame: { fin: true, opcode: 0x8, payload: encodeWsClosePayload(1002) }, remaining: Buffer.alloc(0) };
  }

  if (length === 126) {
    if (buffer.length < offset + 2) return null;
    length = buffer.readUInt16BE(offset);
    offset += 2;
  } else if (length === 127) {
    if (buffer.length < offset + 8) return null;
    const hi = buffer.readUInt32BE(offset);
    const lo = buffer.readUInt32BE(offset + 4);
    offset += 8;
    const combined = hi * 2 ** 32 + lo;
    if (!Number.isSafeInteger(combined)) {
      // Too large for a JS buffer anyway; treat as protocol error.
      return { frame: { fin: true, opcode: 0x8, payload: encodeWsClosePayload(1002) }, remaining: Buffer.alloc(0) };
    }
    length = combined;
  }

  if (length > maxPayloadBytes) {
    // Close immediately without buffering untrusted payloads.
    return { frame: { fin: true, opcode: 0x8, payload: encodeWsClosePayload(1009) }, remaining: Buffer.alloc(0) };
  }

  let maskKey: Buffer | null = null;
  if (buffer.length < offset + 4) return null;
  maskKey = buffer.subarray(offset, offset + 4);
  offset += 4;

  if (buffer.length < offset + length) return null;
  let payload = buffer.subarray(offset, offset + length);
  const remaining = buffer.subarray(offset + length);

  payload = unmask(payload, maskKey);

  // If we consumed the entire buffer, avoid keeping a reference to the backing allocation
  // via an empty subarray view.
  const remainingTrimmed = remaining.length === 0 ? Buffer.alloc(0) : remaining;
  return { frame: { fin, opcode, payload }, remaining: remainingTrimmed };
}

function unmask(payload: Buffer, key: Buffer): Buffer {
  const out = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; i++) {
    out[i] = payload[i] ^ key[i % 4];
  }
  return out;
}

export function concat2(a: Buffer, b: Buffer): Buffer {
  const out = Buffer.allocUnsafe(a.length + b.length);
  a.copy(out, 0);
  b.copy(out, a.length);
  return out;
}

export function encodeWsFrame(opcode: number, payload: Buffer): Buffer {
  const finOpcode = 0x80 | (opcode & 0x0f);
  const length = payload.length;

  if (length < 126) {
    const out = Buffer.allocUnsafe(2 + length);
    out[0] = finOpcode;
    out[1] = length;
    payload.copy(out, 2);
    return out;
  }

  if (length < 65536) {
    const out = Buffer.allocUnsafe(4 + length);
    out[0] = finOpcode;
    out[1] = 126;
    out.writeUInt16BE(length, 2);
    payload.copy(out, 4);
    return out;
  }

  const out = Buffer.allocUnsafe(10 + length);
  out[0] = finOpcode;
  out[1] = 127;
  out.writeUInt32BE(0, 2);
  out.writeUInt32BE(length, 6);
  payload.copy(out, 10);
  return out;
}

