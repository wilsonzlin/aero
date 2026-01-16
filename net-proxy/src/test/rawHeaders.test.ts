import assert from "node:assert/strict";
import test from "node:test";

import { rawHeaderSingle } from "../rawHeaders";

test("rawHeaders: rawHeaderSingle tri-state contract", () => {
  assert.equal(rawHeaderSingle(undefined, "origin", 10), undefined);
  assert.equal(rawHeaderSingle({}, "origin", 10), undefined);
  assert.equal(rawHeaderSingle([], "origin", 10), undefined);

  assert.equal(rawHeaderSingle(["Host", "example.com"], "origin", 100), undefined);

  assert.equal(rawHeaderSingle(["Origin", "https://a.test"], "origin", 100), "https://a.test");
  assert.equal(rawHeaderSingle(["oRiGiN", "https://a.test"], "origin", 100), "https://a.test");

  assert.equal(rawHeaderSingle(["Origin", "a", "Origin", "b"], "origin", 100), null);
  assert.equal(rawHeaderSingle(["Origin", 123], "origin", 100), null);
  assert.equal(rawHeaderSingle(["Origin", "toolong"], "origin", 3), null);
  assert.equal(rawHeaderSingle(["Origin", "ok"], "origin", 2), "ok");
});

