export const UDP_RELAY_V1_HEADER_LEN = 8;

export const UDP_RELAY_V2_MAGIC = 0xa2;
export const UDP_RELAY_V2_VERSION = 0x02;
export const UDP_RELAY_V2_AF_IPV4 = 0x04;
export const UDP_RELAY_V2_AF_IPV6 = 0x06;

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
  readonly code: 'too_short' | 'payload_too_large' | 'invalid_v2';

  constructor(code: UdpRelayDecodeError['code'], message: string) {
    super(message);
    this.code = code;
  }
}

function assertUint16(name: string, value: number): void {
  if (!Number.isInteger(value) || value < 0 || value > 0xffff) {
    throw new RangeError(`${name} must be an integer in [0, 65535] (got ${value})`);
  }
}

function assertIpv4(name: string, value: readonly number[]): asserts value is [number, number, number, number] {
  if (value.length !== 4) {
    throw new RangeError(`${name} must have length 4 (got ${value.length})`);
  }
  for (let i = 0; i < 4; i++) {
    const octet = value[i];
    if (!Number.isInteger(octet) || octet < 0 || octet > 255) {
      throw new RangeError(`${name}[${i}] must be an integer in [0, 255] (got ${octet})`);
    }
  }
}

export function encodeUdpRelayV1Datagram(
  d: UdpRelayV1Datagram,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  if (maxPayload < 0) {
    throw new RangeError(`maxPayload must be >= 0 (got ${maxPayload})`);
  }
  assertUint16('guestPort', d.guestPort);
  assertIpv4('remoteIpv4', d.remoteIpv4);
  assertUint16('remotePort', d.remotePort);

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

export type UdpRelayV2Datagram = {
  guestPort: number;
  // IPv4 (length 4) or IPv6 (length 16) in network byte order.
  remoteIp: Uint8Array;
  remotePort: number;
  payload: Uint8Array;
};

export type UdpRelayFrame =
  | ({ version: 1 } & UdpRelayV1Datagram)
  | { version: 2; addressFamily: 4 | 6; guestPort: number; remoteIp: Uint8Array; remotePort: number; payload: Uint8Array };

function isV2Prefix(frame: Uint8Array): boolean {
  return frame.length >= 2 && frame[0] === UDP_RELAY_V2_MAGIC && frame[1] === UDP_RELAY_V2_VERSION;
}

export function encodeUdpRelayV2Datagram(
  d: UdpRelayV2Datagram,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): Uint8Array {
  if (maxPayload < 0) {
    throw new RangeError(`maxPayload must be >= 0 (got ${maxPayload})`);
  }
  assertUint16('guestPort', d.guestPort);
  assertUint16('remotePort', d.remotePort);
  if (d.payload.length > maxPayload) {
    throw new RangeError(`payload too large: ${d.payload.length} > ${maxPayload}`);
  }

  const ipLen = d.remoteIp.length;
  let af: number;
  if (ipLen === 4) {
    af = UDP_RELAY_V2_AF_IPV4;
  } else if (ipLen === 16) {
    af = UDP_RELAY_V2_AF_IPV6;
  } else {
    throw new RangeError(`remoteIp must have length 4 (IPv4) or 16 (IPv6) (got ${ipLen})`);
  }

  const headerLen = 4 + 2 + ipLen + 2;
  const out = new Uint8Array(headerLen + d.payload.length);
  out[0] = UDP_RELAY_V2_MAGIC;
  out[1] = UDP_RELAY_V2_VERSION;
  out[2] = af;
  out[3] = 0x00;
  out[4] = (d.guestPort >>> 8) & 0xff;
  out[5] = d.guestPort & 0xff;
  out.set(d.remoteIp, 6);
  const portOff = 6 + ipLen;
  out[portOff] = (d.remotePort >>> 8) & 0xff;
  out[portOff + 1] = d.remotePort & 0xff;
  out.set(d.payload, headerLen);
  return out;
}

export function decodeUdpRelayV2Datagram(
  frame: Uint8Array,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): { addressFamily: 4 | 6; datagram: UdpRelayV2Datagram } {
  if (maxPayload < 0) {
    throw new RangeError(`maxPayload must be >= 0 (got ${maxPayload})`);
  }
  if (frame.length < 12) {
    throw new UdpRelayDecodeError('too_short', `frame too short: ${frame.length} < 12`);
  }
  if (!isV2Prefix(frame)) {
    throw new UdpRelayDecodeError('invalid_v2', 'missing v2 prefix');
  }

  const af = frame[2];
  if (frame[3] !== 0x00) {
    throw new UdpRelayDecodeError('invalid_v2', `v2 reserved byte must be 0x00 (got 0x${frame[3].toString(16)})`);
  }

  const ipLen = af === UDP_RELAY_V2_AF_IPV4 ? 4 : af === UDP_RELAY_V2_AF_IPV6 ? 16 : 0;
  if (ipLen === 0) {
    throw new UdpRelayDecodeError('invalid_v2', `unknown address family: 0x${af.toString(16)}`);
  }

  const minLen = 4 + 2 + ipLen + 2;
  if (frame.length < minLen) {
    throw new UdpRelayDecodeError('too_short', `frame too short: ${frame.length} < ${minLen}`);
  }

  const payloadLen = frame.length - minLen;
  if (payloadLen > maxPayload) {
    throw new UdpRelayDecodeError('payload_too_large', `payload too large: ${payloadLen} > ${maxPayload}`);
  }

  const guestPort = (frame[4] << 8) | frame[5];
  const remoteIp = frame.subarray(6, 6 + ipLen);
  const remotePortOff = 6 + ipLen;
  const remotePort = (frame[remotePortOff] << 8) | frame[remotePortOff + 1];
  const payload = frame.subarray(minLen);

  const addressFamily = ipLen === 4 ? 4 : 6;

  return {
    addressFamily,
    datagram: { guestPort, remoteIp, remotePort, payload },
  };
}

export function decodeUdpRelayFrame(
  frame: Uint8Array,
  { maxPayload = UDP_RELAY_DEFAULT_MAX_PAYLOAD }: { maxPayload?: number } = {},
): UdpRelayFrame {
  if (isV2Prefix(frame)) {
    const { addressFamily, datagram } = decodeUdpRelayV2Datagram(frame, { maxPayload });
    return {
      version: 2,
      addressFamily,
      guestPort: datagram.guestPort,
      remoteIp: datagram.remoteIp,
      remotePort: datagram.remotePort,
      payload: datagram.payload,
    };
  }
  return { version: 1, ...decodeUdpRelayV1Datagram(frame, { maxPayload }) };
}
