import { generateKeyPairSync } from "node:crypto";
import { describe, expect, it } from "vitest";

import {
  buildCloudFrontUrl,
  createSignedCookies,
  createSignedUrl,
  formatSetCookie,
} from "../src/cloudfront";

describe("cloudfront signing", () => {
  it("formats signed cookies with standard CloudFront names", () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const url = buildCloudFrontUrl({
      cloudfrontDomain: "d111111abcdef8.cloudfront.net",
      // Wildcard resource: required so a single cookie set can cover `disk.img`,
      // `manifest.json`, and `chunks/*` for chunked delivery.
      path: "/images/u/i/v/*",
    });

    const expiresAt = new Date("2030-01-01T00:00:00.000Z");
    const cookies = createSignedCookies({
      url,
      keyPairId: "KTESTKEYPAIR",
      privateKeyPem: privateKey,
      expiresAt,
      cookiePath: "/images",
      cookieDomain: "example.com",
    });

    const names = cookies.map((cookie) => cookie.name);
    expect(names).toContain("CloudFront-Key-Pair-Id");
    expect(names).toContain("CloudFront-Signature");
    expect(names).toContain("CloudFront-Policy");
    expect(names).not.toContain("CloudFront-Expires");

    const formatted = formatSetCookie(cookies[0]);
    expect(formatted).toContain(`${cookies[0].name}=`);
    expect(formatted).toContain("Path=/images");
    expect(formatted).toContain("Secure");
    expect(formatted).toContain("HttpOnly");
    expect(formatted).toContain("SameSite=None");
    expect(formatted).toContain("Domain=example.com");
    expect(formatted).toContain(`Expires=${expiresAt.toUTCString()}`);
  });

  it("supports configurable SameSite attributes", () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const url = "https://d111111abcdef8.cloudfront.net/images/u/i/v/disk.img";
    const expiresAt = new Date("2030-01-01T00:00:00.000Z");

    const cookies = createSignedCookies({
      url,
      keyPairId: "KTESTKEYPAIR",
      privateKeyPem: privateKey,
      expiresAt,
      cookiePath: "/images",
      cookieSameSite: "Lax",
    });

    const formatted = formatSetCookie(cookies[0]);
    expect(formatted).toContain("HttpOnly");
    expect(formatted).toContain("SameSite=Lax");
    expect(formatted).not.toContain("SameSite=None");
  });

  it("can emit Partitioned cookies when enabled", () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const url = "https://d111111abcdef8.cloudfront.net/images/u/i/v/disk.img";
    const expiresAt = new Date("2030-01-01T00:00:00.000Z");

    const cookies = createSignedCookies({
      url,
      keyPairId: "KTESTKEYPAIR",
      privateKeyPem: privateKey,
      expiresAt,
      cookiePartitioned: true,
    });

    const formatted = formatSetCookie(cookies[0]);
    expect(formatted).toContain("Partitioned");
    expect(formatted).toContain("SameSite=None");
  });

  it("rejects Partitioned cookies unless SameSite=None", () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const url = "https://d111111abcdef8.cloudfront.net/images/u/i/v/disk.img";
    const expiresAt = new Date("2030-01-01T00:00:00.000Z");

    expect(() =>
      createSignedCookies({
        url,
        keyPairId: "KTESTKEYPAIR",
        privateKeyPem: privateKey,
        expiresAt,
        cookieSameSite: "Strict",
        cookiePartitioned: true,
      })
    ).toThrow(/SameSite=None/);
  });

  it("produces a signed URL containing CloudFront query parameters", () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const url = "https://d111111abcdef8.cloudfront.net/images/u/i/v/disk.img";
    const expiresAt = new Date("2030-01-01T00:00:00.000Z");

    const signed = createSignedUrl({
      url,
      keyPairId: "KTESTKEYPAIR",
      privateKeyPem: privateKey,
      expiresAt,
    });

    expect(signed).toContain("Key-Pair-Id=KTESTKEYPAIR");
    expect(signed).toMatch(/(Expires=|Policy=)/);
    expect(signed).toMatch(/Signature=/);
  });
});
