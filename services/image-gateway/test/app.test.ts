import { HeadBucketCommand, HeadObjectCommand } from "@aws-sdk/client-s3";
import type { S3Client } from "@aws-sdk/client-s3";
import { describe, expect, it } from "vitest";

import { buildApp } from "../src/app";
import type { Config } from "../src/config";
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
    cloudfrontSignedTtlSeconds: 60,

    imageBasePath: "/images",
    partSizeBytes: 64 * 1024 * 1024,

    authMode: "dev",
    port: 0,
    corsAllowOrigin: "http://localhost:5173",

    ...overrides,
  };
}

describe("app", () => {
  it("serves HEAD /v1/images/:id/range with size + validator headers", async () => {
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
      uploadId: "upload-1",
      status: "complete",
      size: 123,
      etag: '"etag"',
      lastModified: new Date("2020-01-01T00:00:00.000Z").toISOString(),
    });

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadBucketCommand) return {};
        if (command instanceof HeadObjectCommand) {
          return {
            ContentLength: 123,
            ETag: '"etag"',
            LastModified: lastModified,
            ContentType: "application/octet-stream",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "HEAD",
      url: `/v1/images/${imageId}/range`,
      headers: { "x-user-id": ownerId },
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["content-length"]).toBe("123");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
    expect(res.headers["access-control-allow-credentials"]).toBe("true");
  });

  it("rejects multi-range requests", async () => {
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
      uploadId: "upload-1",
      status: "complete",
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for invalid ranges");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-0,2-3",
      },
    });

    expect(res.statusCode).toBe(400);
    expect(res.json()).toMatchObject({
      error: { code: "BAD_REQUEST" },
    });
  });

  it("returns 416 with RFC-style Content-Range when S3 rejects the range", async () => {
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
      uploadId: "upload-1",
      status: "complete",
      size: 100,
    });

    const s3 = {
      async send() {
        const err = new Error("InvalidRange") as Error & {
          $metadata?: { httpStatusCode?: number };
        };
        err.$metadata = { httpStatusCode: 416 };
        throw err;
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=999-1000",
      },
    });

    expect(res.statusCode).toBe(416);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["content-range"]).toBe("bytes */100");
  });

  it("handles CORS preflight with OPTIONS", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadBucketCommand) return {};
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "OPTIONS",
      url: "/v1/images",
      headers: {
        origin: "http://localhost:5173",
        "access-control-request-method": "POST",
        "access-control-request-headers": "content-type,x-user-id",
      },
    });

    expect(res.statusCode).toBe(204);
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
    expect(res.headers["access-control-allow-methods"]).toContain("POST");
    expect(res.headers["access-control-allow-headers"]).toBe("content-type,x-user-id");
    expect(res.headers["access-control-allow-credentials"]).toBe("true");
  });
});
