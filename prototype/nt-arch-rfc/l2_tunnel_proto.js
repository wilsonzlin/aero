// Minimal implementation of the Aero L2 tunnel framing used by the
// networking-architecture RFC prototype.
//
// Keep in sync with:
// - docs/l2-tunnel-protocol.md
// - web/src/shared/l2TunnelProtocol.ts

const L2_TUNNEL_SUBPROTOCOL = "aero-l2-tunnel-v1";

const L2_TUNNEL_MAGIC = 0xa2;
const L2_TUNNEL_VERSION = 0x03;
const L2_TUNNEL_HEADER_LEN = 4;

const L2_TUNNEL_TYPE_FRAME = 0x00;
const L2_TUNNEL_TYPE_PING = 0x01;
const L2_TUNNEL_TYPE_PONG = 0x02;
const L2_TUNNEL_TYPE_ERROR = 0x7f;

function encodeL2Message(type, payload) {
  const body = Buffer.isBuffer(payload) ? payload : Buffer.from(payload);
  const out = Buffer.allocUnsafe(L2_TUNNEL_HEADER_LEN + body.length);
  out[0] = L2_TUNNEL_MAGIC;
  out[1] = L2_TUNNEL_VERSION;
  out[2] = type & 0xff;
  out[3] = 0;
  body.copy(out, L2_TUNNEL_HEADER_LEN);
  return out;
}

function decodeL2Message(buf) {
  if (buf.length < L2_TUNNEL_HEADER_LEN) throw new Error("l2 message too short");
  if (buf[0] !== L2_TUNNEL_MAGIC) throw new Error("l2 invalid magic");
  if (buf[1] !== L2_TUNNEL_VERSION) throw new Error("l2 unsupported version");
  return {
    type: buf[2],
    flags: buf[3],
    payload: buf.subarray(L2_TUNNEL_HEADER_LEN),
  };
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
  encodeL2Message,
};

