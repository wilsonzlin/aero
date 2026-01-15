import assert from "node:assert/strict";
import test from "node:test";

import { parseUpstreams } from "../src/dns/upstream.js";

test("parseUpstreams: parses UDP upstreams with default port", () => {
  const upstreams = parseUpstreams([" 8.8.8.8 ", ""]);
  assert.deepEqual(upstreams, [{ kind: "udp", host: "8.8.8.8", port: 53, label: "8.8.8.8:53" }]);
});

test("parseUpstreams: parses UDP upstreams with explicit port", () => {
  const upstreams = parseUpstreams(["1.1.1.1:5353"]);
  assert.deepEqual(upstreams, [{ kind: "udp", host: "1.1.1.1", port: 5353, label: "1.1.1.1:5353" }]);
});

test("parseUpstreams: accepts bracketed IPv6", () => {
  const upstreams = parseUpstreams(["[::1]:53"]);
  assert.deepEqual(upstreams, [{ kind: "udp", host: "::1", port: 53, label: "::1:53" }]);
});

test("parseUpstreams: rejects unbracketed IPv6 with port", () => {
  assert.throws(() => parseUpstreams(["::1:53"]), /use \[ipv6\]:port/);
});

test("parseUpstreams: supports udp:// prefix", () => {
  const upstreams = parseUpstreams(["udp://8.8.4.4:53"]);
  assert.deepEqual(upstreams, [{ kind: "udp", host: "8.8.4.4", port: 53, label: "8.8.4.4:53" }]);
});

test("parseUpstreams: parses DoH URLs", () => {
  const upstreams = parseUpstreams(["https://dns.example/dns-query"]);
  assert.deepEqual(upstreams, [{ kind: "doh", url: "https://dns.example/dns-query", label: "https://dns.example/dns-query" }]);
});

test("parseUpstreams: rejects oversized entries", () => {
  assert.throws(() => parseUpstreams(["x".repeat(5000)]), /Invalid upstream/);
});
