import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import {
  UDP_RELAY_DEFAULT_MAX_PAYLOAD,
  UDP_RELAY_V1_HEADER_LEN,
  UdpRelayDecodeError,
  decodeUdpRelayFrame,
  decodeUdpRelayV2Datagram,
  decodeUdpRelayV1Datagram,
  encodeUdpRelayV2Datagram,
  encodeUdpRelayV1Datagram
} from "../udpRelayProtocol";

type NetworkingVectors = {
  schemaVersion: number;
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
  const vectorsPath = path.join(__dirname, "../../../tests/protocol-vectors/networking.json");
  return JSON.parse(fs.readFileSync(vectorsPath, "utf8")) as NetworkingVectors;
}

function hexToU8(hex: string): Uint8Array {
  return new Uint8Array(Buffer.from(hex, "hex"));
}

const vectors = loadVectors();

test("udp relay v1: golden vector matches PROTOCOL.md", () => {
  const v = vectors.udpRelay.v1;
  const encoded = encodeUdpRelayV1Datagram({
    guestPort: v.guestPort,
    remoteIpv4: v.remoteIpv4,
    remotePort: v.remotePort,
    payload: new TextEncoder().encode(v.payloadUtf8)
  });
  assert.deepEqual(encoded, hexToU8(v.frameHex));

  const decoded = decodeUdpRelayV1Datagram(encoded);
  assert.equal(decoded.guestPort, v.guestPort);
  assert.deepEqual(decoded.remoteIpv4, v.remoteIpv4);
  assert.equal(decoded.remotePort, v.remotePort);
  assert.deepEqual(decoded.payload, new TextEncoder().encode(v.payloadUtf8));
});

test("udp relay v2: ipv6 golden vector matches PROTOCOL.md", () => {
  const v = vectors.udpRelay.v2_ipv6;
  const remoteIp = hexToU8(v.remoteIpHex);

  const encoded = encodeUdpRelayV2Datagram({
    guestPort: v.guestPort,
    remoteIp,
    remotePort: v.remotePort,
    payload: hexToU8(v.payloadHex)
  });

  assert.deepEqual(encoded, hexToU8(v.frameHex));

  const decoded = decodeUdpRelayFrame(encoded);
  assert.equal(decoded.version, 2);
  assert.equal(decoded.addressFamily, v.addressFamily);
  assert.equal(decoded.guestPort, v.guestPort);
  assert.deepEqual(decoded.remoteIp, remoteIp);
  assert.equal(decoded.remotePort, v.remotePort);
  assert.deepEqual(decoded.payload, hexToU8(v.payloadHex));
});

test("udp relay v1: decode rejects frames shorter than header", () => {
  for (let n = 0; n < UDP_RELAY_V1_HEADER_LEN; n++) {
    assert.throws(
      () => decodeUdpRelayV1Datagram(new Uint8Array(n)),
      (err) => err instanceof UdpRelayDecodeError && err.code === "too_short"
    );
  }
});

test("udp relay v2: decode rejects invalid message type", () => {
  const frame = hexToU8(vectors.udpRelay.v2_ipv6.frameHex);
  frame[3] = 0x01; // type must be 0x00
  assert.throws(
    () => decodeUdpRelayV2Datagram(frame),
    (err) => err instanceof UdpRelayDecodeError && err.code === "invalid_v2"
  );
});

test("udp relay default max payload constant is sensible", () => {
  assert.ok(UDP_RELAY_DEFAULT_MAX_PAYLOAD >= 1200);
});
