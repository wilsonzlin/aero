import assert from "node:assert/strict";
import test from "node:test";
import { findHttpErrorReflectionSinksInSource } from "./_helpers/http_error_reflection_scan_helpers.js";

function hasKind(hits, substr) {
  return hits.some((h) => typeof h.kind === "string" && h.kind.includes(substr));
}

test("http error reflection scan: detects direct and chained err.message response sinks", () => {
  const src = [
    "function handler(req, res) {",
    "  try { throw new Error('x') } catch (err) {",
    "    res.send(err?.message);",
    "    reply.send(err?.['message']);",
    "    res.send(err.m\\u0065ssage);",
    "    res.send(err?.m\\u0065ssage);",
    "    res.send((err).message);",
    "    res.send(((err))?.message);",
    "    res.send((err)['message']);",
    "    res.send((err).m\\u0065ssage);",
    "    res.send(String((err)));",
    "    res.send(JSON.stringify((err)));",
    "    res['send'](err.message);",
    "    reply?.['send']?.(err.message);",
    "    reply.code(500).send(err.message);",
    "    res.send(String(err));",
    "    res.writeHead(500, err.message);",
    "    reply.json({ error: 'bad', message: err.message });",
    "    reply.json({ message: (err).message });",
    "  }",
    "}",
  ].join("\n");
  const hits = findHttpErrorReflectionSinksInSource(src);
  assert.ok(hasKind(hits, "err.message"), hits.map((h) => h.kind).join("\n"));
  assert.ok(hasKind(hits, "String(err)"), hits.map((h) => h.kind).join("\n"));
});

test("http error reflection scan: detects err['message'] bracket-string access in response args", () => {
  const src = [
    "function handler(req, res) {",
    "  try { throw new Error('x') } catch (err) {",
    "    res.send(err?.['message']);",
    "    res.send(err['message']);",
    "    res.send(err[\"m\\\\u0065ssage\"]);",
    "    res.send(err[\"m\\\\u{65}ssage\"]);",
    "  }",
    "}",
  ].join("\n");
  const hits = findHttpErrorReflectionSinksInSource(src);
  assert.ok(hits.some((h) => String(h.kind).includes("http send")), hits.map((h) => h.kind).join("\n"));
});

test("http error reflection scan: detects unicode-escaped dot message access in response args", () => {
  const src = [
    "function handler(req, res) {",
    "  try { throw new Error('x') } catch (err) {",
    "    res.send(err.m\\u0065ssage);",
    "    res.send(err?.m\\u0065ssage);",
    "    res.send((err)?.m\\u0065ssage);",
    "    res.send(err.m\\u{65}ssage);",
    "    res.send(err?.m\\u{65}ssage);",
    "    res.send((err)?.m\\u{65}ssage);",
    "  }",
    "}",
  ].join("\n");
  const hits = findHttpErrorReflectionSinksInSource(src);
  assert.ok(hits.some((h) => String(h.kind).includes("http send")), hits.map((h) => h.kind).join("\n"));
});

test("http error reflection scan: detects reply.raw.end(err) reference-taking", () => {
  const src = [
    "function handler(reply) {",
    "  try { throw new Error('x') } catch (err) {",
    "    reply.raw.end(err);",
    "  }",
    "}",
  ].join("\n");
  const hits = findHttpErrorReflectionSinksInSource(src);
  assert.ok(hits.some((h) => h.kind === "http direct err arg"), hits.map((h) => h.kind).join("\n"));
});

test("http error reflection scan: detects reply.raw.end((err)) parenthesized direct arg", () => {
  const src = [
    "function handler(reply) {",
    "  try { throw new Error('x') } catch (err) {",
    "    reply.raw.end((err));",
    "  }",
    "}",
  ].join("\n");
  const hits = findHttpErrorReflectionSinksInSource(src);
  assert.ok(hits.some((h) => h.kind === "http direct err arg"), hits.map((h) => h.kind).join("\n"));
});

test("http error reflection scan: does not false-positive on nearby catch(err) blocks", () => {
  const src = [
    "function handler(reply) {",
    "  try {",
    "    reply.send('ok');",
    "  } catch (err) {",
    "    console.log(err);",
    "  }",
    "}",
  ].join("\n");
  assert.deepEqual(findHttpErrorReflectionSinksInSource(src), []);
});

