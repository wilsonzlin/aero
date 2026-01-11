import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import {
  TcpMuxFrameParser,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/protocol.js";

function decodeB64(b64) {
  return Buffer.from(b64, "base64");
}

function vectorsPath() {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "..", "protocol-vectors", "tcp-mux-v1.json");
}

const vectors = JSON.parse(fs.readFileSync(vectorsPath(), "utf8"));
assert.equal(vectors.schema, 1);

for (const v of vectors.frames) {
  test(`tcp-mux frame vectors: ${v.name}`, () => {
    const payload = decodeB64(v.payload_b64);
    const expectedFrame = decodeB64(v.frame_b64);

    const parser = new TcpMuxFrameParser();
    const parsed = parser.push(expectedFrame);
    assert.equal(parsed.length, 1);
    assert.equal(parsed[0].msgType, v.msgType);
    assert.equal(parsed[0].streamId, v.streamId);
    assert.deepEqual(parsed[0].payload, payload);
    assert.doesNotThrow(() => parser.finish());

    const encoded = encodeTcpMuxFrame(v.msgType, v.streamId, payload);
    assert.deepEqual(encoded, expectedFrame);
  });
}

for (const v of vectors.openPayloads) {
  test(`tcp-mux OPEN payload vectors: ${v.name}`, () => {
    const expected = decodeB64(v.payload_b64);

    const encoded = encodeTcpMuxOpenPayload({ host: v.host, port: v.port, metadata: v.metadata });
    assert.deepEqual(encoded, expected);

    const decoded = decodeTcpMuxOpenPayload(encoded);
    assert.equal(decoded.host, v.host);
    assert.equal(decoded.port, v.port);
    assert.equal(decoded.metadata, v.metadata);
  });
}

for (const v of vectors.closePayloads) {
  test(`tcp-mux CLOSE payload vectors: ${v.name}`, () => {
    const expected = decodeB64(v.payload_b64);

    const encoded = encodeTcpMuxClosePayload(v.flags);
    assert.deepEqual(encoded, expected);

    const decoded = decodeTcpMuxClosePayload(encoded);
    assert.equal(decoded.flags, v.flags);
  });
}

for (const v of vectors.errorPayloads) {
  test(`tcp-mux ERROR payload vectors: ${v.name}`, () => {
    const payload = decodeB64(v.payload_b64);

    if (v.expectError) {
      assert.throws(
        () => decodeTcpMuxErrorPayload(payload),
        (err) => err instanceof Error && err.message.includes(v.errorContains),
      );
      return;
    }

    const encoded = encodeTcpMuxErrorPayload(v.code, v.message);
    assert.deepEqual(encoded, payload);

    const decoded = decodeTcpMuxErrorPayload(payload);
    assert.deepEqual(decoded, { code: v.code, message: v.message });
  });
}

for (const v of vectors.parserStreams) {
  test(`tcp-mux parser stream vectors: ${v.name}`, () => {
    const parser = new TcpMuxFrameParser();
    const parsed = [];
    for (const chunkB64 of v.chunks_b64) {
      parsed.push(...parser.push(decodeB64(chunkB64)));
    }

    if (v.expectError) {
      assert.throws(
        () => parser.finish(),
        (err) => err instanceof Error && err.message.includes(v.errorContains),
      );
      return;
    }

    assert.equal(parsed.length, v.expectFrames.length);
    for (let i = 0; i < v.expectFrames.length; i++) {
      const exp = v.expectFrames[i];
      const got = parsed[i];
      assert.equal(got.msgType, exp.msgType);
      assert.equal(got.streamId, exp.streamId);
      assert.deepEqual(got.payload, decodeB64(exp.payload_b64));
    }

    assert.doesNotThrow(() => parser.finish());
  });
}

