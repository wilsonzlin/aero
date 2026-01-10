import test from "node:test";
import assert from "node:assert/strict";

import {
  UDP_RELAY_DEFAULT_MAX_PAYLOAD,
  UDP_RELAY_V1_HEADER_LEN,
  UdpRelayDecodeError,
  decodeUdpRelayV1Datagram,
  encodeUdpRelayV1Datagram,
} from "../web/src/shared/udpRelayProtocol.ts";

test("udp relay v1: golden vector matches PROTOCOL.md", () => {
  const encoded = encodeUdpRelayV1Datagram({
    guestPort: 10000,
    remoteIpv4: [192, 0, 2, 1],
    remotePort: 53,
    payload: new Uint8Array([0x61, 0x62, 0x63]),
  });

  const expected = new Uint8Array([0x27, 0x10, 0xc0, 0x00, 0x02, 0x01, 0x00, 0x35, 0x61, 0x62, 0x63]);
  assert.deepEqual(encoded, expected);

  const decoded = decodeUdpRelayV1Datagram(encoded);
  assert.equal(decoded.guestPort, 10000);
  assert.deepEqual(decoded.remoteIpv4, [192, 0, 2, 1]);
  assert.equal(decoded.remotePort, 53);
  assert.deepEqual(decoded.payload, new Uint8Array([0x61, 0x62, 0x63]));
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

  const frame = new Uint8Array([
    0,
    1, // guest_port
    127,
    0,
    0,
    1, // remote_ipv4
    0,
    2, // remote_port
    0,
    1,
    2,
    3, // payload (4 bytes)
  ]);

  assert.throws(
    () => decodeUdpRelayV1Datagram(frame, { maxPayload }),
    (err) => err instanceof UdpRelayDecodeError && err.code === "payload_too_large",
  );
});

test("udp relay v1: default max payload constant is sensible", () => {
  assert.ok(UDP_RELAY_DEFAULT_MAX_PAYLOAD >= 1200);
});

