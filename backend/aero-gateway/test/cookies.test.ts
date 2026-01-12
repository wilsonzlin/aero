import assert from "node:assert/strict";
import test from "node:test";
import { isRequestSecure } from "../src/cookies.js";

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

