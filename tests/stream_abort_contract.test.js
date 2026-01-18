import test from "node:test";
import assert from "node:assert/strict";

import { isExpectedStreamAbort } from "../src/stream_abort.js";

test("stream_abort: classifies known abort/disconnect codes", () => {
  assert.equal(isExpectedStreamAbort({ code: "ERR_STREAM_PREMATURE_CLOSE" }), true);
  assert.equal(isExpectedStreamAbort({ code: "ECONNRESET" }), true);
  assert.equal(isExpectedStreamAbort({ code: "EPIPE" }), true);

  assert.equal(isExpectedStreamAbort({ code: "ETIMEDOUT" }), false);
  assert.equal(isExpectedStreamAbort({}), false);
  assert.equal(isExpectedStreamAbort(null), false);
  assert.equal(isExpectedStreamAbort("nope"), false);
});

test("stream_abort: does not throw on hostile error-like objects", () => {
  const hostileCode = {};
  Object.defineProperty(hostileCode, "code", {
    get() {
      throw new Error("boom");
    },
  });

  const hostileCause = {};
  Object.defineProperty(hostileCause, "cause", {
    get() {
      throw new Error("boom");
    },
  });

  assert.doesNotThrow(() => isExpectedStreamAbort(hostileCode));
  assert.doesNotThrow(() => isExpectedStreamAbort(hostileCause));
  assert.doesNotThrow(() => isExpectedStreamAbort({ cause: hostileCode }));
});
