import test from "node:test";
import assert from "node:assert/strict";

import { dcCloseSafe, dcSendSafe, pcCloseSafe } from "../web/src/net/rtcSafe.ts";

test("web rtcSafe: dcSendSafe returns false when channel is not open", () => {
  const calls = [];
  const dc = {
    readyState: "closing",
    send: () => {
      calls.push("send");
    },
  };
  assert.equal(dcSendSafe(dc, new Uint8Array([1, 2, 3])), false);
  assert.deepEqual(calls, []);
});

test("web rtcSafe: dcSendSafe returns true and sends when open", () => {
  const calls = [];
  const dc = {
    readyState: "open",
    send: (data) => {
      calls.push(data);
    },
  };
  assert.equal(dcSendSafe(dc, new Uint8Array([1, 2, 3])), true);
  assert.equal(calls.length, 1);
});

test("web rtcSafe: dcSendSafe returns false when send throws", () => {
  const dc = {
    readyState: "open",
    send: () => {
      throw new Error("boom");
    },
  };
  assert.equal(dcSendSafe(dc, new Uint8Array([1])), false);
});

test("web rtcSafe: dcCloseSafe never throws", () => {
  dcCloseSafe(null);
  dcCloseSafe({});
  dcCloseSafe({
    close: () => {
      throw new Error("boom");
    },
  });
});

test("web rtcSafe: pcCloseSafe never throws", () => {
  pcCloseSafe(null);
  pcCloseSafe({});
  pcCloseSafe({
    close: () => {
      throw new Error("boom");
    },
  });
});
