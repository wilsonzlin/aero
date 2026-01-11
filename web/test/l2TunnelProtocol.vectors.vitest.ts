import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import * as l2 from "../src/shared/l2TunnelProtocol";

type ValidVector = {
  name: string;
  type: number;
  flags: number;
  payload_b64: string;
  frame_b64: string;
  wire_b64: string;
  code?: number;
  message?: string;
};

type InvalidVector = {
  name: string;
  frame_b64: string;
  wire_b64: string;
  expectError: true;
  errorContains: string;
};

type Vector = ValidVector | InvalidVector;

type VectorsFile = {
  schema: number;
  magic: number;
  version: number;
  flags: number;
  vectors: Vector[];
};

function decodeB64(b64: string): Uint8Array {
  return new Uint8Array(Buffer.from(b64, "base64"));
}

function vectorsPath(): string {
  const dir = path.dirname(fileURLToPath(import.meta.url));
  return path.resolve(dir, "..", "..", "protocol-vectors", "l2-tunnel-v1.json");
}

describe("l2 tunnel protocol vectors", () => {
  const vectors = JSON.parse(fs.readFileSync(vectorsPath(), "utf8")) as VectorsFile;
  expect(vectors.schema).toBe(1);
  expect(vectors.magic).toBe(l2.L2_TUNNEL_MAGIC);
  expect(vectors.version).toBe(l2.L2_TUNNEL_VERSION);
  expect(vectors.flags).toBe(0);

  for (const v of vectors.vectors) {
    it(v.name, () => {
      const wire = decodeB64(v.wire_b64);

      if ("expectError" in v && v.expectError) {
        let err: unknown;
        try {
          l2.decodeL2Message(wire);
        } catch (e) {
          err = e;
        }
        expect(err).toBeInstanceOf(Error);
        expect((err as Error).message).toContain(v.errorContains);
        return;
      }

      const payload = decodeB64(v.payload_b64);

      const decoded = l2.decodeL2Message(wire);
      expect(decoded.version).toBe(vectors.version);
      expect(decoded.type).toBe(v.type);
      expect(decoded.flags).toBe(v.flags);
      expect(Buffer.from(decoded.payload)).toEqual(Buffer.from(payload));

      let encoded: Uint8Array | undefined;
      switch (v.type) {
        case l2.L2_TUNNEL_TYPE_FRAME:
          encoded = l2.encodeL2Frame(payload);
          break;
        case l2.L2_TUNNEL_TYPE_PING:
          encoded = l2.encodePing(payload);
          break;
        case l2.L2_TUNNEL_TYPE_PONG:
          encoded = l2.encodePong(payload);
          break;
        case l2.L2_TUNNEL_TYPE_ERROR: {
          // No dedicated ERROR encoder today; if one is added, it must match the vectors.
          const maybeEncodeError =
            (l2 as any).encodeL2Error ?? (l2 as any).encodeError ?? (l2 as any).encodeL2TunnelError;
          if (typeof maybeEncodeError !== "function") return;
          encoded = maybeEncodeError(payload);
          break;
        }
        default:
          throw new Error(`unsupported type in vectors: ${v.type}`);
      }

      expect(Buffer.from(encoded)).toEqual(Buffer.from(wire));
    });
  }
});
