import test from "node:test";
import assert from "node:assert/strict";

import { encodeHttpTextResponse } from "../src/http_text_response.js";
import httpTextResponseCjs from "../src/http_text_response.cjs";

test("http_text_response: encodes headers + body with correct Content-Length", () => {
  const buf = encodeHttpTextResponse({
    statusCode: 418,
    statusText: "I'm a teapot",
    bodyText: "hello\n",
  });
  assert.ok(Buffer.isBuffer(buf));

  const text = buf.toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 418 I'm a teapot\r\n"));
  assert.ok(text.includes("\r\nContent-Type: text/plain; charset=utf-8\r\n"));
  assert.ok(text.includes("\r\nCache-Control: no-store\r\n"));
  assert.ok(text.includes("\r\nConnection: close\r\n"));
  assert.ok(text.includes("\r\n\r\nhello\n"));
  assert.ok(!text.includes("\r\n\r\n\r\nhello\n"));

  const lenMatch = text.match(/\r\nContent-Length: (\d+)\r\n/u);
  assert.ok(lenMatch, "expected Content-Length header");
  assert.equal(Number(lenMatch[1]), Buffer.byteLength("hello\n"));
});

test("http_text_response: CJS parity", () => {
  const cjsEncode = httpTextResponseCjs.encodeHttpTextResponse;
  assert.equal(typeof cjsEncode, "function");

  const a = encodeHttpTextResponse({ statusCode: 418, statusText: "I'm a teapot", bodyText: "hello\n" });
  const b = cjsEncode({ statusCode: 418, statusText: "I'm a teapot", bodyText: "hello\n" });
  assert.equal(a.toString("hex"), b.toString("hex"));

  const c = encodeHttpTextResponse({ statusCode: 400, statusText: "Bad Request", bodyText: "bad\n", cacheControl: null });
  const d = cjsEncode({ statusCode: 400, statusText: "Bad Request", bodyText: "bad\n", cacheControl: null });
  assert.equal(c.toString("hex"), d.toString("hex"));

  assert.throws(() => cjsEncode({ statusCode: 200, statusText: "OK\r\nX: y", bodyText: "" }));
  assert.throws(() => cjsEncode({ statusCode: 200, statusText: "OK", bodyText: "", contentType: "text/plain\r\nx:y" }));
  assert.throws(() => cjsEncode({ statusCode: 200, statusText: "OK", bodyText: "", cacheControl: "no-store\r\nx:y" }));
  assert.throws(() => cjsEncode({ statusCode: 200, statusText: "OK", bodyText: "", cacheControl: "" }));
});

test("http_text_response: omits Cache-Control when cacheControl=null", () => {
  const buf = encodeHttpTextResponse({
    statusCode: 400,
    statusText: "Bad Request",
    bodyText: "bad\n",
    cacheControl: null,
  });
  const text = buf.toString("utf8");
  assert.ok(text.startsWith("HTTP/1.1 400 Bad Request\r\n"));
  assert.ok(!text.includes("\r\nCache-Control:"));
});

test("http_text_response: rejects CRLF in statusText/contentType/cacheControl", () => {
  assert.throws(() => encodeHttpTextResponse({ statusCode: 200, statusText: "OK\r\nX: y", bodyText: "" }));
  assert.throws(() =>
    encodeHttpTextResponse({ statusCode: 200, statusText: "OK", bodyText: "", contentType: "text/plain\r\nx:y" }),
  );
  assert.throws(() =>
    encodeHttpTextResponse({ statusCode: 200, statusText: "OK", bodyText: "", cacheControl: "no-store\r\nx:y" }),
  );
});

test("http_text_response: rejects empty cacheControl (use null to omit)", () => {
  assert.throws(() =>
    encodeHttpTextResponse({ statusCode: 200, statusText: "OK", bodyText: "", cacheControl: "" }),
  );
  assert.throws(() =>
    encodeHttpTextResponse({ statusCode: 200, statusText: "OK", bodyText: "", cacheControl: "   " }),
  );
});
