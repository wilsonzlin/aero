import { describe, expect, it } from "vitest";

import {
  UDP_RELAY_V1_HEADER_LEN,
  UDP_RELAY_V2_IPV4_HEADER_LEN,
  UDP_RELAY_V2_IPV6_HEADER_LEN,
  UDP_RELAY_V2_MAGIC,
  UDP_RELAY_V2_VERSION,
  UdpRelayDecodeError,
  decodeUdpRelayFrame,
  decodeUdpRelayV1Datagram,
  decodeUdpRelayV2Datagram,
  encodeUdpRelayV1Datagram,
  encodeUdpRelayV2Datagram,
} from "./udpRelayProtocol";

describe("udpRelayProtocol", () => {
  it("round-trips v1 datagrams", () => {
    const payload = Uint8Array.of(1, 2, 3);
    const frame = encodeUdpRelayV1Datagram({
      guestPort: 1234,
      remoteIpv4: [192, 0, 2, 1],
      remotePort: 5678,
      payload,
    });

    expect(frame.length).toBe(UDP_RELAY_V1_HEADER_LEN + payload.length);

    const decoded = decodeUdpRelayV1Datagram(frame);
    expect(decoded.guestPort).toBe(1234);
    expect(decoded.remoteIpv4).toEqual([192, 0, 2, 1]);
    expect(decoded.remotePort).toBe(5678);
    expect(Buffer.from(decoded.payload)).toEqual(Buffer.from(payload));

    const decodedAny = decodeUdpRelayFrame(frame);
    expect(decodedAny.version).toBe(1);
  });

  it("round-trips v2 IPv6 datagrams", () => {
    const payload = new TextEncoder().encode("hello v2");
    const remoteIp = Uint8Array.from([
      0x20, 0x01, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
    ]);

    const frame = encodeUdpRelayV2Datagram({
      guestPort: 48879,
      remoteIp,
      remotePort: 51966,
      payload,
    });

    expect(frame[0]).toBe(UDP_RELAY_V2_MAGIC);
    expect(frame[1]).toBe(UDP_RELAY_V2_VERSION);
    expect(frame.length).toBe(UDP_RELAY_V2_IPV6_HEADER_LEN + payload.length);

    const { addressFamily, datagram } = decodeUdpRelayV2Datagram(frame);
    expect(addressFamily).toBe(6);
    expect(datagram.guestPort).toBe(48879);
    expect(Buffer.from(datagram.remoteIp)).toEqual(Buffer.from(remoteIp));
    expect(datagram.remotePort).toBe(51966);
    expect(Buffer.from(datagram.payload)).toEqual(Buffer.from(payload));

    const decodedAny = decodeUdpRelayFrame(frame);
    expect(decodedAny.version).toBe(2);
    if (decodedAny.version !== 2) throw new Error("unreachable");
    expect(decodedAny.addressFamily).toBe(6);
    expect(Buffer.from(decodedAny.remoteIp)).toEqual(Buffer.from(remoteIp));
  });

  it("rejects too-short v2 frames", () => {
    const short = new Uint8Array(UDP_RELAY_V2_IPV4_HEADER_LEN - 1);
    expect(() => decodeUdpRelayV2Datagram(short)).toThrowError(UdpRelayDecodeError);
    try {
      decodeUdpRelayV2Datagram(short);
      throw new Error("expected decode to throw");
    } catch (err) {
      expect(err).toBeInstanceOf(UdpRelayDecodeError);
      expect((err as UdpRelayDecodeError).code).toBe("too_short");
    }
  });

  it("rejects non-v2 prefixes", () => {
    const buf = new Uint8Array(UDP_RELAY_V2_IPV4_HEADER_LEN);
    buf[0] = 0xaa;
    buf[1] = 0xbb;
    expect(() => decodeUdpRelayV2Datagram(buf)).toThrowError(UdpRelayDecodeError);
    try {
      decodeUdpRelayV2Datagram(buf);
      throw new Error("expected decode to throw");
    } catch (err) {
      expect(err).toBeInstanceOf(UdpRelayDecodeError);
      expect((err as UdpRelayDecodeError).code).toBe("invalid_v2");
    }
  });
});

