import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { TcpTargetParseError, parseTcpTarget } from "./tcpTarget.ts";

function parse(query: string) {
  return parseTcpTarget(new URLSearchParams(query));
}

describe("parseTcpTarget", () => {
  it("parses host+port form", () => {
    assert.deepEqual(parse("host=example.com&port=443"), {
      host: "example.com",
      port: 443,
      version: 1,
    });
  });

  it("parses target form", () => {
    assert.deepEqual(parse("target=example.com:443"), {
      host: "example.com",
      port: 443,
      version: 1,
    });
  });

  it("defaults to v1 when v is absent", () => {
    assert.equal(parse("host=example.com&port=80").version, 1);
  });

  it("accepts explicit v=1", () => {
    assert.equal(parse("v=1&host=example.com&port=80").version, 1);
  });

  it("rejects unsupported versions", () => {
    assert.throws(
      () => parse("v=2&host=example.com&port=80"),
      (err: unknown) =>
        err instanceof TcpTargetParseError &&
        err.code === "ERR_TCP_UNSUPPORTED_VERSION",
    );
  });

  it("prefers target when both forms are provided", () => {
    assert.deepEqual(parse("target=example.com:443&host=bad&port=1"), {
      host: "example.com",
      port: 443,
      version: 1,
    });
  });

  it("parses RFC3986 bracketed IPv6 in target form", () => {
    assert.deepEqual(parse("target=%5B2001:db8::1%5D:443"), {
      host: "2001:db8::1",
      port: 443,
      version: 1,
    });
  });

  it("accepts bracketed IPv6 host param", () => {
    assert.deepEqual(parse("host=%5B2001:db8::1%5D&port=443"), {
      host: "2001:db8::1",
      port: 443,
      version: 1,
    });
  });

  it("rejects missing host", () => {
    assert.throws(
      () => parse("port=80"),
      (err: unknown) =>
        err instanceof TcpTargetParseError &&
        err.code === "ERR_TCP_MISSING_HOST",
    );
  });

  it("rejects missing port", () => {
    assert.throws(
      () => parse("host=example.com"),
      (err: unknown) =>
        err instanceof TcpTargetParseError &&
        err.code === "ERR_TCP_MISSING_PORT",
    );
  });

  it("rejects invalid target strings", () => {
    assert.throws(
      () => parse("target=example.com"),
      (err: unknown) =>
        err instanceof TcpTargetParseError &&
        err.code === "ERR_TCP_INVALID_TARGET",
    );
  });
});

