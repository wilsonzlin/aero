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

test("http handler does not throw if req.url getter throws (and responds best-effort)", async () => {
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
    headers: {},
  };
  Object.defineProperty(req, "url", {
    get() {
      throw new Error("boom");
    },
  });

  const calls = [];
  const res = {
    statusCode: 0,
    setHeader() {},
    end() {
      calls.push("end");
    },
    destroy() {
      calls.push("destroy");
    },
  };

  assert.doesNotThrow(() => handler(req, res));
  await new Promise((r) => setImmediate(r));
  assert.ok(calls.includes("end") || calls.includes("destroy"));
});

