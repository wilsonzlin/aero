export const L2_TUNNEL_MAGIC = 0xa2;

// Version is intentionally distinct from the WebRTC UDP relay v2 prefix (0x02).
export const L2_TUNNEL_VERSION = 0x03;

export const L2_TUNNEL_HEADER_LEN = 4;

export const L2_TUNNEL_TYPE_FRAME = 0x00;
export const L2_TUNNEL_TYPE_PING = 0x01;
export const L2_TUNNEL_TYPE_PONG = 0x02;
export const L2_TUNNEL_TYPE_ERROR = 0x7f;

// Keep in sync with docs/l2-tunnel-protocol.md.
export const L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD = 2048;
export const L2_TUNNEL_DEFAULT_MAX_CONTROL_PAYLOAD = 256;

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

