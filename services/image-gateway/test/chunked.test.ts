import { GetObjectCommand, HeadObjectCommand } from "@aws-sdk/client-s3";
import type { S3Client } from "@aws-sdk/client-s3";
import { generateKeyPairSync } from "node:crypto";
import { Readable } from "node:stream";
import { describe, expect, it } from "vitest";

import { buildApp } from "../src/app";
import { CACHE_CONTROL_PRIVATE_NO_STORE, type Config } from "../src/config";
import { MemoryImageStore } from "../src/store";

function makeConfig(overrides: Partial<Config> = {}): Config {
  return {
    s3Bucket: "bucket",
    awsRegion: "us-east-1",
    s3Endpoint: undefined,
    s3ForcePathStyle: false,

    cloudfrontDomain: undefined,
    cloudfrontKeyPairId: undefined,
    cloudfrontPrivateKeyPem: undefined,
    cloudfrontAuthMode: "cookie",
    cloudfrontCookieDomain: undefined,
    cloudfrontCookieSameSite: "None",
    cloudfrontCookiePartitioned: false,
    cloudfrontSignedTtlSeconds: 60,

    imageBasePath: "/images",
    partSizeBytes: 64 * 1024 * 1024,
    imageCacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,

    authMode: "dev",
    port: 0,
    corsAllowOrigin: "http://localhost:5173",
    crossOriginResourcePolicy: "same-site",

    ...overrides,
  };
}

describe("chunked delivery", () => {
  it("proxies the chunked manifest from S3 with cache headers", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1",
      uploadId: "upload-1",
      status: "complete",
    });

    const manifestJson = JSON.stringify({ schema: "aero.chunked-disk-image.v1" });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          expect(command.input.Key).toBe("images/user-1/image-1/v1/manifest.json");
          return {
            Body: Readable.from([manifestJson]),
            ContentType: "application/json",
            ContentLength: manifestJson.length,
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    expect(res.body).toBe(manifestJson);
    expect(res.headers["content-type"]).toBe("application/json");
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
  });

  it("uses chunkedManifestKey as the base for chunk lookups when both fields are present", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      chunkedManifestKey: "images/user-1/image-1/v9/manifest.json",
      uploadId: "upload-1",
      status: "complete",
    });

    let sawManifest = false;
    let sawChunk = false;

    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          if (command.input.Key === "images/user-1/image-1/v9/manifest.json") {
            sawManifest = true;
            return {
              ContentLength: 10,
              ContentType: "application/json",
            };
          }
          if (command.input.Key === "images/user-1/image-1/v9/chunks/00000000.bin") {
            sawChunk = true;
            return {
              ContentLength: 123,
              ContentType: "application/octet-stream",
            };
          }
        }
        throw new Error(`unexpected command: ${JSON.stringify(command)}`);
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const manifestRes = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });
    expect(manifestRes.statusCode).toBe(200);

    const chunkRes = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/chunks/0`,
      headers: { "x-user-id": ownerId },
    });
    expect(chunkRes.statusCode).toBe(200);

    expect(sawManifest).toBe(true);
    expect(sawChunk).toBe(true);
  });

  it("propagates Content-Encoding for the manifest when provided by S3", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          expect(command.input.Key).toBe("images/user-1/image-1/v1/manifest.json");
          return {
            ContentLength: 10,
            ContentType: "application/json",
            ContentEncoding: "gzip",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["content-encoding"]).toBe("gzip");
  });

  it("proxies a chunk object with identity encoding and no-transform", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const chunk = "hello";

    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          expect(command.input.Key).toBe("images/user-1/image-1/v1/chunks/00000042.bin");
          return {
            Body: Readable.from([chunk]),
            ContentType: "application/octet-stream",
            ContentLength: chunk.length,
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/chunks/42`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    expect(res.body).toBe(chunk);
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
  });

  it("rejects non-identity Content-Encoding for chunk objects", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          return {
            Body: Readable.from([Buffer.from("0123")]),
            ContentType: "application/octet-stream",
            ContentLength: 4,
            ContentEncoding: "gzip",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/chunks/0`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(502);
    expect(res.json()).toMatchObject({ error: { code: "S3_ERROR" } });
  });

  it("accepts padded chunk filenames (00000000.bin) in the gateway route", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          expect(command.input.Key).toBe("images/user-1/image-1/v1/chunks/00000000.bin");
          return {
            ContentLength: 123,
            ContentType: "application/octet-stream",
            ETag: '"etag-chunk"',
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/chunks/00000000.bin`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["content-length"]).toBe("123");
  });

  it("supports HEAD for manifest and chunk objects", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          if (command.input.Key === "images/user-1/image-1/v1/manifest.json") {
            return {
              ContentLength: 10,
              ContentType: "application/json",
              ETag: '"etag-manifest"',
            };
          }
          if (command.input.Key === "images/user-1/image-1/v1/chunks/00000000.bin") {
            return {
              ContentLength: 123,
              ContentType: "application/octet-stream",
              ETag: '"etag-chunk"',
            };
          }
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const manifestRes = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });
    expect(manifestRes.statusCode).toBe(200);
    expect(manifestRes.headers["content-type"]).toBe("application/json");
    expect(manifestRes.headers["content-length"]).toBe("10");
    expect(manifestRes.headers["etag"]).toBe('"etag-manifest"');
    expect(manifestRes.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(manifestRes.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(manifestRes.headers["x-content-type-options"]).toBe("nosniff");

    const chunkRes = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/chunked/chunks/0`,
      headers: { "x-user-id": ownerId },
    });
    expect(chunkRes.statusCode).toBe(200);
    expect(chunkRes.headers["content-type"]).toBe("application/octet-stream");
    expect(chunkRes.headers["content-length"]).toBe("123");
    expect(chunkRes.headers["etag"]).toBe('"etag-chunk"');
    expect(chunkRes.headers["content-encoding"]).toBe("identity");
    expect(chunkRes.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(chunkRes.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(chunkRes.headers["x-content-type-options"]).toBe("nosniff");
  });

  it("returns a chunked manifest URL from /stream-url (CloudFront cookie mode)", async () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const config = makeConfig({
      cloudfrontDomain: "d111111abcdef8.cloudfront.net",
      cloudfrontKeyPairId: "KTESTKEYPAIR",
      cloudfrontPrivateKeyPem: privateKey,
      cloudfrontAuthMode: "cookie",
    });

    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/stream-url`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    const body = res.json() as { chunked?: { delivery: string; manifestUrl: string } };
    expect(body.chunked).toEqual({
      delivery: "chunked",
      manifestUrl:
        "https://d111111abcdef8.cloudfront.net/images/user-1/image-1/v1/manifest.json",
    });
    expect(res.headers["set-cookie"]).toBeTruthy();
  });

  it("returns a signed chunked manifest URL from /stream-url (CloudFront URL mode)", async () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const config = makeConfig({
      cloudfrontDomain: "d111111abcdef8.cloudfront.net",
      cloudfrontKeyPairId: "KTESTKEYPAIR",
      cloudfrontPrivateKeyPem: privateKey,
      cloudfrontAuthMode: "url",
    });

    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/stream-url`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    const body = res.json() as { chunked?: { delivery: string; manifestUrl: string } };
    expect(body.chunked?.delivery).toBe("chunked");
    expect(body.chunked?.manifestUrl).toMatch(
      new RegExp(`/v1/images/${imageId}/chunked/manifest$`)
    );
  });

  it("redirects chunked endpoints to CloudFront URLs when configured (cookie mode)", async () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const config = makeConfig({
      cloudfrontDomain: "d111111abcdef8.cloudfront.net",
      cloudfrontKeyPairId: "KTESTKEYPAIR",
      cloudfrontPrivateKeyPem: privateKey,
      cloudfrontAuthMode: "cookie",
    });
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = { async send() {} } as unknown as S3Client;
    const app = buildApp({ config, s3, store });
    await app.ready();

    const manifestRes = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });
    expect(manifestRes.statusCode).toBe(307);
    expect(manifestRes.headers.location).toBe(
      "https://d111111abcdef8.cloudfront.net/images/user-1/image-1/v1/manifest.json"
    );

    const chunkRes = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/chunks/0`,
      headers: { "x-user-id": ownerId },
    });
    expect(chunkRes.statusCode).toBe(307);
    expect(chunkRes.headers.location).toBe(
      "https://d111111abcdef8.cloudfront.net/images/user-1/image-1/v1/chunks/00000000.bin"
    );
  });

  it("redirects chunked endpoints to signed CloudFront URLs when configured (url mode)", async () => {
    const { privateKey } = generateKeyPairSync("rsa", {
      modulusLength: 2048,
      privateKeyEncoding: { type: "pkcs8", format: "pem" },
      publicKeyEncoding: { type: "spki", format: "pem" },
    });

    const config = makeConfig({
      cloudfrontDomain: "d111111abcdef8.cloudfront.net",
      cloudfrontKeyPairId: "KTESTKEYPAIR",
      cloudfrontPrivateKeyPem: privateKey,
      cloudfrontAuthMode: "url",
    });
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      chunkedPrefix: "images/user-1/image-1/v1/",
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = { async send() {} } as unknown as S3Client;
    const app = buildApp({ config, s3, store });
    await app.ready();

    const manifestRes = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/manifest`,
      headers: { "x-user-id": ownerId },
    });
    expect(manifestRes.statusCode).toBe(307);
    expect(manifestRes.headers.location).toContain(
      "https://d111111abcdef8.cloudfront.net/images/user-1/image-1/v1/manifest.json"
    );
    expect(manifestRes.headers.location).toContain("Key-Pair-Id=KTESTKEYPAIR");
    expect(manifestRes.headers.location).toContain("Signature=");

    const chunkRes = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/chunked/chunks/0`,
      headers: { "x-user-id": ownerId },
    });
    expect(chunkRes.statusCode).toBe(307);
    expect(chunkRes.headers.location).toContain(
      "https://d111111abcdef8.cloudfront.net/images/user-1/image-1/v1/chunks/00000000.bin"
    );
    expect(chunkRes.headers.location).toContain("Key-Pair-Id=KTESTKEYPAIR");
    expect(chunkRes.headers.location).toContain("Signature=");
  });
});
