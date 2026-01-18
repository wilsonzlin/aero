import assert from "node:assert/strict";
import test from "node:test";

import { sendJsonNoStore, sendTextNoStore, tryWriteResponse } from "../src/http_response_safe.js";

test("http_response_safe: does not throw if res.writeHead/res.end getters throw", () => {
  const res = new Proxy(
    {},
    {
      get(_t, prop) {
        if (prop === "writeHead") throw new Error("boom");
        if (prop === "end") throw new Error("boom");
        if (prop === "destroy") throw new Error("boom");
        return undefined;
      },
    },
  );

  assert.doesNotThrow(() => tryWriteResponse(res, 200, undefined, undefined));
  assert.doesNotThrow(() => tryWriteResponse(res, 200, null, undefined));
  assert.doesNotThrow(() => sendJsonNoStore(res, 200, { ok: true }, {}));
});

test("http_response_safe: sendJsonNoStore falls back to stable 500 when JSON.stringify throws", () => {
  const res = {
    headers: null,
    statusCode: null,
    body: null,
    writeHead(statusCode, headers) {
      this.statusCode = statusCode;
      this.headers = headers;
    },
    end(body) {
      this.body = body ?? "";
    },
  };

  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error("boom");
      },
    },
  );

  // JSON.stringify(hostile) will throw due to the proxy getter trap.
  sendJsonNoStore(res, 200, hostile, {});
  assert.equal(res.statusCode, 500);
  assert.equal(res.headers["cache-control"], "no-store");
  assert.equal(res.headers["content-type"], "application/json; charset=utf-8");
  assert.equal(res.body, `{"error":"internal server error"}`);
});

test("http_response_safe: tryWriteResponse ignores empty array headers", () => {
  const res = {
    writeHeadArgs: null,
    endCalled: 0,
    writeHead() {
      this.writeHeadArgs = Array.from(arguments);
    },
    end() {
      this.endCalled += 1;
    },
  };

  tryWriteResponse(res, 204, [], undefined);
  assert.deepEqual(res.writeHeadArgs, [204]);
  assert.equal(res.endCalled, 1);
});

test("http_response_safe: tryWriteResponse passes through valid raw headers arrays", () => {
  const res = {
    writeHeadArgs: null,
    endCalled: 0,
    writeHead() {
      this.writeHeadArgs = Array.from(arguments);
    },
    end() {
      this.endCalled += 1;
    },
  };

  const headers = ["cache-control", "no-store"];
  tryWriteResponse(res, 204, headers, undefined);
  assert.deepEqual(res.writeHeadArgs, [204, headers]);
  assert.equal(res.endCalled, 1);
});

test("http_response_safe: tryWriteResponse ignores invalid raw headers arrays", () => {
  const res = {
    writeHeadArgs: null,
    endCalled: 0,
    writeHead() {
      this.writeHeadArgs = Array.from(arguments);
    },
    end() {
      this.endCalled += 1;
    },
  };

  // Odd-length raw header list is invalid.
  tryWriteResponse(res, 204, ["cache-control"], undefined);
  assert.deepEqual(res.writeHeadArgs, [204]);
  assert.equal(res.endCalled, 1);
});

test("http_response_safe: sendJsonNoStore does not throw if opts.contentType getter throws", () => {
  const res = {
    statusCode: null,
    headers: null,
    body: null,
    writeHead(statusCode, headers) {
      this.statusCode = statusCode;
      this.headers = headers;
    },
    end(body) {
      this.body = body ?? "";
    },
  };

  const opts = {
    get contentType() {
      throw new Error("boom");
    },
  };

  assert.doesNotThrow(() => sendJsonNoStore(res, 200, { ok: true }, opts));
  assert.equal(res.statusCode, 200);
  assert.equal(res.headers["content-type"], "application/json; charset=utf-8");
});

test("http_response_safe: sendTextNoStore does not throw if opts.contentType getter throws", () => {
  const res = {
    statusCode: null,
    headers: null,
    body: null,
    writeHead(statusCode, headers) {
      this.statusCode = statusCode;
      this.headers = headers;
    },
    end(body) {
      this.body = body ?? "";
    },
  };

  const opts = {
    get contentType() {
      throw new Error("boom");
    },
  };

  assert.doesNotThrow(() => sendTextNoStore(res, 200, "ok", opts));
  assert.equal(res.statusCode, 200);
  assert.equal(res.headers["content-type"], "text/plain; charset=utf-8");
  assert.equal(res.body, "ok");
});

