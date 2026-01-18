import test from "node:test";
import assert from "node:assert/strict";

import { createHttpHandler } from "../src/http.js";

test("http handler does not throw if res.setHeader throws (best-effort headers)", async () => {
  const handler = createHttpHandler({
    config: {
      staticDir: ".",
      tokens: [],
      allowedOrigins: [],
      allowHosts: [{ kind: "wildcard" }],
      allowPrivateRanges: true,
    },
    logger: {
      info() {},
      warn() {},
      error() {},
    },
    metrics: {
      toPrometheus() {
        return "";
      },
      increment() {},
    },
  });

  const calls = [];

  const req = {
    method: "GET",
    url: "/healthz",
    headers: {},
  };

  const res = {
    destroyed: false,
    headersSent: false,
    writableEnded: false,
    statusCode: 0,
    setHeader() {
      throw new Error("boom");
    },
    end() {
      calls.push("end");
      this.writableEnded = true;
    },
    destroy() {
      calls.push("destroy");
      this.destroyed = true;
    },
  };

  assert.doesNotThrow(() => handler(req, res));
  await new Promise((r) => setImmediate(r));

  // With hostile setHeader, the handler may fall back to destroying the response.
  assert.ok(calls.includes("destroy") || calls.includes("end"));
});

test("http handler does not throw if res.destroyed getter throws during static handling", async () => {
  const handler = createHttpHandler({
    config: {
      staticDir: ".",
      tokens: [],
      allowedOrigins: [],
      allowHosts: [{ kind: "wildcard" }],
      allowPrivateRanges: true,
    },
    logger: {
      info() {},
      warn() {},
      error() {},
    },
    metrics: {
      toPrometheus() {
        return "";
      },
      increment() {},
    },
  });

  const req = {
    method: "GET",
    url: "/README.md",
    headers: {},
  };

  const res = {
    get destroyed() {
      throw new Error("boom");
    },
    setHeader() {},
    end() {},
    destroy() {},
  };

  assert.doesNotThrow(() => handler(req, res));
  await new Promise((r) => setImmediate(r));
});

