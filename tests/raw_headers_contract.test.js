import assert from "node:assert/strict";
import test from "node:test";

const { hasRepeatedRawHeader, iterRawHeaderValues, rawHeaderSingle } = await import(
  new URL("../backend/aero-gateway/src/rawHeaders.ts", import.meta.url)
);

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

test("rawHeaders: hasRepeatedRawHeader detects repeated keys", () => {
  assert.equal(hasRepeatedRawHeader(undefined, "origin"), false);
  assert.equal(hasRepeatedRawHeader([], "origin"), false);
  assert.equal(hasRepeatedRawHeader(["Origin", "a"], "origin"), false);
  assert.equal(hasRepeatedRawHeader(["Origin", "a", "Origin", "b"], "origin"), true);
  assert.equal(hasRepeatedRawHeader(["oRiGiN", "a", "origin", "b"], "origin"), true);
});

test("rawHeaders: iterRawHeaderValues yields string values in order", () => {
  const rawHeaders = ["A", "1", "Origin", "a", "Origin", "b", "Origin", 123, 5, "c"];
  assert.deepEqual(Array.from(iterRawHeaderValues(rawHeaders, "origin")), ["a", "b"]);
});

