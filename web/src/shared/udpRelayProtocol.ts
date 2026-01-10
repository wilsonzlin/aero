export const UDP_RELAY_V1_HEADER_LEN = 8;

// Keep in sync with proxy/webrtc-udp-relay/internal/udpproto.DefaultMaxPayload and
// proxy/webrtc-udp-relay/PROTOCOL.md.
export const UDP_RELAY_DEFAULT_MAX_PAYLOAD = 1200;

export type UdpRelayV1Datagram = {
  // Guest-side UDP port.
  //
  // Outbound (guest -> remote): source port.
  // Inbound (remote -> guest): destination port.
  guestPort: number;

  // Remote IPv4 address.
  //
  // Outbound: destination IP.
  // Inbound: source IP.
  remoteIpv4: [number, number, number, number];

  // Remote UDP port.
  //
  // Outbound: destination port.
  // Inbound: source port.
  remotePort: number;

  // UDP payload bytes.
  payload: Uint8Array;
};

export class UdpRelayDecodeError extends Error {
  readonly code: 'too_short' | 'payload_too_large';

  constructor(code: UdpRelayDecodeError['code'], message: string) {
    super(message);
    this.code = code;
  }
}

export function encodeUdpRelayV1Datagram(
  d: UdpRelayV1Datagram,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  if (maxPayload < 0) {
    throw new RangeError(`maxPayload must be >= 0 (got ${maxPayload})`);
  }
  if (d.payload.length > maxPayload) {
    throw new RangeError(`payload too large: ${d.payload.length} > ${maxPayload}`);
  }

  const out = new Uint8Array(UDP_RELAY_V1_HEADER_LEN + d.payload.length);
  out[0] = (d.guestPort >>> 8) & 0xff;
  out[1] = d.guestPort & 0xff;
  out[2] = d.remoteIpv4[0] & 0xff;
  out[3] = d.remoteIpv4[1] & 0xff;
  out[4] = d.remoteIpv4[2] & 0xff;
  out[5] = d.remoteIpv4[3] & 0xff;
  out[6] = (d.remotePort >>> 8) & 0xff;
  out[7] = d.remotePort & 0xff;
  out.set(d.payload, UDP_RELAY_V1_HEADER_LEN);
  return out;
}

export function decodeUdpRelayV1Datagram(
  frame: Uint8Array,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): UdpRelayV1Datagram {
  if (maxPayload < 0) {
    throw new RangeError(`maxPayload must be >= 0 (got ${maxPayload})`);
  }
  if (frame.length < UDP_RELAY_V1_HEADER_LEN) {
    throw new UdpRelayDecodeError(
      'too_short',
      `frame too short: ${frame.length} < ${UDP_RELAY_V1_HEADER_LEN}`,
    );
  }

  const payloadLen = frame.length - UDP_RELAY_V1_HEADER_LEN;
  if (payloadLen > maxPayload) {
    throw new UdpRelayDecodeError('payload_too_large', `payload too large: ${payloadLen} > ${maxPayload}`);
  }

  return {
    guestPort: (frame[0] << 8) | frame[1],
    remoteIpv4: [frame[2], frame[3], frame[4], frame[5]],
    remotePort: (frame[6] << 8) | frame[7],
    payload: frame.subarray(UDP_RELAY_V1_HEADER_LEN),
  };
}

