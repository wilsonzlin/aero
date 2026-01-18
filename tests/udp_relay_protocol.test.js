import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import { fileURLToPath } from "node:url";

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

function decodeB64(b64) {
  return new Uint8Array(Buffer.from(b64, "base64"));
}

function parseIpv4(ip) {
  const parts = ip.split(".");
  if (parts.length !== 4) throw new Error(`invalid IPv4: ${ip}`);
  const nums = parts.map((p) => Number(p));
  for (const n of nums) {
    if (!Number.isInteger(n) || n < 0 || n > 255) throw new Error(`invalid IPv4: ${ip}`);
  }
  return [nums[0], nums[1], nums[2], nums[3]];
}

function parseIpv6(ip) {
  const parts = ip.split("::");
  if (parts.length > 2) throw new Error(`invalid IPv6: ${ip}`);

  const left = parts[0] ? parts[0].split(":").filter((p) => p.length > 0) : [];
  const right = parts.length === 2 && parts[1] ? parts[1].split(":").filter((p) => p.length > 0) : [];

  let hextets;
  if (parts.length === 2) {
    const missing = 8 - (left.length + right.length);
    if (missing < 0) throw new Error(`invalid IPv6: ${ip}`);
    hextets = [...left, ...Array.from({ length: missing }, () => "0"), ...right];
  } else {
    hextets = left;
  }

  if (hextets.length !== 8) throw new Error(`invalid IPv6: ${ip}`);

  const out = new Uint8Array(16);
  for (let i = 0; i < 8; i++) {
    const v = Number.parseInt(hextets[i], 16);
    if (!Number.isInteger(v) || v < 0 || v > 0xffff) throw new Error(`invalid IPv6: ${ip}`);
    out[i * 2] = (v >>> 8) & 0xff;
    out[i * 2 + 1] = v & 0xff;
  }
  return out;
}

function parseIp(ip) {
  if (ip.includes(".")) return new Uint8Array(parseIpv4(ip));
  return parseIpv6(ip);
}

function loadUdpRelayVectors() {
  const vectorsPath = fileURLToPath(new URL("../protocol-vectors/udp-relay.json", import.meta.url));
  return JSON.parse(fs.readFileSync(vectorsPath, "utf8"));
}

const udpRelayVectors = loadUdpRelayVectors();
assert.equal(udpRelayVectors.schema, 1);

function getVector(name) {
  const v = udpRelayVectors.vectors.find((x) => x.name === name);
  assert.ok(v, `missing udp relay vector: ${name}`);
  return v;
}

test("udp relay protocol vectors", () => {
  for (const v of udpRelayVectors.vectors) {
    const frame = decodeB64(v.frame_b64);

    if (v.expectError) {
      assert.throws(
        () => decodeUdpRelayFrame(frame),
        (err) => err instanceof Error && (!v.errorContains || err.message.includes(v.errorContains)),
      );
      continue;
    }

    const payload = decodeB64(v.payload_b64);
    const decoded = decodeUdpRelayFrame(frame);

    assert.equal(decoded.version, v.version);
    assert.equal(decoded.guestPort, v.guestPort);
    assert.equal(decoded.remotePort, v.remotePort);
    assert.deepEqual(decoded.payload, payload);

    if (v.version === 1) {
      const ip4 = parseIpv4(v.remoteIp);
      assert.deepEqual(decoded.remoteIpv4, ip4);
      const encoded = encodeUdpRelayV1Datagram({
        guestPort: v.guestPort,
        remoteIpv4: ip4,
        remotePort: v.remotePort,
        payload,
      });
      assert.deepEqual(encoded, frame);
    } else {
      const ip = parseIp(v.remoteIp);
      assert.equal(decoded.addressFamily, ip.byteLength === 4 ? 4 : 6);
      assert.deepEqual(decoded.remoteIp, ip);
      const encoded = encodeUdpRelayV2Datagram({
        guestPort: v.guestPort,
        remoteIp: ip,
        remotePort: v.remotePort,
        payload,
      });
      assert.deepEqual(encoded, frame);
    }
  }
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
  const frame = decodeB64(getVector("err_v2_unsupported_message_type").frame_b64);
  assert.throws(
    () => decodeUdpRelayV2Datagram(frame),
    (err) => err instanceof UdpRelayDecodeError && err.code === "invalid_v2",
  );
});

test("udp relay v2: decode rejects unknown address family", () => {
  const frame = decodeB64(getVector("err_v2_unknown_address_family").frame_b64);
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

  const base = decodeB64(getVector("v1_ipv4_example_abc").frame_b64);
  const frame = new Uint8Array([...base, 0x00]);

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

// Sanity: ensure we keep the UDP relay spec constant in-sync when editing vectors.
test("udp relay vectors use the protocol TEST-NET/Documentation ranges", () => {
  const v1 = getVector("v1_ipv4_example_abc");
  assert.equal(v1.remoteIp, "192.0.2.1");

  const v2 = getVector("v2_ipv6_example_010203");
  assert.equal(v2.remoteIp, "2001:db8::1");
  assert.equal(v2.version, 2);
  assert.equal(v2.addressFamily, undefined);

  // Validate the address family constant used in the wire format is still the same.
  // (The vector frames themselves are authoritative; this just makes intent obvious.)
  assert.equal(UDP_RELAY_V2_AF_IPV6, 0x06);
  assert.equal(UDP_RELAY_V2_TYPE_DATAGRAM, 0x00);
});
