import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  UDP_RELAY_V1_HEADER_LEN,
  decodeUdpRelayFrame,
  encodeUdpRelayV1Datagram,
  encodeUdpRelayV2Datagram,
  UdpRelayDecodeError,
} from "../src/shared/udpRelayProtocol.ts";

type NetworkingVectors = {
  udpRelay: {
    v1: {
      guestPort: number;
      remoteIpv4: [number, number, number, number];
      remotePort: number;
      payloadUtf8: string;
      frameHex: string;
    };
    v2_ipv6: {
      guestPort: number;
      addressFamily: 6;
      remoteIpHex: string;
      remotePort: number;
      payloadHex: string;
      frameHex: string;
    };
  };
};

function loadVectors(): NetworkingVectors {
  const path = new URL("../../tests/protocol-vectors/networking.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8")) as NetworkingVectors;
}

function hexToBytes(hex: string): Uint8Array {
  return Buffer.from(hex, "hex");
}

const vectors = loadVectors();

test("udp relay v1 matches golden vector", () => {
  const v = vectors.udpRelay.v1;
  const payload = Buffer.from(v.payloadUtf8, "utf8");

  const encoded = encodeUdpRelayV1Datagram({
    guestPort: v.guestPort,
    remoteIpv4: v.remoteIpv4,
    remotePort: v.remotePort,
    payload,
  });
  assert.equal(Buffer.from(encoded).toString("hex"), v.frameHex);

  const decoded = decodeUdpRelayFrame(hexToBytes(v.frameHex));
  assert.equal(decoded.version, 1);
  assert.equal(decoded.guestPort, v.guestPort);
  assert.deepEqual(decoded.remoteIpv4, v.remoteIpv4);
  assert.equal(decoded.remotePort, v.remotePort);
  assert.equal(Buffer.from(decoded.payload).toString("utf8"), v.payloadUtf8);

  // Roundtrip: decode -> re-encode should preserve bytes exactly.
  const reencoded = encodeUdpRelayV1Datagram({
    guestPort: decoded.guestPort,
    remoteIpv4: decoded.remoteIpv4,
    remotePort: decoded.remotePort,
    payload: decoded.payload,
  });
  assert.equal(Buffer.from(reencoded).toString("hex"), v.frameHex);
});

test("udp relay v2 (IPv6) matches golden vector", () => {
  const v = vectors.udpRelay.v2_ipv6;
  const remoteIp = hexToBytes(v.remoteIpHex);
  const payload = hexToBytes(v.payloadHex);

  const encoded = encodeUdpRelayV2Datagram({
    guestPort: v.guestPort,
    remoteIp,
    remotePort: v.remotePort,
    payload,
  });
  assert.equal(Buffer.from(encoded).toString("hex"), v.frameHex);

  const decoded = decodeUdpRelayFrame(hexToBytes(v.frameHex));
  assert.equal(decoded.version, 2);
  assert.equal(decoded.addressFamily, v.addressFamily);
  assert.equal(decoded.guestPort, v.guestPort);
  assert.equal(Buffer.from(decoded.remoteIp).toString("hex"), v.remoteIpHex);
  assert.equal(decoded.remotePort, v.remotePort);
  assert.equal(Buffer.from(decoded.payload).toString("hex"), v.payloadHex);

  // Roundtrip: decode -> re-encode should preserve bytes exactly.
  const reencoded = encodeUdpRelayV2Datagram({
    guestPort: decoded.guestPort,
    remoteIp: decoded.remoteIp,
    remotePort: decoded.remotePort,
    payload: decoded.payload,
  });
  assert.equal(Buffer.from(reencoded).toString("hex"), v.frameHex);
});

test("udp relay decoder rejects malformed frames (too short / invalid v2 type / payload too large)", () => {
  const v1 = hexToBytes(vectors.udpRelay.v1.frameHex);
  assert.throws(
    () => decodeUdpRelayFrame(v1.subarray(0, UDP_RELAY_V1_HEADER_LEN - 1)),
    (err) => {
      assert.ok(err instanceof UdpRelayDecodeError);
      assert.equal(err.code, "too_short");
      return true;
    },
  );

  const v2 = Buffer.from(vectors.udpRelay.v2_ipv6.frameHex, "hex");
  assert.throws(
    () => decodeUdpRelayFrame(v2.subarray(0, 11)),
    (err) => {
      assert.ok(err instanceof UdpRelayDecodeError);
      assert.equal(err.code, "too_short");
      return true;
    },
  );

  const badType = Buffer.from(v2);
  badType[3] = 0x01;
  assert.throws(
    () => decodeUdpRelayFrame(badType),
    (err) => {
      assert.ok(err instanceof UdpRelayDecodeError);
      assert.equal(err.code, "invalid_v2");
      return true;
    },
  );

  assert.throws(
    () => decodeUdpRelayFrame(v1, { maxPayload: 2 }),
    (err) => {
      assert.ok(err instanceof UdpRelayDecodeError);
      assert.equal(err.code, "payload_too_large");
      return true;
    },
  );
});

