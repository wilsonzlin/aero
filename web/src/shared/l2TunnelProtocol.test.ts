import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_VERSION,
  L2TunnelDecodeError,
  decodeStructuredErrorPayload,
  decodeL2Message,
  encodeError,
  encodeStructuredErrorPayload,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "./l2TunnelProtocol";

type L2ValidVector = {
  name: string;
  msgType: number;
  flags: number;
  payloadHex: string;
  wireHex: string;
  structured?: {
    code: number;
    message: string;
  };
};

type L2InvalidVector = {
  name: string;
  wireHex: string;
  errorCode: L2TunnelDecodeError["code"];
};

type RootVectors = {
  version: number;
  // Key matches `L2_TUNNEL_SUBPROTOCOL`.
  "aero-l2-tunnel-v1": {
    valid: L2ValidVector[];
    invalid: L2InvalidVector[];
  };
};

function decodeHex(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) throw new Error(`hex string length must be even, got ${hex.length}`);
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    const byte = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    if (!Number.isFinite(byte)) throw new Error(`invalid hex at ${i}`);
    out[i] = byte;
  }
  return out;
}

function encodeHex(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

function loadVectors(): RootVectors {
  const here = dirname(fileURLToPath(import.meta.url));
  const vectorPath = join(here, "../../../crates/conformance/test-vectors/aero-vectors-v1.json");
  return JSON.parse(readFileSync(vectorPath, "utf8")) as RootVectors;
}

describe("l2TunnelProtocol", () => {
  it("matches the canonical l2 tunnel framing vectors", () => {
    const vectors = loadVectors();
    expect(vectors.version).toBe(1);

    for (const v of vectors[L2_TUNNEL_SUBPROTOCOL].valid) {
      const payload = decodeHex(v.payloadHex);
      const expectedWire = decodeHex(v.wireHex);

      const decoded = decodeL2Message(expectedWire);
      expect(decoded.version).toBe(L2_TUNNEL_VERSION);
      expect(decoded.type).toBe(v.msgType);
      expect(decoded.flags).toBe(v.flags);
      expect(encodeHex(decoded.payload)).toBe(v.payloadHex);
      if (v.structured) {
        const expectedPayload = encodeStructuredErrorPayload(v.structured.code, v.structured.message, Number.MAX_SAFE_INTEGER);
        expect(Buffer.from(decoded.payload)).toEqual(Buffer.from(expectedPayload));
        expect(decodeStructuredErrorPayload(decoded.payload)).toEqual(v.structured);
      }

      let encoded: Uint8Array;
      switch (v.msgType) {
        case L2_TUNNEL_TYPE_FRAME:
          encoded = encodeL2Frame(payload);
          break;
        case L2_TUNNEL_TYPE_PING:
          encoded = encodePing(payload);
          break;
        case L2_TUNNEL_TYPE_PONG:
          encoded = encodePong(payload);
          break;
        case L2_TUNNEL_TYPE_ERROR:
          encoded = encodeError(payload);
          break;
        default:
          throw new Error(`unsupported msgType in vectors: ${v.msgType}`);
      }

      expect(encodeHex(encoded)).toBe(v.wireHex);
    }

    for (const v of vectors[L2_TUNNEL_SUBPROTOCOL].invalid) {
      const wire = decodeHex(v.wireHex);
      try {
        decodeL2Message(wire);
        throw new Error(`expected decode to throw (${v.name})`);
      } catch (err) {
        expect(err).toBeInstanceOf(L2TunnelDecodeError);
        expect((err as L2TunnelDecodeError).code).toBe(v.errorCode);
      }
    }
  });
});
