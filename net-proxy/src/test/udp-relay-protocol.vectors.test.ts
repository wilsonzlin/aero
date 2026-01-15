import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import {
  UDP_RELAY_DEFAULT_MAX_PAYLOAD,
  decodeUdpRelayFrame,
  encodeUdpRelayV1Datagram,
  encodeUdpRelayV2Datagram,
} from "../udpRelayProtocol";

type UdpRelayVector = {
  name: string;
  frame_b64: string;
  version?: 1 | 2;
  guestPort?: number;
  remoteIp?: string;
  remotePort?: number;
  payload_b64?: string;
  expectError?: true;
  errorContains?: string;
};

function parseIpv4(ip: string): [number, number, number, number] {
  const parts = ip.split(".");
  if (parts.length !== 4) throw new Error(`invalid IPv4: ${ip}`);
  const nums = parts.map((p) => Number(p));
  for (const n of nums) {
    if (!Number.isInteger(n) || n < 0 || n > 255) throw new Error(`invalid IPv4: ${ip}`);
  }
  return [nums[0]!, nums[1]!, nums[2]!, nums[3]!];
}

function parseIpv6(ip: string): Uint8Array {
  const parts = ip.split("::");
  if (parts.length > 2) throw new Error(`invalid IPv6: ${ip}`);

  const left = parts[0] ? parts[0].split(":").filter((p) => p.length > 0) : [];
  const right = parts.length === 2 && parts[1] ? parts[1].split(":").filter((p) => p.length > 0) : [];

  let hextets: string[];
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
    const v = Number.parseInt(hextets[i]!, 16);
    if (!Number.isInteger(v) || v < 0 || v > 0xffff) throw new Error(`invalid IPv6: ${ip}`);
    out[i * 2] = (v >>> 8) & 0xff;
    out[i * 2 + 1] = v & 0xff;
  }
  return out;
}

function parseIp(ip: string): Uint8Array {
  if (ip.includes(".")) return new Uint8Array(parseIpv4(ip));
  return parseIpv6(ip);
}

function decodeB64(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, "base64"));
}

const vectorsPath = path.resolve(__dirname, "../../../protocol-vectors/udp-relay.json");
const raw = JSON.parse(fs.readFileSync(vectorsPath, "utf8")) as { schema: number; vectors: UdpRelayVector[] };
assert.equal(raw.schema, 1);

for (const v of raw.vectors) {
  test(`udp relay vectors: ${v.name}`, () => {
    const frame = decodeB64(v.frame_b64);

    if (v.expectError) {
      assert.throws(
        () => decodeUdpRelayFrame(frame),
        (err) => err instanceof Error && (!v.errorContains || err.message.includes(v.errorContains)),
      );
      return;
    }

    assert.ok(v.version);
    assert.ok(v.guestPort !== undefined);
    assert.ok(v.remoteIp);
    assert.ok(v.remotePort !== undefined);
    assert.ok(v.payload_b64 !== undefined);

    const payload = decodeB64(v.payload_b64!);

    const decoded = decodeUdpRelayFrame(frame);
    assert.equal(decoded.version, v.version);
    assert.equal(decoded.guestPort, v.guestPort);
    assert.equal(decoded.remotePort, v.remotePort);
    assert.deepEqual(decoded.payload, payload);

    if (v.version === 1) {
      const ip4 = parseIpv4(v.remoteIp!);
      assert.equal(decoded.version, 1);
      assert.deepEqual(decoded.remoteIpv4, ip4);

      const encoded = encodeUdpRelayV1Datagram({
        guestPort: v.guestPort!,
        remoteIpv4: ip4,
        remotePort: v.remotePort!,
        payload,
      });
      assert.deepEqual(encoded, frame);
    } else {
      const ip = parseIp(v.remoteIp!);
      assert.equal(decoded.version, 2);
      assert.equal(decoded.addressFamily, ip.byteLength === 4 ? 4 : 6);
      assert.deepEqual(decoded.remoteIp, ip);

      const encoded = encodeUdpRelayV2Datagram({
        guestPort: v.guestPort!,
        remoteIp: ip,
        remotePort: v.remotePort!,
        payload,
      });
      assert.deepEqual(encoded, frame);
    }
  });
}

test("udp relay default max payload constant is sensible", () => {
  assert.ok(UDP_RELAY_DEFAULT_MAX_PAYLOAD >= 1200);
});
