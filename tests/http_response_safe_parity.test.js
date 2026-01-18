import assert from "node:assert/strict";
import test from "node:test";

import * as esm from "../src/http_response_safe.js";
import * as cjs from "../src/http_response_safe.cjs";

test("http_response_safe: ESM/CJS parity for JSON fallback behavior", () => {
  const mkRes = () => ({
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
  });

  const hostile = new Proxy(
    {},
    {
      get() {
        throw new Error("boom");
      },
    },
  );

  const a = mkRes();
  const b = mkRes();
  esm.sendJsonNoStore(a, 200, hostile, {});
  cjs.sendJsonNoStore(b, 200, hostile, {});

  assert.equal(a.statusCode, 500);
  assert.equal(b.statusCode, 500);
  assert.equal(a.headers["cache-control"], "no-store");
  assert.equal(b.headers["cache-control"], "no-store");
  assert.equal(a.body, `{"error":"internal server error"}`);
  assert.equal(b.body, `{"error":"internal server error"}`);
});

test("http_response_safe: ESM/CJS parity for tryWriteResponse header handling", () => {
  const mkRes = () => ({
    writeHeadArgs: null,
    endArgs: null,
    writeHead() {
      this.writeHeadArgs = Array.from(arguments);
    },
    end() {
      this.endArgs = Array.from(arguments);
    },
  });

  const a = mkRes();
  const b = mkRes();
  esm.tryWriteResponse(a, 204, [], undefined);
  cjs.tryWriteResponse(b, 204, [], undefined);
  assert.deepEqual(a.writeHeadArgs, [204]);
  assert.deepEqual(b.writeHeadArgs, [204]);

  const a1 = mkRes();
  const b1 = mkRes();
  const rawHeaders = ["cache-control", "no-store"];
  esm.tryWriteResponse(a1, 204, rawHeaders, undefined);
  cjs.tryWriteResponse(b1, 204, rawHeaders, undefined);
  assert.deepEqual(a1.writeHeadArgs, [204, rawHeaders]);
  assert.deepEqual(b1.writeHeadArgs, [204, rawHeaders]);

  const aBad = mkRes();
  const bBad = mkRes();
  esm.tryWriteResponse(aBad, 204, ["cache-control"], undefined);
  cjs.tryWriteResponse(bBad, 204, ["cache-control"], undefined);
  assert.deepEqual(aBad.writeHeadArgs, [204]);
  assert.deepEqual(bBad.writeHeadArgs, [204]);

  const a2 = mkRes();
  const b2 = mkRes();
  const headers = { "cache-control": "no-store" };
  esm.tryWriteResponse(a2, 204, headers, undefined);
  cjs.tryWriteResponse(b2, 204, headers, undefined);
  assert.deepEqual(a2.writeHeadArgs, [204, headers]);
  assert.deepEqual(b2.writeHeadArgs, [204, headers]);
});

