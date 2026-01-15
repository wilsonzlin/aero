import assert from "node:assert/strict";
import fs from "node:fs";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { encodeL2Message } from "../prototype/nt-arch-rfc/l2_tunnel_proto.js";
import { L2_TUNNEL_SUBPROTOCOL } from "../web/src/shared/l2TunnelProtocol.js";

function decodeHex(hex) {
  assert.equal(hex.length % 2, 0, `hex string length must be even, got ${hex.length}`);
  return Buffer.from(hex, "hex");
}

function loadVectors() {
  const vectorsPath = fileURLToPath(new URL("../crates/conformance/test-vectors/aero-vectors-v1.json", import.meta.url));
  return JSON.parse(fs.readFileSync(vectorsPath, "utf8"));
}

const vectors = loadVectors();
assert.equal(vectors.version, 1);
assert.ok(vectors[L2_TUNNEL_SUBPROTOCOL], `missing ${L2_TUNNEL_SUBPROTOCOL} vectors`);

describe("prototype/nt-arch-rfc L2 tunnel protocol vectors", () => {
  for (const v of vectors[L2_TUNNEL_SUBPROTOCOL].valid) {
    it(v.name, () => {
      const payload = decodeHex(v.payloadHex);
      const expectedFrame = decodeHex(v.wireHex);

      const encoded = encodeL2Message(v.msgType, payload);
      assert.deepEqual(encoded, expectedFrame);
    });
  }
});
