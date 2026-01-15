import assert from "node:assert/strict";
import test from "node:test";
import { getCookieValue, getCookieValueFromRequest, isRequestSecure } from "../src/cookies.js";

function makeReq(
  opts: Readonly<{
    encrypted?: boolean;
    forwardedProto?: string | string[];
  }>,
): import("node:http").IncomingMessage {
  return {
    socket: { encrypted: opts.encrypted },
    headers: {
      "x-forwarded-proto": opts.forwardedProto,
    },
  } as unknown as import("node:http").IncomingMessage;
}

test("isRequestSecure: encrypted socket is always secure", () => {
  assert.equal(isRequestSecure(makeReq({ encrypted: true }), { trustProxy: false }), true);
  assert.equal(isRequestSecure(makeReq({ encrypted: true }), { trustProxy: true }), true);
});

test("isRequestSecure: without trustProxy, forwarded proto is ignored", () => {
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "https" }), { trustProxy: false }), false);
});

test("isRequestSecure: trustProxy uses first X-Forwarded-Proto value, case-insensitive", () => {
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "https" }), { trustProxy: true }), true);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "HTTPS" }), { trustProxy: true }), true);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "https, http" }), { trustProxy: true }), true);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: " https " }), { trustProxy: true }), true);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "http" }), { trustProxy: true }), false);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "ftp" }), { trustProxy: true }), false);
});

test("isRequestSecure: handles repeated headers (array)", () => {
  assert.equal(isRequestSecure(makeReq({ forwardedProto: ["https", "http"] }), { trustProxy: true }), true);
});

test("isRequestSecure: missing/empty header is not secure", () => {
  assert.equal(isRequestSecure(makeReq({ forwardedProto: undefined }), { trustProxy: true }), false);
  assert.equal(isRequestSecure(makeReq({ forwardedProto: "" }), { trustProxy: true }), false);
});

test("getCookieValue: returns decoded cookie value", () => {
  assert.equal(getCookieValue("a=1; aero_session=hello%20world; b=2", "aero_session"), "hello world");
});

test("getCookieValue: ignores whitespace and returns first match", () => {
  assert.equal(getCookieValue("  a=1 ; aero_session = t1 ; aero_session=t2", "aero_session"), "t1");
});

test("getCookieValue: handles array headers", () => {
  assert.equal(getCookieValue(["a=1", "aero_session=ok"], "aero_session"), "ok");
});

test("getCookieValue: array headers preserve first cookie wins semantics", () => {
  assert.equal(getCookieValue(["aero_session=", "aero_session=ok"], "aero_session"), "");
  assert.equal(getCookieValue(["aero_session=%zz", "aero_session=ok"], "aero_session"), "%zz");
});

test("getCookieValue: returns raw value on decodeURIComponent failure", () => {
  assert.equal(getCookieValue("aero_session=%zz", "aero_session"), "%zz");
});

test("getCookieValue: returns undefined when missing", () => {
  assert.equal(getCookieValue("a=1; b=2", "aero_session"), undefined);
});

test("getCookieValueFromRequest: prefers rawHeaders order for repeated Cookie headers", () => {
  const req = {
    headers: {
      // Simulate a merged value that would otherwise allow bypass.
      cookie: "aero_session=ok",
    },
    rawHeaders: ["Cookie", "aero_session=", "Cookie", "aero_session=ok"],
  } as unknown as import("node:http").IncomingMessage;

  assert.equal(getCookieValueFromRequest(req, "aero_session"), "");
});
