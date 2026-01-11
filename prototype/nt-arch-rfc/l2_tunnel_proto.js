// Compatibility wrapper for the networking-architecture RFC prototype.
//
// The prototype originally shipped its own handwritten encoder/decoder; we now
// delegate to the canonical TypeScript codec to avoid drift.

import {
  L2_TUNNEL_HEADER_LEN,
  L2_TUNNEL_MAGIC,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_VERSION,
  decodeL2Message,
  encodeError,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "../../web/src/shared/l2TunnelProtocol.ts";

function normalizePayload(payload) {
  if (payload === undefined) return new Uint8Array();
  if (payload instanceof Uint8Array) return payload;
  return Buffer.from(payload);
}

function toBuffer(view) {
  // `ws` accepts Uint8Array, but the prototype historically returned `Buffer`.
  return Buffer.from(view.buffer, view.byteOffset, view.byteLength);
}

function encodeL2Message(type, payload) {
  const body = normalizePayload(payload);

  switch (type) {
    case L2_TUNNEL_TYPE_FRAME:
      return toBuffer(encodeL2Frame(body));
    case L2_TUNNEL_TYPE_PING:
      return toBuffer(encodePing(body));
    case L2_TUNNEL_TYPE_PONG:
      return toBuffer(encodePong(body));
    case L2_TUNNEL_TYPE_ERROR:
      return toBuffer(encodeError(body));
    default:
      throw new RangeError(`unknown L2 tunnel message type: ${type}`);
  }
}

export {
  L2_TUNNEL_HEADER_LEN,
  L2_TUNNEL_MAGIC,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_VERSION,
  decodeL2Message,
  encodeError,
  encodeL2Frame,
  encodeL2Message,
  encodePing,
  encodePong,
};
