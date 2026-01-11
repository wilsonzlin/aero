import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  L2_TUNNEL_MAGIC,
  L2_TUNNEL_VERSION,
  decodeL2Message,
  encodeL2Frame,
  encodePing,
  encodePong,
} from "../src/shared/l2TunnelProtocol.ts";

type L2TunnelVector = {
  name: string;
  type: number;
  flags: number;
  payload_b64: string;
  frame_b64: string;
  code?: number;
  message?: string;
};

type L2TunnelVectorsFile = {
  schema: number;
  magic: number;
  version: number;
  flags: number;
  vectors: L2TunnelVector[];
};

function loadVectors(): L2TunnelVectorsFile {
  const path = new URL("../../protocol-vectors/l2-tunnel-v1.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8")) as L2TunnelVectorsFile;
}

function b64ToBytes(b64: string): Uint8Array {
  return Buffer.from(b64, "base64");
}

const vectors = loadVectors();

test("l2 tunnel matches golden protocol vectors", () => {
  assert.equal(vectors.schema, 1);
  assert.equal(vectors.magic, L2_TUNNEL_MAGIC);
  assert.equal(vectors.version, L2_TUNNEL_VERSION);
  assert.equal(vectors.flags, 0);

  function getVector(type: number): L2TunnelVector {
    const v = vectors.vectors.find((x) => x.type === type);
    assert.ok(v, `missing l2 tunnel vector for type=0x${type.toString(16)}`);
    return v;
  }

  // FRAME (0x00)
  {
    const v = getVector(0x00);
    const payload = b64ToBytes(v.payload_b64);
    const frame = b64ToBytes(v.frame_b64);

    const encoded = encodeL2Frame(payload);
    assert.deepEqual(Buffer.from(encoded), Buffer.from(frame));

    const decoded = decodeL2Message(frame);
    assert.equal(decoded.version, vectors.version);
    assert.equal(decoded.type, v.type);
    assert.equal(decoded.flags, v.flags);
    assert.deepEqual(Buffer.from(decoded.payload), Buffer.from(payload));

    // Roundtrip: decode -> encode should preserve bytes exactly.
    const reencoded = encodeL2Frame(decoded.payload);
    assert.deepEqual(Buffer.from(reencoded), Buffer.from(frame));
  }

  // PING (0x01)
  {
    const v = getVector(0x01);
    const payload = b64ToBytes(v.payload_b64);
    const frame = b64ToBytes(v.frame_b64);

    const encoded = encodePing(payload);
    assert.deepEqual(Buffer.from(encoded), Buffer.from(frame));

    const decoded = decodeL2Message(frame);
    assert.equal(decoded.version, vectors.version);
    assert.equal(decoded.type, v.type);
    assert.equal(decoded.flags, v.flags);
    assert.deepEqual(Buffer.from(decoded.payload), Buffer.from(payload));

    // Roundtrip: decode -> encode should preserve bytes exactly.
    const reencoded = encodePing(decoded.payload);
    assert.deepEqual(Buffer.from(reencoded), Buffer.from(frame));
  }

  // PONG (0x02)
  {
    const v = getVector(0x02);
    const payload = b64ToBytes(v.payload_b64);
    const frame = b64ToBytes(v.frame_b64);

    const encoded = encodePong(payload);
    assert.deepEqual(Buffer.from(encoded), Buffer.from(frame));

    const decoded = decodeL2Message(frame);
    assert.equal(decoded.version, vectors.version);
    assert.equal(decoded.type, v.type);
    assert.equal(decoded.flags, v.flags);
    assert.deepEqual(Buffer.from(decoded.payload), Buffer.from(payload));

    // Roundtrip: decode -> encode should preserve bytes exactly.
    const reencoded = encodePong(decoded.payload);
    assert.deepEqual(Buffer.from(reencoded), Buffer.from(frame));
  }

  // ERROR (0x7F) - structured payload
  {
    const v = getVector(0x7f);
    const payload = b64ToBytes(v.payload_b64);
    const frame = b64ToBytes(v.frame_b64);

    const decoded = decodeL2Message(frame);
    assert.equal(decoded.version, vectors.version);
    assert.equal(decoded.type, v.type);
    assert.equal(decoded.flags, v.flags);
    assert.deepEqual(Buffer.from(decoded.payload), Buffer.from(payload));

    // Verify the structured encoding: code (u16 BE) | msg_len (u16 BE) | msg (UTF-8)
    assert.equal(typeof v.code, "number");
    assert.equal(typeof v.message, "string");
    const msg = Buffer.from(v.message!, "utf8");
    const header = Buffer.from([
      (v.code! >>> 8) & 0xff,
      v.code! & 0xff,
      (msg.length >>> 8) & 0xff,
      msg.length & 0xff,
    ]);
    const expected = Buffer.concat([header, msg]);
    assert.deepEqual(Buffer.from(payload), expected);
  }
});
