import test from "node:test";
import assert from "node:assert/strict";

import ipaddr from "ipaddr.js";

test("ipaddr.js basic parsing works (ipv4 + ipv6)", () => {
  assert.equal(ipaddr.parse("1.2.3.4").kind(), "ipv4");
  assert.equal(ipaddr.parse("2001:db8::1").kind(), "ipv6");
});

test("ipaddr.js IPv6 :: compression round-trips via toString()", () => {
  assert.equal(ipaddr.parse("::").toString(), "::");
  assert.equal(ipaddr.parse("::1").toString(), "::1");
  assert.equal(ipaddr.fromByteArray(Array(15).fill(0).concat([1])).toString(), "::1");
});

test("ipaddr.js CIDR matching behaves as expected", () => {
  const cidr = ipaddr.parseCIDR("10.0.0.0/8");
  assert.equal(ipaddr.parse("10.1.2.3").match(cidr), true);
  assert.equal(ipaddr.parse("192.168.0.1").match(cidr), false);
});

test("ipaddr.js range classification matches expected categories", () => {
  assert.equal(ipaddr.parse("8.8.8.8").range(), "unicast");
  assert.equal(ipaddr.parse("10.0.0.1").range(), "private");
  assert.equal(ipaddr.parse("127.0.0.1").range(), "loopback");

  assert.equal(ipaddr.parse("::").range(), "unspecified");
  assert.equal(ipaddr.parse("::1").range(), "loopback");
  assert.equal(ipaddr.parse("fc00::1").range(), "uniqueLocal");
  assert.equal(ipaddr.parse("fe80::1").range(), "linkLocal");
});

