export const L2_TUNNEL_MAGIC = 0xa2;

// Version is intentionally distinct from the WebRTC UDP relay v2 prefix (0x02).
export const L2_TUNNEL_VERSION = 0x03;

// Keep in sync with docs/l2-tunnel-protocol.md.
export const L2_TUNNEL_SUBPROTOCOL = "aero-l2-tunnel-v1";

// Optional auth token WebSocket subprotocol prefix (see docs/l2-tunnel-protocol.md).
//
// Clients MAY offer an additional `Sec-WebSocket-Protocol` entry
// `aero-l2-token.<token>` alongside `aero-l2-tunnel-v1`. The negotiated
// subprotocol must still be `aero-l2-tunnel-v1`; the token entry is used only
// for authentication.
export const L2_TUNNEL_TOKEN_SUBPROTOCOL_PREFIX = "aero-l2-token.";

export const L2_TUNNEL_HEADER_LEN = 4;

export const L2_TUNNEL_TYPE_FRAME = 0x00;
export const L2_TUNNEL_TYPE_PING = 0x01;
export const L2_TUNNEL_TYPE_PONG = 0x02;
export const L2_TUNNEL_TYPE_ERROR = 0x7f;

// Keep in sync with docs/l2-tunnel-protocol.md.
export const L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD = 2048;
export const L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD = 256;

// Header bytes for the structured ERROR payload encoding:
//   code (u16 BE) | msg_len (u16 BE)
export const L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN = 4;

// Structured `ERROR` payload codes (see `docs/l2-tunnel-protocol.md`).
export const L2_TUNNEL_ERROR_CODE_PROTOCOL_ERROR = 1;
export const L2_TUNNEL_ERROR_CODE_AUTH_REQUIRED = 2;
export const L2_TUNNEL_ERROR_CODE_AUTH_INVALID = 3;
export const L2_TUNNEL_ERROR_CODE_ORIGIN_MISSING = 4;
export const L2_TUNNEL_ERROR_CODE_ORIGIN_DENIED = 5;
export const L2_TUNNEL_ERROR_CODE_QUOTA_BYTES = 6;
export const L2_TUNNEL_ERROR_CODE_QUOTA_FPS = 7;
export const L2_TUNNEL_ERROR_CODE_QUOTA_CONNECTIONS = 8;
export const L2_TUNNEL_ERROR_CODE_BACKPRESSURE = 9;

export type L2TunnelMessage = {
  version: number;
  type: number;
  flags: number;
  payload: Uint8Array;
};

export class L2TunnelDecodeError extends Error {
  readonly code: "too_short" | "invalid_magic" | "unsupported_version" | "payload_too_large";

  constructor(code: L2TunnelDecodeError["code"], message: string) {
    super(message);
    this.code = code;
  }
}

function assertNonNegative(name: string, value: number): void {
  if (!Number.isFinite(value) || value < 0) {
    throw new RangeError(`${name} must be >= 0 (got ${value})`);
  }
}

type StructuredErrorPayload = {
  code: number;
  message: string;
};

const structuredErrorTextEncoder = new TextEncoder();
const structuredErrorTextDecoder = new TextDecoder("utf-8", { fatal: true });

/**
 * Encode a structured `ERROR` payload:
 *
 * ```text
 * code (u16 BE) | msg_len (u16 BE) | msg (msg_len bytes, UTF-8)
 * ```
 *
 * This is used as the payload bytes for an `L2_TUNNEL_TYPE_ERROR` message (see
 * `docs/l2-tunnel-protocol.md`).
 *
 * The returned payload is truncated as needed to fit within `maxPayloadBytes`.
 */
export function encodeStructuredErrorPayload(code: number, message: string, maxPayloadBytes: number): Uint8Array {
  if (!Number.isInteger(code) || code < 0 || code > 0xffff) {
    throw new RangeError(`code must be a u16 (0..=65535), got ${code}`);
  }
  assertNonNegative("maxPayloadBytes", maxPayloadBytes);
  if (maxPayloadBytes < L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN) {
    return new Uint8Array();
  }

  const maxMsgLen = Math.min(maxPayloadBytes - L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN, 0xffff);

  let msgBytes = structuredErrorTextEncoder.encode(message);
  if (msgBytes.length > maxMsgLen) {
    // Truncate to fit and respect UTF-8 boundaries.
    let end = maxMsgLen;
    while (end > 0) {
      try {
        structuredErrorTextDecoder.decode(msgBytes.subarray(0, end));
        break;
      } catch {
        end -= 1;
      }
    }
    msgBytes = msgBytes.subarray(0, end);
  }

  const out = new Uint8Array(L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + msgBytes.length);
  const dv = new DataView(out.buffer, out.byteOffset, out.byteLength);
  dv.setUint16(0, code, false);
  dv.setUint16(2, msgBytes.length, false);
  out.set(msgBytes, L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN);
  return out;
}

/**
 * Attempt to decode a structured `ERROR` payload (see `encodeStructuredErrorPayload`).
 *
 * Returns `{ code, message }` only if the payload matches the exact structured encoding.
 */
export function decodeStructuredErrorPayload(payload: Uint8Array): StructuredErrorPayload | null {
  if (payload.byteLength < L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN) return null;
  const dv = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
  const code = dv.getUint16(0, false);
  const msgLen = dv.getUint16(2, false);
  if (payload.byteLength !== L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN + msgLen) return null;
  try {
    const msgBytes = payload.subarray(L2_TUNNEL_ERROR_STRUCTURED_HEADER_LEN);
    const message = structuredErrorTextDecoder.decode(msgBytes);
    return { code, message };
  } catch {
    return null;
  }
}

function encodeMessage(type: number, payload: Uint8Array, maxPayload: number): Uint8Array {
  assertNonNegative("maxPayload", maxPayload);
  if (payload.length > maxPayload) {
    throw new RangeError(`payload too large: ${payload.length} > ${maxPayload}`);
  }

  const out = new Uint8Array(L2_TUNNEL_HEADER_LEN + payload.length);
  out[0] = L2_TUNNEL_MAGIC;
  out[1] = L2_TUNNEL_VERSION;
  out[2] = type & 0xff;
  out[3] = 0; // flags (reserved)
  out.set(payload, L2_TUNNEL_HEADER_LEN);
  return out;
}

export function encodeL2Frame(
  payload: Uint8Array,
  { maxPayload = L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  return encodeMessage(L2_TUNNEL_TYPE_FRAME, payload, maxPayload);
}

export function encodePing(
  payload: Uint8Array = new Uint8Array(),
  { maxPayload = L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  return encodeMessage(L2_TUNNEL_TYPE_PING, payload, maxPayload);
}

export function encodePong(
  payload: Uint8Array = new Uint8Array(),
  { maxPayload = L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  return encodeMessage(L2_TUNNEL_TYPE_PONG, payload, maxPayload);
}

export function encodeError(
  payload: Uint8Array = new Uint8Array(),
  { maxPayload = L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  return encodeMessage(L2_TUNNEL_TYPE_ERROR, payload, maxPayload);
}

export function decodeL2Message(
  buf: Uint8Array,
  {
    maxFramePayload = L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
    maxControlPayload = L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD,
  }: { maxFramePayload?: number; maxControlPayload?: number } = {},
): L2TunnelMessage {
  assertNonNegative("maxFramePayload", maxFramePayload);
  assertNonNegative("maxControlPayload", maxControlPayload);

  if (buf.length < L2_TUNNEL_HEADER_LEN) {
    throw new L2TunnelDecodeError("too_short", `message too short: ${buf.length} < ${L2_TUNNEL_HEADER_LEN}`);
  }
  if (buf[0] !== L2_TUNNEL_MAGIC) {
    throw new L2TunnelDecodeError("invalid_magic", `invalid magic: 0x${buf[0].toString(16)}`);
  }
  if (buf[1] !== L2_TUNNEL_VERSION) {
    throw new L2TunnelDecodeError("unsupported_version", `unsupported version: 0x${buf[1].toString(16)}`);
  }

  const version = buf[1];
  const type = buf[2];
  const flags = buf[3];
  const payload = buf.subarray(L2_TUNNEL_HEADER_LEN);

  const maxPayload = type === L2_TUNNEL_TYPE_FRAME ? maxFramePayload : maxControlPayload;
  if (payload.length > maxPayload) {
    throw new L2TunnelDecodeError("payload_too_large", `payload too large: ${payload.length} > ${maxPayload}`);
  }

  return { version, type, flags, payload };
}
