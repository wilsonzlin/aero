import {
  CreateMultipartUploadCommand,
  GetObjectCommand,
  HeadBucketCommand,
  HeadObjectCommand,
} from "@aws-sdk/client-s3";
import type { S3Client } from "@aws-sdk/client-s3";
import { Readable } from "node:stream";
import { describe, expect, it } from "vitest";

import { buildApp } from "../src/app";
import {
  CACHE_CONTROL_PRIVATE_NO_STORE,
  CACHE_CONTROL_PUBLIC_IMMUTABLE,
  type Config,
} from "../src/config";
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

describe("app", () => {
  it("starts multipart uploads with anti-transform S3 metadata (default cache policy)", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();

    let createMultipartInput: CreateMultipartUploadCommand["input"] | undefined;

    const s3 = {
      async send(command: unknown) {
        if (command instanceof CreateMultipartUploadCommand) {
          createMultipartInput = command.input;
          return { UploadId: "upload-1" };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "POST",
      url: "/v1/images",
      headers: { "x-user-id": "user-1" },
    });

    expect(res.statusCode).toBe(200);

    expect(createMultipartInput).toBeDefined();
    expect(createMultipartInput).toMatchObject({
      Bucket: "bucket",
      ContentType: "application/octet-stream",
      CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
      ContentEncoding: "identity",
    });
    expect(createMultipartInput?.Key).toMatch(/^images\/user-1\//);
    expect(createMultipartInput?.Key).toMatch(/\/disk\.img$/);
  });

  it("supports public immutable cache-control for newly created objects", async () => {
    const config = makeConfig({ imageCacheControl: CACHE_CONTROL_PUBLIC_IMMUTABLE });
    const store = new MemoryImageStore();

    let createMultipartInput: CreateMultipartUploadCommand["input"] | undefined;

    const s3 = {
      async send(command: unknown) {
        if (command instanceof CreateMultipartUploadCommand) {
          createMultipartInput = command.input;
          return { UploadId: "upload-1" };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "POST",
      url: "/v1/images",
      headers: { "x-user-id": "user-1" },
    });

    expect(res.statusCode).toBe(200);
    expect(createMultipartInput).toMatchObject({
      CacheControl: CACHE_CONTROL_PUBLIC_IMMUTABLE,
    });
  });

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
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
            ContentEncoding: "identity",
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
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
    expect(res.headers["access-control-allow-credentials"]).toBe("true");
    expect(res.headers["access-control-expose-headers"]).toContain("accept-ranges");
    expect(res.headers["access-control-expose-headers"]).toContain("content-range");
    expect(res.headers["access-control-expose-headers"]).toContain("content-length");
    expect(res.headers["access-control-expose-headers"]).toContain("etag");
  });

  it("appends no-transform when S3 Cache-Control metadata is missing it", async () => {
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
            CacheControl: "private, no-store",
            ContentEncoding: "identity",
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
    expect(res.headers["cache-control"]).toBe("private, no-store, no-transform");
  });

  it("passes through cache-control + content-encoding on ranged GET responses", async () => {
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

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          return {
            Body: Readable.from([Buffer.from("0123")]),
            ContentRange: "bytes 0-3/4",
            ContentLength: 4,
            ETag: '"etag"',
            LastModified: lastModified,
            ContentType: "application/octet-stream",
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
            ContentEncoding: "identity",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-3",
      },
    });

    expect(res.statusCode).toBe(206);
    expect(res.headers["content-range"]).toBe("bytes 0-3/4");
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.payload).toBe("0123");
  });

  it("infers Content-Length from Content-Range when S3 omits ContentLength on a ranged response", async () => {
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

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          return {
            Body: Readable.from([Buffer.from("0123")]),
            ContentRange: "bytes 0-3/4",
            ETag: '"etag"',
            LastModified: lastModified,
            ContentType: "application/octet-stream",
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
            ContentEncoding: "identity",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-3",
      },
    });

    expect(res.statusCode).toBe(206);
    expect(res.headers["content-range"]).toBe("bytes 0-3/4");
    expect(res.headers["content-length"]).toBe("4");
    expect(res.payload).toBe("0123");
  });

  it("serves a full-body 200 response when Range is omitted", async () => {
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
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          expect(command.input.Range).toBeUndefined();
          return {
            Body: Readable.from([Buffer.from("0123")]),
            ContentLength: 4,
            ContentType: "application/octet-stream",
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
      },
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["content-length"]).toBe("4");
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.payload).toBe("0123");
  });

  it("rejects non-identity Content-Encoding from S3", async () => {
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
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          return {
            Body: Readable.from([Buffer.from("0123")]),
            ContentRange: "bytes 0-3/4",
            ContentLength: 4,
            ContentType: "application/octet-stream",
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
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-3",
      },
    });

    expect(res.statusCode).toBe(502);
    expect(res.json()).toMatchObject({ error: { code: "S3_ERROR" } });
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
      size: 100,
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

    expect(res.statusCode).toBe(416);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["content-range"]).toBe("bytes */100");
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("returns 416 with Content-Range size by probing S3 when image size is unknown", async () => {
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

    let headCalls = 0;
    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          headCalls += 1;
          return {
            ContentLength: 123,
            ETag: '"etag"',
            ContentType: "application/octet-stream",
          };
        }
        throw new Error("unexpected command");
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

    expect(headCalls).toBe(1);
    expect(res.statusCode).toBe(416);
    expect(res.headers["content-range"]).toBe("bytes */123");
    expect(store.get(imageId)?.size).toBe(123);
  });

  it("allows ranged GET when If-Range matches the current ETag", async () => {
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
      etag: '"etag"',
    });

    let getObjectCalls = 0;
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          getObjectCalls += 1;
          expect(command.input.Range).toBe("bytes=0-4");
          expect(command.input.IfMatch).toBe('"etag"');
          return {
            Body: Buffer.from("hello"),
            ContentRange: "bytes 0-4/10",
            ContentLength: 5,
            ETag: '"etag"',
            ContentType: "application/octet-stream",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-4",
        "if-range": '"etag"',
      },
    });

    expect(getObjectCalls).toBe(1);
    expect(res.statusCode).toBe(206);
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["content-range"]).toBe("bytes 0-4/10");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.payload).toBe("hello");
  });

  it("allows ranged GET when If-Range matches the current Last-Modified time (HTTP-date form)", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    // Use sub-second precision to ensure we compare at HTTP-date granularity (seconds).
    const lastModified = new Date("2020-01-01T00:00:00.456Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
    });

    const ifRangeValue = lastModified.toUTCString();
    const expectedIfUnmodifiedSince = new Date(ifRangeValue);

    let getObjectCalls = 0;
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          getObjectCalls += 1;
          expect(command.input.Range).toBe("bytes=0-4");
          expect(command.input.IfMatch).toBeUndefined();
          expect(command.input.IfUnmodifiedSince?.toISOString()).toBe(
            expectedIfUnmodifiedSince.toISOString()
          );
          return {
            Body: Buffer.from("hello"),
            ContentRange: "bytes 0-4/10",
            ContentLength: 5,
            ETag: '"etag"',
            ContentType: "application/octet-stream",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-4",
        "if-range": ifRangeValue,
      },
    });

    expect(getObjectCalls).toBe(1);
    expect(res.statusCode).toBe(206);
    expect(res.headers["content-range"]).toBe("bytes 0-4/10");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.payload).toBe("hello");
  });

  it("ignores Range and returns 200 when If-Range HTTP-date is older than Last-Modified", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    const lastModified = new Date("2020-01-01T00:00:01.000Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
    });

    const ifRangeValue = new Date("2020-01-01T00:00:00.000Z").toUTCString();

    let getObjectCalls = 0;
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          getObjectCalls += 1;
          expect(command.input.Range).toBeUndefined();
          expect(command.input.IfMatch).toBeUndefined();
          expect(command.input.IfUnmodifiedSince).toBeUndefined();
          return {
            Body: Buffer.from("hello"),
            ContentLength: 5,
            ETag: '"etag"',
            ContentType: "application/octet-stream",
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-4",
        "if-range": ifRangeValue,
      },
    });

    expect(getObjectCalls).toBe(1);
    expect(res.statusCode).toBe(200);
    expect(res.headers["content-range"]).toBeUndefined();
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.payload).toBe("hello");
  });

  it("ignores Range and returns 200 when If-Range does not match", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v2",
      s3Key: "images/user-1/image-1/v2/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag-current"',
      lastModified: new Date("2020-01-01T00:00:00.000Z").toISOString(),
    });

    let getObjectCalls = 0;
    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          getObjectCalls += 1;
          expect(command.input.Range).toBeUndefined();
          expect(command.input.IfMatch).toBeUndefined();
          return {
            Body: Readable.from([Buffer.from("hello")]),
            ContentLength: 5,
            ETag: '"etag-current"',
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
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-4",
        "if-range": '"etag-old"',
      },
    });

    expect(getObjectCalls).toBe(1);
    expect(res.statusCode).toBe(200);
    expect(res.headers["etag"]).toBe('"etag-current"');
    expect(res.headers["content-range"]).toBeUndefined();
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["access-control-expose-headers"]).toContain("etag");
    expect(res.payload).toBe("hello");
  });

  it("returns 304 for GET If-None-Match when validators match (no S3 call)", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for a satisfied conditional request");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        "if-none-match": '"etag"',
      },
    });

    expect(res.statusCode).toBe(304);
    expect(res.payload).toBe("");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["cache-control"]).toBe("private, no-store, no-transform");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("returns 304 for GET If-None-Match when a weak validator matches (no S3 call)", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for a satisfied conditional request");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        "if-none-match": 'W/"etag"',
      },
    });

    expect(res.statusCode).toBe(304);
    expect(res.payload).toBe("");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["cache-control"]).toBe("private, no-store, no-transform");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("handles commas inside quoted entity-tags in If-None-Match", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"a,b"',
      lastModified: lastModified.toISOString(),
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for a satisfied conditional request");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        // Commas are valid inside a quoted entity-tag.
        "if-none-match": '"a,b"',
      },
    });

    expect(res.statusCode).toBe(304);
    expect(res.payload).toBe("");
    expect(res.headers["etag"]).toBe('"a,b"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["cache-control"]).toBe("private, no-store, no-transform");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("returns 304 for GET If-Modified-Since when last-modified is not newer (no S3 call)", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    // Use sub-second precision to ensure we don't accidentally compare timestamps at millisecond
    // resolution (HTTP-date is only second-granular).
    const lastModified = new Date("2020-01-01T00:00:00.456Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for a satisfied conditional request");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        "if-modified-since": lastModified.toUTCString(),
      },
    });

    expect(res.statusCode).toBe(304);
    expect(res.payload).toBe("");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["cache-control"]).toBe("private, no-store, no-transform");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("returns 304 for HEAD If-Modified-Since when last-modified is not newer", async () => {
    const config = makeConfig();
    const store = new MemoryImageStore();
    const ownerId = "user-1";
    const imageId = "image-1";

    const lastModified = new Date("2020-01-01T00:00:00.789Z");
    store.create({
      id: imageId,
      ownerId,
      createdAt: new Date().toISOString(),
      version: "v1",
      s3Key: "images/user-1/image-1/v1/disk.img",
      uploadId: "upload-1",
      status: "complete",
      etag: '"etag"',
      lastModified: lastModified.toISOString(),
      size: 123,
    });

    const s3 = {
      async send(command: unknown) {
        if (command instanceof HeadObjectCommand) {
          return {
            ContentLength: 123,
            ETag: '"etag"',
            LastModified: lastModified,
            ContentType: "application/octet-stream",
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
            ContentEncoding: "identity",
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
      headers: {
        "x-user-id": ownerId,
        "if-modified-since": lastModified.toUTCString(),
      },
    });

    expect(res.statusCode).toBe(304);
    expect(res.payload).toBe("");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
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
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
  });

  it("retries without Range when S3 returns 412 for IfMatch (If-Range race)", async () => {
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
      etag: '"etag"',
    });

    let getObjectCalls = 0;
    const s3 = {
      async send(command: unknown) {
        if (command instanceof GetObjectCommand) {
          getObjectCalls += 1;
          if (getObjectCalls === 1) {
            expect(command.input.Range).toBe("bytes=0-4");
            expect(command.input.IfMatch).toBe('"etag"');
            const err = new Error("PreconditionFailed") as Error & {
              $metadata?: { httpStatusCode?: number };
            };
            err.$metadata = { httpStatusCode: 412 };
            throw err;
          }
          expect(command.input.Range).toBeUndefined();
          expect(command.input.IfMatch).toBeUndefined();
          return {
            Body: Readable.from([Buffer.from("hello")]),
            ContentLength: 5,
            ETag: '"etag-new"',
            ContentType: "application/octet-stream",
            CacheControl: CACHE_CONTROL_PRIVATE_NO_STORE,
          };
        }
        throw new Error("unexpected command");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: "bytes=0-4",
        "if-range": '"etag"',
      },
    });

    expect(getObjectCalls).toBe(2);
    expect(res.statusCode).toBe(200);
    expect(res.headers["content-range"]).toBeUndefined();
    expect(res.headers["etag"]).toBe('"etag-new"');
    expect(res.headers["cache-control"]).toBe(CACHE_CONTROL_PRIVATE_NO_STORE);
    expect(res.payload).toBe("hello");
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

  it("rejects overly long request URLs with 414 (and still applies CORS headers)", async () => {
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

    const qs = "a".repeat(9_000);
    const res = await app.inject({
      method: "GET",
      url: `/healthz?${qs}`,
      headers: { origin: "http://localhost:5173" },
    });

    expect(res.statusCode).toBe(414);
    expect(res.json()).toMatchObject({ error: { code: "URL_TOO_LONG" } });
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
    expect(res.headers["access-control-allow-credentials"]).toBe("true");
  });

  it("does not reflect oversized Access-Control-Request-Headers", async () => {
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

    const huge = "a".repeat(5_000);
    const res = await app.inject({
      method: "OPTIONS",
      url: "/v1/images",
      headers: {
        origin: "http://localhost:5173",
        "access-control-request-method": "POST",
        "access-control-request-headers": huge,
      },
    });

    expect(res.statusCode).toBe(204);
    expect(res.headers["access-control-allow-origin"]).toBe("http://localhost:5173");
    expect(res.headers["access-control-allow-headers"]).toBe("range,if-range,content-type,x-user-id");
  });

  it("rejects oversized Range headers without calling S3", async () => {
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
    });

    const s3 = {
      async send() {
        throw new Error("S3 should not be called for oversized Range headers");
      },
    } as unknown as S3Client;

    const app = buildApp({ config, s3, store });
    await app.ready();

    const res = await app.inject({
      method: "GET",
      url: `/v1/images/${imageId}/range`,
      headers: {
        "x-user-id": ownerId,
        range: `bytes=0-${"0".repeat(20_000)}`,
      },
    });

    expect(res.statusCode).toBe(413);
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
  });
});
