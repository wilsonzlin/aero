import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  L2_TUNNEL_SUBPROTOCOL,
  L2_TUNNEL_TYPE_ERROR,
  L2_TUNNEL_TYPE_FRAME,
  L2_TUNNEL_TYPE_PING,
  L2_TUNNEL_TYPE_PONG,
  L2_TUNNEL_VERSION,
  L2TunnelDecodeError,
  decodeStructuredErrorPayload,
  decodeL2Message,
  encodeError,
  encodeStructuredErrorPayload,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "../src/shared/l2TunnelProtocol.ts";

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

type VectorsFile = {
  version: number;
  // Key matches `L2_TUNNEL_SUBPROTOCOL`.
  "aero-l2-tunnel-v1": {
    valid: L2ValidVector[];
    invalid: L2InvalidVector[];
  };
};

function loadVectors(): VectorsFile {
  const path = new URL("../../crates/conformance/test-vectors/aero-vectors-v1.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8")) as VectorsFile;
}

function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 !== 0) throw new Error(`hex length must be even, got ${hex.length}`);
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    const byte = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
    if (!Number.isFinite(byte)) throw new Error(`invalid hex at byte ${i}`);
    out[i] = byte;
  }
  return out;
}

const vectors = loadVectors();

test("l2 tunnel matches canonical conformance vectors", () => {
  assert.equal(vectors.version, 1);

  for (const v of vectors[L2_TUNNEL_SUBPROTOCOL].valid) {
    const payload = hexToBytes(v.payloadHex);
    const wire = hexToBytes(v.wireHex);

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

    assert.deepEqual(Buffer.from(encoded), Buffer.from(wire), `encode ${v.name}`);

    const decoded = decodeL2Message(wire);
    assert.equal(decoded.version, L2_TUNNEL_VERSION, `decode ${v.name}`);
    assert.equal(decoded.type, v.msgType, `decode ${v.name}`);
    assert.equal(decoded.flags, v.flags, `decode ${v.name}`);
    assert.deepEqual(Buffer.from(decoded.payload), Buffer.from(payload), `decode ${v.name}`);

    if (v.structured) {
      const expectedPayload = encodeStructuredErrorPayload(
        v.structured.code,
        v.structured.message,
        Number.MAX_SAFE_INTEGER,
      );
      assert.deepEqual(Buffer.from(payload), Buffer.from(expectedPayload), `structured ERROR ${v.name}`);

      const decoded = decodeStructuredErrorPayload(payload);
      assert.deepEqual(decoded, v.structured, `decode structured ERROR ${v.name}`);
    }
  }

  for (const v of vectors[L2_TUNNEL_SUBPROTOCOL].invalid) {
    const wire = hexToBytes(v.wireHex);
    try {
      decodeL2Message(wire);
      throw new Error(`expected decode to throw (${v.name})`);
    } catch (err) {
      assert.ok(err instanceof L2TunnelDecodeError, `expected L2TunnelDecodeError (${v.name})`);
      assert.equal(err.code, v.errorCode, `decode errorCode (${v.name})`);
    }
  }
});
