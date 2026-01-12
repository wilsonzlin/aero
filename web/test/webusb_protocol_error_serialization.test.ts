import assert from "node:assert/strict";
import test from "node:test";

import { deserializeWebUsbError, serializeWebUsbError } from "../../src/platform/legacy/webusb_protocol.ts";

test("serializeWebUsbError preserves name/message for DOMException-like objects", () => {
  const err = { name: "NotAllowedError", message: "Permission denied." };
  const serialized = serializeWebUsbError(err);
  assert.equal(serialized.name, "NotAllowedError");
  assert.equal(serialized.message, "Permission denied.");
});

test("deserializeWebUsbError rehydrates name/message", () => {
  const serialized = { name: "NetworkError", message: "Unable to claim interface." };
  const err = deserializeWebUsbError(serialized);
  assert.equal(err.name, "NetworkError");
  assert.equal(err.message, "Unable to claim interface.");
});
