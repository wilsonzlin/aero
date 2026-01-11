import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import {
  TcpMuxFrameParser,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/net/tcpMuxProxy";

type TcpMuxFrameVector = {
  name: string;
  msgType: number;
  streamId: number;
  payload_b64: string;
  frame_b64: string;
};

type TcpMuxOpenPayloadVector = {
  name: string;
  host: string;
  port: number;
  metadata?: string;
  payload_b64: string;
};

type TcpMuxClosePayloadVector = {
  name: string;
  flags: number;
  payload_b64: string;
};

type TcpMuxErrorPayloadVector =
  | { name: string; payload_b64: string; code: number; message: string }
  | { name: string; payload_b64: string; expectError: true; errorContains: string };

type TcpMuxParserFrame = { msgType: number; streamId: number; payload_b64: string };

type TcpMuxParserStreamVector =
  | { name: string; chunks_b64: string[]; expectFrames: TcpMuxParserFrame[] }
  | { name: string; chunks_b64: string[]; expectError: true; errorContains: string };

type TcpMuxVectorsFile = {
  schema: number;
  frames: TcpMuxFrameVector[];
  openPayloads: TcpMuxOpenPayloadVector[];
  closePayloads: TcpMuxClosePayloadVector[];
  errorPayloads: TcpMuxErrorPayloadVector[];
  parserStreams: TcpMuxParserStreamVector[];
};

function decodeB64(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, "base64"));
}

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "protocol-vectors", "tcp-mux-v1.json");
}

describe("tcp-mux protocol vectors", () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), "utf8")) as TcpMuxVectorsFile;
  expect(vectors.schema).toBe(1);

  for (const v of vectors.frames) {
    it(`frame/${v.name}`, () => {
      const payload = decodeB64(v.payload_b64);
      const expectedFrame = decodeB64(v.frame_b64);

      const parser = new TcpMuxFrameParser();
      const parsed = parser.push(expectedFrame);
      expect(parsed).toHaveLength(1);
      expect(parsed[0]!.msgType).toBe(v.msgType);
      expect(parsed[0]!.streamId).toBe(v.streamId);
      expect(Buffer.from(parsed[0]!.payload)).toEqual(Buffer.from(payload));
      expect(() => parser.finish()).not.toThrow();

      const encoded = encodeTcpMuxFrame(v.msgType as any, v.streamId, payload);
      expect(Buffer.from(encoded)).toEqual(Buffer.from(expectedFrame));
    });
  }

  for (const v of vectors.openPayloads) {
    it(`openPayload/${v.name}`, () => {
      const expected = decodeB64(v.payload_b64);

      const encoded = encodeTcpMuxOpenPayload({ host: v.host, port: v.port, metadata: v.metadata });
      expect(Buffer.from(encoded)).toEqual(Buffer.from(expected));

      const decoded = decodeTcpMuxOpenPayload(encoded);
      expect(decoded.host).toBe(v.host);
      expect(decoded.port).toBe(v.port);
      expect(decoded.metadata).toBe(v.metadata);
    });
  }

  for (const v of vectors.closePayloads) {
    it(`closePayload/${v.name}`, () => {
      const expected = decodeB64(v.payload_b64);

      const encoded = encodeTcpMuxClosePayload(v.flags);
      expect(Buffer.from(encoded)).toEqual(Buffer.from(expected));

      const decoded = decodeTcpMuxClosePayload(encoded);
      expect(decoded.flags).toBe(v.flags);
    });
  }

  for (const v of vectors.errorPayloads) {
    it(`errorPayload/${v.name}`, () => {
      const payload = decodeB64(v.payload_b64);

      if ("expectError" in v && v.expectError) {
        let err: unknown;
        try {
          decodeTcpMuxErrorPayload(payload);
        } catch (e) {
          err = e;
        }
        expect(err).toBeInstanceOf(Error);
        expect((err as Error).message).toContain(v.errorContains);
        return;
      }

      const encoded = encodeTcpMuxErrorPayload(v.code, v.message);
      expect(Buffer.from(encoded)).toEqual(Buffer.from(payload));

      const decoded = decodeTcpMuxErrorPayload(payload);
      expect(decoded).toEqual({ code: v.code, message: v.message });
    });
  }

  for (const v of vectors.parserStreams) {
    it(`parserStream/${v.name}`, () => {
      const parser = new TcpMuxFrameParser();
      const parsed: Array<{ msgType: number; streamId: number; payload: Uint8Array }> = [];

      for (const chunkB64 of v.chunks_b64) {
        parsed.push(...parser.push(decodeB64(chunkB64)));
      }

      if ("expectError" in v && v.expectError) {
        let err: unknown;
        try {
          parser.finish();
        } catch (e) {
          err = e;
        }
        expect(err).toBeInstanceOf(Error);
        expect((err as Error).message).toContain(v.errorContains);
        return;
      }

      expect(parsed).toHaveLength(v.expectFrames.length);
      for (let i = 0; i < v.expectFrames.length; i++) {
        const exp = v.expectFrames[i]!;
        const got = parsed[i]!;
        expect(got.msgType).toBe(exp.msgType);
        expect(got.streamId).toBe(exp.streamId);
        expect(Buffer.from(got.payload)).toEqual(Buffer.from(decodeB64(exp.payload_b64)));
      }

      expect(() => parser.finish()).not.toThrow();
    });
  }
});

