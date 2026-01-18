import test from "node:test";
import assert from "node:assert/strict";

import { wsBufferedAmountSafe } from "../wsBufferedAmount";

test("wsBufferedAmountSafe returns bufferedAmount when finite number", () => {
  const ws = { bufferedAmount: 123 } as any;
  assert.equal(wsBufferedAmountSafe(ws), 123);
});

test("wsBufferedAmountSafe returns 0 for non-number bufferedAmount", () => {
  const ws = { bufferedAmount: "nope" } as any;
  assert.equal(wsBufferedAmountSafe(ws), 0);
});

test("wsBufferedAmountSafe returns 0 if bufferedAmount getter throws", () => {
  const ws = {} as any;
  Object.defineProperty(ws, "bufferedAmount", {
    get() {
      throw new Error("boom");
    },
  });
  assert.equal(wsBufferedAmountSafe(ws), 0);
});

