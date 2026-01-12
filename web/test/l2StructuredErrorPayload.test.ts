import assert from "node:assert/strict";
import test from "node:test";

import { decodeStructuredErrorPayload, encodeStructuredErrorPayload } from "../src/shared/l2TunnelProtocol.ts";

test("structured ERROR payload encodes/decodes and truncates on UTF-8 boundaries", () => {
  // Underflow: cannot fit the structured header, so return an empty payload.
  assert.equal(encodeStructuredErrorPayload(1, "hi", 0).byteLength, 0);
  assert.equal(encodeStructuredErrorPayload(1, "hi", 3).byteLength, 0);

  // Exactly enough space for the structured header, but no message bytes.
  const headerOnly = encodeStructuredErrorPayload(1, "hi", 4);
  assert.equal(headerOnly.byteLength, 4);
  assert.deepEqual(decodeStructuredErrorPayload(headerOnly), { code: 1, message: "" });

  // Message is truncated to fit and must respect UTF-8 boundaries.
  const truncatedAscii = encodeStructuredErrorPayload(1, "hi", 5);
  assert.equal(truncatedAscii.byteLength, 5);
  assert.deepEqual(decodeStructuredErrorPayload(truncatedAscii), { code: 1, message: "h" });

  // Emoji is 4 bytes; if only 1 byte is available for the message, it must be dropped.
  const truncatedEmoji = encodeStructuredErrorPayload(1, "😃", 5);
  assert.equal(truncatedEmoji.byteLength, 4);
  assert.deepEqual(decodeStructuredErrorPayload(truncatedEmoji), { code: 1, message: "" });

  const roundtrip = encodeStructuredErrorPayload(42, "bad frame", 256);
  assert.deepEqual(decodeStructuredErrorPayload(roundtrip), { code: 42, message: "bad frame" });

  // Reject invalid structured payloads:
  // - length mismatch
  assert.equal(decodeStructuredErrorPayload(new Uint8Array([0, 1, 0, 2, 0x61])), null);
  // - invalid UTF-8
  assert.equal(decodeStructuredErrorPayload(new Uint8Array([0, 1, 0, 1, 0x80])), null);
});

