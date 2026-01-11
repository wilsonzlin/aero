import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "../src/shared/udpRelayProtocol";

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
  // Minimal RFC 5952-ish parser that supports `::` compression and hex hextets.
  // Good enough for our test vectors.
  if (!ip.includes(":")) throw new Error(`invalid IPv6: ${ip}`);

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
    const h = hextets[i]!;
    const v = Number.parseInt(h, 16);
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

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "protocol-vectors", "udp-relay.json");
}

describe("udp relay protocol vectors", () => {
  const raw = JSON.parse(fs.readFileSync(vectorsPath(), "utf8")) as { schema: number; vectors: UdpRelayVector[] };
  expect(raw.schema).toBe(1);

  for (const v of raw.vectors) {
    it(v.name, () => {
      const frame = decodeB64(v.frame_b64);

      if (v.expectError) {
        let err: unknown;
        try {
          decodeUdpRelayFrame(frame);
        } catch (e) {
          err = e;
        }
        expect(err).toBeInstanceOf(Error);
        if (v.errorContains) expect((err as Error).message).toContain(v.errorContains);
        return;
      }

      expect(v.version).toBeDefined();
      expect(v.guestPort).toBeDefined();
      expect(v.remoteIp).toBeDefined();
      expect(v.remotePort).toBeDefined();
      expect(v.payload_b64).toBeDefined();

      const payload = decodeB64(v.payload_b64!);

      const decoded = decodeUdpRelayFrame(frame);
      expect(decoded.version).toBe(v.version);
      expect(decoded.guestPort).toBe(v.guestPort);
      expect(decoded.remotePort).toBe(v.remotePort);
      expect(Buffer.from(decoded.payload)).toEqual(Buffer.from(payload));

      if (v.version === 1) {
        const ip4 = parseIpv4(v.remoteIp!);
        expect(decoded.version).toBe(1);
        expect(decoded.remoteIpv4).toEqual(ip4);

        const encoded = encodeUdpRelayV1Datagram({
          guestPort: v.guestPort!,
          remoteIpv4: ip4,
          remotePort: v.remotePort!,
          payload,
        });
        expect(Buffer.from(encoded)).toEqual(Buffer.from(frame));
      } else {
        const ip = parseIp(v.remoteIp!);
        expect(decoded.version).toBe(2);
        expect(decoded.addressFamily).toBe(ip.byteLength === 4 ? 4 : 6);
        expect(Buffer.from(decoded.remoteIp)).toEqual(Buffer.from(ip));

        const encoded = encodeUdpRelayV2Datagram({
          guestPort: v.guestPort!,
          remoteIp: ip,
          remotePort: v.remotePort!,
          payload,
        });
        expect(Buffer.from(encoded)).toEqual(Buffer.from(frame));
      }
    });
  }
});

