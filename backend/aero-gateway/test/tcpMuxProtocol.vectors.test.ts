import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "node:test";

import {
  TcpMuxFrameParser,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/protocol/tcpMux.js";

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

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "..", "protocol-vectors", "tcp-mux-v1.json");
}

describe("tcp-mux protocol vectors", () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), "utf8")) as TcpMuxVectorsFile;
  assert.equal(vectors.schema, 1);

  for (const v of vectors.frames) {
    it(`frame/${v.name}`, () => {
      const payload = decodeB64(v.payload_b64);
      const expectedFrame = decodeB64(v.frame_b64);

      const parser = new TcpMuxFrameParser();
      const parsed = parser.push(expectedFrame);
      assert.equal(parsed.length, 1);
      assert.equal(parsed[0]!.msgType, v.msgType);
      assert.equal(parsed[0]!.streamId, v.streamId);
      assert.deepEqual(parsed[0]!.payload, payload);
      assert.doesNotThrow(() => parser.finish());

      const encoded = encodeTcpMuxFrame(v.msgType as any, v.streamId, payload);
      assert.deepEqual(encoded, expectedFrame);
    });
  }

  for (const v of vectors.openPayloads) {
    it(`openPayload/${v.name}`, () => {
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
    it(`closePayload/${v.name}`, () => {
      const expected = decodeB64(v.payload_b64);

      const encoded = encodeTcpMuxClosePayload(v.flags);
      assert.deepEqual(encoded, expected);

      const decoded = decodeTcpMuxClosePayload(encoded);
      assert.equal(decoded.flags, v.flags);
    });
  }

  for (const v of vectors.errorPayloads) {
    it(`errorPayload/${v.name}`, () => {
      const payload = decodeB64(v.payload_b64);

      if ("expectError" in v) {
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
    it(`parserStream/${v.name}`, () => {
      const parser = new TcpMuxFrameParser();
      const parsed: Array<{ msgType: number; streamId: number; payload: Buffer }> = [];
      for (const chunkB64 of v.chunks_b64) {
        parsed.push(...parser.push(decodeB64(chunkB64)));
      }

      if ("expectError" in v) {
        assert.throws(
          () => parser.finish(),
          (err) => err instanceof Error && err.message.includes(v.errorContains),
        );
        return;
      }

      const ok = v;
      assert.equal(parsed.length, ok.expectFrames.length);
      for (let i = 0; i < ok.expectFrames.length; i++) {
        const expected = ok.expectFrames[i]!;
        const got = parsed[i]!;
        assert.equal(got.msgType, expected.msgType);
        assert.equal(got.streamId, expected.streamId);
        assert.deepEqual(got.payload, decodeB64(expected.payload_b64));
      }

      assert.doesNotThrow(() => parser.finish());
    });
  }
});
