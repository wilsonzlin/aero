// @vitest-environment node

import { describe, expect, it } from "vitest";

import { assertNonSecretUrl, assertValidLeaseEndpoint } from "./url_safety";

describe("assertNonSecretUrl", () => {
  it("accepts stable URL without query", () => {
    expect(() => assertNonSecretUrl("https://cdn.example.test/disk.img")).not.toThrow();
  });

  it("rejects X-Amz-Signature (case-insensitive)", () => {
    expect(() => assertNonSecretUrl("https://cdn.example.test/disk.img?X-Amz-Signature=deadbeef")).toThrow();
  });

  it("rejects signature=", () => {
    expect(() => assertNonSecretUrl("https://cdn.example.test/disk.img?signature=deadbeef")).toThrow();
  });

  it("rejects policy= and key-pair-id=", () => {
    expect(() => assertNonSecretUrl("https://cdn.example.test/disk.img?policy=deadbeef")).toThrow();
    expect(() => assertNonSecretUrl("https://cdn.example.test/disk.img?key-pair-id=deadbeef")).toThrow();
  });

  it("rejects embedded basic auth", () => {
    expect(() => assertNonSecretUrl("https://user:pass@cdn.example.test/disk.img")).toThrow();
  });
});

describe("assertValidLeaseEndpoint", () => {
  it("accepts /api/lease", () => {
    expect(() => assertValidLeaseEndpoint("/api/lease")).not.toThrow();
  });

  it("rejects absolute URL", () => {
    expect(() => assertValidLeaseEndpoint("https://evil.example/lease")).toThrow();
  });

  it("rejects path without leading slash", () => {
    expect(() => assertValidLeaseEndpoint("api/lease")).toThrow();
  });
});

