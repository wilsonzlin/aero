import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import {
  TcpMuxMsgType,
  TcpMuxFrameParser,
  MAX_TCP_MUX_OPEN_HOST_BYTES,
  MAX_TCP_MUX_OPEN_METADATA_BYTES,
  MAX_TCP_MUX_ERROR_MESSAGE_BYTES,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../tcpMuxProtocol";

type TcpMuxVectorsFile = {
  schema: number;
  frames: Array<{ name: string; msgType: number; streamId: number; payload_b64: string; frame_b64: string }>;
  openPayloads: Array<{ name: string; host: string; port: number; metadata?: string; payload_b64: string }>;
  closePayloads: Array<{ name: string; flags: number; payload_b64: string }>;
  errorPayloads: Array<
    | { name: string; payload_b64: string; code: number; message: string }
    | { name: string; payload_b64: string; expectError: true; errorContains: string }
  >;
  parserStreams: Array<
    | {
        name: string;
        chunks_b64: string[];
        expectFrames: Array<{ msgType: number; streamId: number; payload_b64: string }>;
      }
    | { name: string; chunks_b64: string[]; expectError: true; errorContains: string }
  >;
};

function decodeB64(b64: string): Buffer {
  return Buffer.from(b64, "base64");
}

const vectorsPath = path.resolve(__dirname, "../../../protocol-vectors/tcp-mux-v1.json");
const vectors = JSON.parse(fs.readFileSync(vectorsPath, "utf8")) as TcpMuxVectorsFile;
assert.equal(vectors.schema, 1);

for (const v of vectors.frames) {
  test(`tcp-mux frame vectors: ${v.name}`, () => {
    const payload = decodeB64(v.payload_b64);
    const expectedFrame = decodeB64(v.frame_b64);

    const parser = new TcpMuxFrameParser();
    const parsed = parser.push(expectedFrame);
    assert.equal(parsed.length, 1);
    assert.equal(parsed[0]!.msgType, v.msgType);
    assert.equal(parsed[0]!.streamId, v.streamId);
    assert.deepEqual(parsed[0]!.payload, payload);
    assert.doesNotThrow(() => parser.finish());

    const encoded = encodeTcpMuxFrame(v.msgType as TcpMuxMsgType, v.streamId, payload);
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
  if ("expectError" in v) {
    const errVector = v;
    test(`tcp-mux ERROR payload vectors: ${v.name}`, () => {
      const payload = decodeB64(errVector.payload_b64);
      assert.throws(
        () => decodeTcpMuxErrorPayload(payload),
        (err) => err instanceof Error && err.message.includes(errVector.errorContains),
      );
    });
  } else {
    const okVector = v;
    test(`tcp-mux ERROR payload vectors: ${v.name}`, () => {
      const payload = decodeB64(okVector.payload_b64);

      const encoded = encodeTcpMuxErrorPayload(okVector.code, okVector.message);
      assert.deepEqual(encoded, payload);

      const decoded = decodeTcpMuxErrorPayload(payload);
      assert.deepEqual(decoded, { code: okVector.code, message: okVector.message });
    });
  }
}

for (const v of vectors.parserStreams) {
  if ("expectError" in v) {
    const errVector = v;
    test(`tcp-mux parser stream vectors: ${v.name}`, () => {
      const parser = new TcpMuxFrameParser();
      for (const chunkB64 of errVector.chunks_b64) {
        parser.push(decodeB64(chunkB64));
      }

      assert.throws(
        () => parser.finish(),
        (err) => err instanceof Error && err.message.includes(errVector.errorContains),
      );
    });
  } else {
    const okVector = v;
    test(`tcp-mux parser stream vectors: ${v.name}`, () => {
      const parser = new TcpMuxFrameParser();
      const parsed: Array<{ msgType: number; streamId: number; payload: Buffer }> = [];
      for (const chunkB64 of okVector.chunks_b64) {
        parsed.push(...parser.push(decodeB64(chunkB64)));
      }

      assert.equal(parsed.length, okVector.expectFrames.length);
      for (let i = 0; i < okVector.expectFrames.length; i++) {
        const exp = okVector.expectFrames[i]!;
        const got = parsed[i]!;
        assert.equal(got.msgType, exp.msgType);
        assert.equal(got.streamId, exp.streamId);
        assert.deepEqual(got.payload, decodeB64(exp.payload_b64));
      }

      assert.doesNotThrow(() => parser.finish());
    });
  }
}

test("tcp-mux OPEN payload limits: rejects oversized host", () => {
  const hostBytes = Buffer.alloc(MAX_TCP_MUX_OPEN_HOST_BYTES + 1, 0x61);
  const payload = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2);
  let off = 0;
  payload.writeUInt16BE(hostBytes.length, off);
  off += 2;
  hostBytes.copy(payload, off);
  off += hostBytes.length;
  payload.writeUInt16BE(80, off);
  off += 2;
  payload.writeUInt16BE(0, off); // metadata_len

  assert.throws(
    () => decodeTcpMuxOpenPayload(payload),
    (err) => err instanceof Error && err.message.includes("host too long"),
  );
});

test("tcp-mux OPEN payload limits: rejects oversized metadata", () => {
  const hostBytes = Buffer.from("a", "utf8");
  const metadataBytes = Buffer.alloc(MAX_TCP_MUX_OPEN_METADATA_BYTES + 1, 0x62);
  const payload = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2 + metadataBytes.length);
  let off = 0;
  payload.writeUInt16BE(hostBytes.length, off);
  off += 2;
  hostBytes.copy(payload, off);
  off += hostBytes.length;
  payload.writeUInt16BE(80, off);
  off += 2;
  payload.writeUInt16BE(metadataBytes.length, off);
  off += 2;
  metadataBytes.copy(payload, off);

  assert.throws(
    () => decodeTcpMuxOpenPayload(payload),
    (err) => err instanceof Error && err.message.includes("metadata too long"),
  );
});

test("tcp-mux ERROR payload limits: rejects oversized message", () => {
  const messageBytes = Buffer.alloc(MAX_TCP_MUX_ERROR_MESSAGE_BYTES + 1, 0x61);
  const payload = Buffer.allocUnsafe(2 + 2 + messageBytes.length);
  payload.writeUInt16BE(1, 0); // code
  payload.writeUInt16BE(messageBytes.length, 2);
  messageBytes.copy(payload, 4);

  assert.throws(
    () => decodeTcpMuxErrorPayload(payload),
    (err) => err instanceof Error && err.message.includes("error message too long"),
  );
});
