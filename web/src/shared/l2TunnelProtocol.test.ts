import { describe, expect, it } from "vitest";

import {
  L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD,
  L2_TUNNEL_MAGIC,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_VERSION,
  decodeL2Message,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "./l2TunnelProtocol";

describe("l2TunnelProtocol", () => {
  it("roundtrips FRAME", () => {
    const payload = Uint8Array.from([0, 1, 2, 3, 4, 5]);
    const encoded = encodeL2Frame(payload);
    const decoded = decodeL2Message(encoded);

    expect(decoded.version).toBe(L2_TUNNEL_VERSION);
    expect(decoded.type).toBe(L2_TUNNEL_TYPE_FRAME);
    expect(decoded.flags).toBe(0);
    expect(Array.from(decoded.payload)).toEqual(Array.from(payload));
  });

  it("roundtrips PING and PONG", () => {
    const payload = Uint8Array.from([9, 8, 7, 6]);

    const ping = decodeL2Message(encodePing(payload));
    expect(ping.type).toBe(L2_TUNNEL_TYPE_PING);
    expect(Array.from(ping.payload)).toEqual(Array.from(payload));

    const pong = decodeL2Message(encodePong(payload));
    expect(pong.type).toBe(L2_TUNNEL_TYPE_PONG);
    expect(Array.from(pong.payload)).toEqual(Array.from(payload));
  });

  it("rejects wrong magic and version", () => {
    const ok = encodeL2Frame(Uint8Array.from([1, 2, 3]));

    const wrongMagic = ok.slice();
    wrongMagic[0] = 0x00;
    expect(() => decodeL2Message(wrongMagic)).toThrow();

    const wrongVersion = ok.slice();
    wrongVersion[1] = 0xff;
    expect(() => decodeL2Message(wrongVersion)).toThrow();
  });

  it("rejects oversized payloads", () => {
    const payload = new Uint8Array(L2_TUNNEL_DEFAULT_MAX_FRAME_PAYLOAD + 1);
    expect(() => encodeL2Frame(payload)).toThrow(RangeError);

    const wire = new Uint8Array(4 + payload.length);
    wire[0] = L2_TUNNEL_MAGIC;
    wire[1] = L2_TUNNEL_VERSION;
    wire[2] = L2_TUNNEL_TYPE_FRAME;
    wire[3] = 0;
    wire.set(payload, 4);
    expect(() => decodeL2Message(wire)).toThrow();
  });
});

