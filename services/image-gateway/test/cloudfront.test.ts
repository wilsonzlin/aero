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
      path: "/images/u/i/v/disk.img",
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
    expect(names.some((name) => name === "CloudFront-Expires" || name === "CloudFront-Policy")).toBe(
      true
    );

    const formatted = formatSetCookie(cookies[0]);
    expect(formatted).toContain(`${cookies[0].name}=`);
    expect(formatted).toContain("Path=/images");
    expect(formatted).toContain("Secure");
    expect(formatted).toContain("SameSite=None");
    expect(formatted).toContain("Domain=example.com");
    expect(formatted).toContain(`Expires=${expiresAt.toUTCString()}`);
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

