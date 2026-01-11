import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  UDP_RELAY_DEFAULT_MAX_PAYLOAD,
  UDP_RELAY_V1_HEADER_LEN,
  UDP_RELAY_V2_AF_IPV6,
  UDP_RELAY_V2_MAGIC,
  UDP_RELAY_V2_TYPE_DATAGRAM,
  UDP_RELAY_V2_VERSION,
  UdpRelayDecodeError,
  decodeUdpRelayFrame,
  decodeUdpRelayV2Datagram,
  decodeUdpRelayV1Datagram,
  encodeUdpRelayV2Datagram,
  encodeUdpRelayV1Datagram,
} from "../web/src/shared/udpRelayProtocol.ts";

function hexToU8(hex) {
  return new Uint8Array(Buffer.from(hex, "hex"));
}

function loadProtocolVectors() {
  const path = new URL("./protocol-vectors/networking.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8"));
}

const vectors = loadProtocolVectors();

test("udp relay v1: golden vector matches PROTOCOL.md", () => {
  const v = vectors.udpRelay.v1;

  const encoded = encodeUdpRelayV1Datagram({
    guestPort: v.guestPort,
    remoteIpv4: v.remoteIpv4,
    remotePort: v.remotePort,
    payload: new TextEncoder().encode(v.payloadUtf8),
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
    payload: hexToU8(v.payloadHex),
  });

  assert.deepEqual(encoded, hexToU8(v.frameHex));

  const decoded = decodeUdpRelayFrame(encoded);
  assert.equal(decoded.version, 2);
  assert.equal(decoded.addressFamily, 6);
  assert.equal(decoded.guestPort, v.guestPort);
  assert.deepEqual(decoded.remoteIp, remoteIp);
  assert.equal(decoded.remotePort, v.remotePort);
  assert.deepEqual(decoded.payload, hexToU8(v.payloadHex));
});

test("udp relay v1: roundtrip encode/decode", () => {
  const input = {
    guestPort: 12345,
    remoteIpv4: [1, 2, 3, 4],
    remotePort: 443,
    payload: new TextEncoder().encode("hello"),
  };

  const encoded = encodeUdpRelayV1Datagram(input);
  assert.equal(encoded.length, UDP_RELAY_V1_HEADER_LEN + input.payload.length);

  const decoded = decodeUdpRelayV1Datagram(encoded);
  assert.equal(decoded.guestPort, input.guestPort);
  assert.deepEqual(decoded.remoteIpv4, input.remoteIpv4);
  assert.equal(decoded.remotePort, input.remotePort);
  assert.deepEqual(decoded.payload, input.payload);
});

test("udp relay v1: decode rejects frames shorter than header", () => {
  for (let n = 0; n < UDP_RELAY_V1_HEADER_LEN; n++) {
    assert.throws(
      () => decodeUdpRelayV1Datagram(new Uint8Array(n)),
      (err) => err instanceof UdpRelayDecodeError && err.code === "too_short",
    );
  }
});

test("udp relay v2: decode rejects invalid message type", () => {
  const frame = hexToU8(vectors.udpRelay.v2_ipv6.frameHex);
  frame[3] = 0x01; // type must be 0x00
  assert.throws(
    () => decodeUdpRelayV2Datagram(frame),
    (err) => err instanceof UdpRelayDecodeError && err.code === "invalid_v2",
  );
});

test("udp relay v2: decode rejects unknown address family", () => {
  const frame = hexToU8(vectors.udpRelay.v2_ipv6.frameHex);
  frame[2] = 0xff; // unknown AF

  assert.throws(
    () => decodeUdpRelayV2Datagram(frame),
    (err) => err instanceof UdpRelayDecodeError && err.code === "invalid_v2",
  );
});

test("udp relay v2: decode rejects too-short frames", () => {
  assert.throws(
    () => decodeUdpRelayV2Datagram(new Uint8Array([UDP_RELAY_V2_MAGIC, UDP_RELAY_V2_VERSION])),
    (err) => err instanceof UdpRelayDecodeError && err.code === "too_short",
  );
});

test("udp relay v2: encode supports IPv4 and rejects invalid address lengths", () => {
  const payload = new Uint8Array([1, 2, 3]);
  const ipv4 = new Uint8Array([127, 0, 0, 1]);

  const frame = encodeUdpRelayV2Datagram({ guestPort: 1, remoteIp: ipv4, remotePort: 2, payload });
  // v2 header = 4 + 2 + 4 + 2 = 12 bytes
  assert.equal(frame.length, 12 + payload.length);

  assert.throws(
    () => encodeUdpRelayV2Datagram({ guestPort: 1, remoteIp: new Uint8Array([1, 2, 3]), remotePort: 2, payload }),
    /remoteIp.*length/i,
  );
});

test("udp relay v1: max payload enforcement", () => {
  const maxPayload = 3;

  assert.throws(
    () =>
      encodeUdpRelayV1Datagram(
        {
          guestPort: 1,
          remoteIpv4: [127, 0, 0, 1],
          remotePort: 2,
          payload: new Uint8Array([0, 1, 2, 3]),
        },
        { maxPayload },
      ),
    /payload too large/i,
  );

  const frame = new Uint8Array([...hexToU8(vectors.udpRelay.v1.frameHex), 0x00]);

  assert.throws(
    () => decodeUdpRelayV1Datagram(frame, { maxPayload }),
    (err) => err instanceof UdpRelayDecodeError && err.code === "payload_too_large",
  );
});

test("udp relay v1: encode validates ports and IPv4 octets", () => {
  const payload = new Uint8Array([1, 2, 3]);

  assert.throws(
    () =>
      encodeUdpRelayV1Datagram({
        guestPort: -1,
        remoteIpv4: [1, 2, 3, 4],
        remotePort: 1,
        payload,
      }),
    /guestPort/i,
  );

  assert.throws(
    () =>
      encodeUdpRelayV1Datagram({
        guestPort: 0,
        remoteIpv4: [1, 2, 3, 4],
        remotePort: 65536,
        payload,
      }),
    /remotePort/i,
  );

  assert.throws(
    () =>
      encodeUdpRelayV1Datagram({
        guestPort: 0,
        remoteIpv4: [256, 0, 0, 1],
        remotePort: 1,
        payload,
      }),
    /remoteIpv4/i,
  );

  assert.throws(
    () =>
      encodeUdpRelayV1Datagram({
        guestPort: 0,
        remoteIpv4: /** @type {any} */ ([1, 2, 3]),
        remotePort: 1,
        payload,
      }),
    /remoteIpv4/i,
  );
});

test("udp relay v1: default max payload constant is sensible", () => {
  assert.ok(UDP_RELAY_DEFAULT_MAX_PAYLOAD >= 1200);
});
