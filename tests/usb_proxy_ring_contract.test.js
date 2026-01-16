import assert from "node:assert/strict";
import test from "node:test";

import { createUsbProxyRingBuffer, UsbProxyRing } from "../web/src/usb/usb_proxy_ring.ts";

test("usb proxy ring: error completion messages are single-line and byte-bounded", () => {
  const sab = createUsbProxyRingBuffer(64 * 1024);
  const ring = new UsbProxyRing(sab);

  assert.equal(
    ring.pushCompletion({
      kind: "controlOut",
      id: 1,
      status: "error",
      message: "hello\n\tworld",
    }),
    true,
  );
  const popped1 = ring.popCompletion();
  assert.ok(popped1 && popped1.status === "error");
  assert.equal(popped1.message, "hello world");

  assert.equal(
    ring.pushCompletion({
      kind: "controlOut",
      id: 2,
      status: "error",
      message: "x".repeat(600),
    }),
    true,
  );
  const popped2 = ring.popCompletion();
  assert.ok(popped2 && popped2.status === "error");
  assert.equal(popped2.message, "x".repeat(512));
  assert.ok(new TextEncoder().encode(popped2.message).byteLength <= 512);

  assert.equal(ring.popCompletion(), null);
});

