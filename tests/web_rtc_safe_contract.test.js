import test from "node:test";
import assert from "node:assert/strict";

import { dcBufferedAmountSafe, dcCloseSafe, dcIsClosedSafe, dcIsOpenSafe, dcSendSafe, pcCloseSafe } from "../web/src/net/rtcSafe.ts";

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

test("web rtcSafe: dcSendSafe returns false if readyState getter throws", () => {
  const calls = [];
  const dc = {
    send: () => {
      calls.push("send");
    },
  };
  Object.defineProperty(dc, "readyState", {
    get() {
      throw new Error("nope");
    },
  });
  assert.equal(dcSendSafe(dc, new Uint8Array([1, 2, 3])), false);
  assert.deepEqual(calls, []);
});

test("web rtcSafe: dcSendSafe returns false if send getter throws", () => {
  const dc = { readyState: "open" };
  Object.defineProperty(dc, "send", {
    get() {
      throw new Error("nope");
    },
  });
  assert.equal(dcSendSafe(dc, new Uint8Array([1])), false);
});

test("web rtcSafe: dcIsOpenSafe returns false if readyState getter throws", () => {
  const dc = {};
  Object.defineProperty(dc, "readyState", {
    get() {
      throw new Error("nope");
    },
  });
  assert.equal(dcIsOpenSafe(dc), false);
});

test("web rtcSafe: dcIsOpenSafe returns true only when open", () => {
  assert.equal(dcIsOpenSafe({ readyState: "connecting" }), false);
  assert.equal(dcIsOpenSafe({ readyState: "open" }), true);
  assert.equal(dcIsOpenSafe({ readyState: "closing" }), false);
  assert.equal(dcIsOpenSafe({ readyState: "closed" }), false);
});

test("web rtcSafe: dcIsClosedSafe returns true only when closed", () => {
  assert.equal(dcIsClosedSafe({ readyState: "connecting" }), false);
  assert.equal(dcIsClosedSafe({ readyState: "open" }), false);
  assert.equal(dcIsClosedSafe({ readyState: "closing" }), false);
  assert.equal(dcIsClosedSafe({ readyState: "closed" }), true);
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

test("web rtcSafe: dcCloseSafe does not throw if close getter throws", () => {
  const dc = {};
  Object.defineProperty(dc, "close", {
    get() {
      throw new Error("nope");
    },
  });
  dcCloseSafe(dc);
});

test("web rtcSafe: pcCloseSafe does not throw if close getter throws", () => {
  const pc = {};
  Object.defineProperty(pc, "close", {
    get() {
      throw new Error("nope");
    },
  });
  pcCloseSafe(pc);
});

test("web rtcSafe: dcBufferedAmountSafe returns 0 for invalid/hostile values", () => {
  assert.equal(dcBufferedAmountSafe(null), 0);
  assert.equal(dcBufferedAmountSafe({ bufferedAmount: 123 }), 123);
  assert.equal(dcBufferedAmountSafe({ bufferedAmount: "nope" }), 0);
});

test("web rtcSafe: dcBufferedAmountSafe returns 0 if bufferedAmount getter throws", () => {
  const dc = {};
  Object.defineProperty(dc, "bufferedAmount", {
    get() {
      throw new Error("nope");
    },
  });
  assert.equal(dcBufferedAmountSafe(dc), 0);
});
