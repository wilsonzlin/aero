import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, it } from "node:test";

import {
  MAX_TCP_MUX_ERROR_MESSAGE_BYTES,
  MAX_TCP_MUX_OPEN_HOST_BYTES,
  MAX_TCP_MUX_OPEN_METADATA_BYTES,
  TcpMuxMsgType,
  type TcpMuxMsgType,
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

      const encoded = encodeTcpMuxFrame(v.msgType as TcpMuxMsgType, v.streamId, payload);
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

  it("openPayload rejects oversized host", () => {
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

  it("openPayload rejects oversized metadata", () => {
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

  it("openPayload rejects invalid UTF-8 and whitespace/control in host", () => {
    const hostWithSpace = Buffer.from("a b", "utf8");
    const payloadSpace = Buffer.allocUnsafe(2 + hostWithSpace.length + 2 + 2);
    let off = 0;
    payloadSpace.writeUInt16BE(hostWithSpace.length, off);
    off += 2;
    hostWithSpace.copy(payloadSpace, off);
    off += hostWithSpace.length;
    payloadSpace.writeUInt16BE(80, off);
    off += 2;
    payloadSpace.writeUInt16BE(0, off); // metadata_len
    assert.throws(
      () => decodeTcpMuxOpenPayload(payloadSpace),
      (err) => err instanceof Error && err.message.toLowerCase().includes("invalid host"),
    );

    const invalidUtf8 = Buffer.from([0xc0, 0xaf]);
    const payloadInvalidUtf8 = Buffer.allocUnsafe(2 + invalidUtf8.length + 2 + 2);
    off = 0;
    payloadInvalidUtf8.writeUInt16BE(invalidUtf8.length, off);
    off += 2;
    invalidUtf8.copy(payloadInvalidUtf8, off);
    off += invalidUtf8.length;
    payloadInvalidUtf8.writeUInt16BE(80, off);
    off += 2;
    payloadInvalidUtf8.writeUInt16BE(0, off); // metadata_len
    assert.throws(
      () => decodeTcpMuxOpenPayload(payloadInvalidUtf8),
      (err) => err instanceof Error && err.message.toLowerCase().includes("not valid utf-8"),
    );
  });

  it("errorPayload rejects oversized message", () => {
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

  it("errorPayload rejects invalid UTF-8 message", () => {
    const invalidUtf8 = Buffer.from([0xc0, 0xaf]);
    const payload = Buffer.allocUnsafe(2 + 2 + invalidUtf8.length);
    payload.writeUInt16BE(1, 0); // code
    payload.writeUInt16BE(invalidUtf8.length, 2);
    invalidUtf8.copy(payload, 4);

    assert.throws(
      () => decodeTcpMuxErrorPayload(payload),
      (err) => err instanceof Error && err.message.toLowerCase().includes("message is not valid utf-8"),
    );
  });

  it("frameParser exposes oversized header without throwing", () => {
    const parser = new TcpMuxFrameParser();
    const header = Buffer.allocUnsafe(9);
    header.writeUInt8(TcpMuxMsgType.DATA, 0);
    header.writeUInt32BE(1, 1);
    const oversizedLen = 16 * 1024 * 1024 + 1;
    header.writeUInt32BE(oversizedLen, 5);

    assert.doesNotThrow(() => parser.push(header));
    const pending = parser.peekHeader();
    assert.ok(pending);
    assert.equal(pending.payloadLength, oversizedLen);
  });
});
