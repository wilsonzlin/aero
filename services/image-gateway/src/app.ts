import {
  CompleteMultipartUploadCommand,
  CreateMultipartUploadCommand,
  GetObjectCommand,
  type GetObjectCommandOutput,
  HeadBucketCommand,
  HeadObjectCommand,
  type HeadObjectCommandOutput,
  UploadPartCommand,
  type S3Client,
} from "@aws-sdk/client-s3";
import { getSignedUrl as getS3SignedUrl } from "@aws-sdk/s3-request-presigner";
import type { FastifyInstance, FastifyReply, FastifyRequest } from "fastify";
import fastify from "fastify";
import { randomUUID } from "node:crypto";

import { getCallerUserId } from "./auth";
import type { Config } from "./config";
import {
  assertCloudFrontSigningConfiguredForConfig,
  buildCloudFrontUrl,
  createSignedCookies,
  createSignedUrl,
  formatSetCookie,
  type StreamAuth,
} from "./cloudfront";
import { ApiError } from "./errors";
import { buildRangeProxyResponse } from "./rangeProxy";
import type { ImageRecord, ImageStore } from "./store";
import { buildImageObjectKey } from "./s3";

export interface BuildAppDeps {
  config: Config;
  s3: S3Client;
  store: ImageStore;
}

const S3_MULTIPART_MAX_PARTS = 10_000;

function assertBodyObject(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  body: any
): asserts body is Record<string, unknown> {
  if (!body || typeof body !== "object") {
    throw new ApiError(400, "Invalid JSON body", "BAD_REQUEST");
  }
}

function normalizeEtag(etag: string): string {
  const trimmed = etag.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) return trimmed;
  return `"${trimmed.replace(/"/g, "")}"`;
}

function requireImage(store: ImageStore, imageId: string): ImageRecord {
  const record = store.get(imageId);
  if (!record) throw new ApiError(404, "Image not found", "NOT_FOUND");
  return record;
}

function assertOwner(record: ImageRecord, callerUserId: string): void {
  if (record.ownerId !== callerUserId) {
    throw new ApiError(403, "Forbidden", "FORBIDDEN");
  }
}

function buildStableImagePath(config: Config, record: ImageRecord): string {
  return `${config.imageBasePath}/${record.ownerId}/${record.id}/${record.version}/disk.img`;
}

function applyCorsHeaders(reply: FastifyReply, config: Config): void {
  const allowOrigin = config.corsAllowOrigin;
  reply
    .header("access-control-allow-origin", allowOrigin)
    // Range reads require exposing non-safelisted headers; harmless for non-range endpoints.
    .header(
      "access-control-expose-headers",
      "accept-ranges,content-range,content-length,etag,last-modified"
    );

  if (allowOrigin !== "*") {
    reply.header("access-control-allow-credentials", "true").header("vary", "origin");
  }
}

function applyCorsPreflight(req: FastifyRequest, reply: FastifyReply): void {
  const requestedHeaders = req.headers["access-control-request-headers"];
  reply
    .header("access-control-allow-methods", "GET,HEAD,POST,OPTIONS")
    .header(
      "access-control-allow-headers",
      typeof requestedHeaders === "string"
        ? requestedHeaders
        : "range,if-range,content-type,x-user-id"
    )
    .header("access-control-max-age", "86400");

  reply.header("vary", "Origin, Access-Control-Request-Method, Access-Control-Request-Headers");
}

function buildSelfUrl(req: FastifyRequest, path: string): string {
  const rawHost = req.headers["x-forwarded-host"] ?? req.headers.host;
  const host =
    typeof rawHost === "string"
      ? rawHost.split(",")[0].trim()
      : Array.isArray(rawHost)
        ? rawHost[0]
        : undefined;

  const rawProto = req.headers["x-forwarded-proto"];
  const proto =
    typeof rawProto === "string"
      ? rawProto.split(",")[0].trim()
      : Array.isArray(rawProto)
        ? rawProto[0]
        : req.protocol;

  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  if (!host) return normalizedPath;
  return `${proto}://${host}${normalizedPath}`;
}

export function buildApp(deps: BuildAppDeps): FastifyInstance {
  const app = fastify({
    logger: process.env.NODE_ENV !== "test",
  });

  app.addHook("onRequest", async (req, reply) => {
    applyCorsHeaders(reply, deps.config);

    if (req.method === "OPTIONS") {
      applyCorsPreflight(req, reply);
      return reply.status(204).send();
    }
  });

  app.setErrorHandler((err, _req, reply) => {
    const statusCode =
      typeof (err as Partial<ApiError>).statusCode === "number"
        ? (err as ApiError).statusCode
        : 500;
    const code =
      typeof (err as Partial<ApiError>).code === "string"
        ? (err as ApiError).code
        : "INTERNAL";

    const message =
      statusCode >= 500
        ? "Internal Server Error"
        : err.message || "Request failed";

    if (statusCode >= 500) {
      app.log.error({ err });
    }

    reply.status(statusCode).send({ error: { code, message } });
  });

  app.get("/health", async () => ({ ok: true }));
  app.get("/healthz", async () => ({ ok: true }));

  app.get("/readyz", async () => {
    try {
      await deps.s3.send(
        new HeadBucketCommand({
          Bucket: deps.config.s3Bucket,
        })
      );
    } catch {
      throw new ApiError(503, "S3 bucket not reachable", "S3_UNAVAILABLE");
    }

    return { ok: true };
  });

  app.post("/v1/images", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);

    const imageId = randomUUID();
    const version = randomUUID();
    const createdAt = new Date().toISOString();

    const s3Key = buildImageObjectKey({
      imageBasePath: deps.config.imageBasePath,
      ownerId: callerUserId,
      imageId,
      version,
    });

    const res = await deps.s3.send(
      new CreateMultipartUploadCommand({
        Bucket: deps.config.s3Bucket,
        Key: s3Key,
        ContentType: "application/octet-stream",
      })
    );

    const uploadId: string | undefined = res.UploadId;
    if (!uploadId) {
      throw new ApiError(502, "S3 did not return an UploadId", "S3_ERROR");
    }

    deps.store.create({
      id: imageId,
      ownerId: callerUserId,
      createdAt,
      version,
      s3Key,
      uploadId,
      status: "uploading",
    });

    reply.send({ imageId, uploadId, partSize: deps.config.partSizeBytes });
  });

  app.post("/v1/images/:imageId/upload-url", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    assertBodyObject(req.body);
    const uploadId = req.body.uploadId;
    const partNumber = req.body.partNumber;

    if (typeof uploadId !== "string" || !uploadId) {
      throw new ApiError(400, "uploadId must be a string", "BAD_REQUEST");
    }
    if (typeof partNumber !== "number" || !Number.isInteger(partNumber) || partNumber <= 0) {
      throw new ApiError(400, "partNumber must be a positive integer", "BAD_REQUEST");
    }
    if (partNumber > S3_MULTIPART_MAX_PARTS) {
      throw new ApiError(
        400,
        `partNumber must be between 1 and ${S3_MULTIPART_MAX_PARTS}`,
        "BAD_REQUEST"
      );
    }
    if (record.status !== "uploading") {
      throw new ApiError(409, "Image is not in uploading state", "INVALID_STATE");
    }
    if (record.uploadId !== uploadId) {
      throw new ApiError(400, "uploadId does not match image record", "BAD_REQUEST");
    }

    const url = await getS3SignedUrl(
      deps.s3,
      new UploadPartCommand({
        Bucket: deps.config.s3Bucket,
        Key: record.s3Key,
        UploadId: uploadId,
        PartNumber: partNumber,
      }),
      { expiresIn: 60 * 60 }
    );

    reply.send({ url });
  });

  app.post("/v1/images/:imageId/complete", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    assertBodyObject(req.body);
    const uploadId = req.body.uploadId;
    const parts = req.body.parts;

    if (typeof uploadId !== "string" || !uploadId) {
      throw new ApiError(400, "uploadId must be a string", "BAD_REQUEST");
    }
    if (!Array.isArray(parts) || parts.length === 0) {
      throw new ApiError(400, "parts must be a non-empty array", "BAD_REQUEST");
    }
    if (parts.length > S3_MULTIPART_MAX_PARTS) {
      throw new ApiError(
        400,
        `parts must have at most ${S3_MULTIPART_MAX_PARTS} entries`,
        "BAD_REQUEST"
      );
    }
    if (record.status !== "uploading") {
      throw new ApiError(409, "Image is not in uploading state", "INVALID_STATE");
    }
    if (record.uploadId !== uploadId) {
      throw new ApiError(400, "uploadId does not match image record", "BAD_REQUEST");
    }

    const normalizedParts = parts
      .map((p) => {
        if (!p || typeof p !== "object") {
          throw new ApiError(400, "Invalid parts entry", "BAD_REQUEST");
        }
        const partNumber = (p as { partNumber?: unknown }).partNumber;
        const etag = (p as { etag?: unknown }).etag;
        if (
          typeof partNumber !== "number" ||
          !Number.isInteger(partNumber) ||
          partNumber <= 0
        ) {
          throw new ApiError(400, "Invalid partNumber", "BAD_REQUEST");
        }
        if (partNumber > S3_MULTIPART_MAX_PARTS) {
          throw new ApiError(
            400,
            `partNumber must be between 1 and ${S3_MULTIPART_MAX_PARTS}`,
            "BAD_REQUEST"
          );
        }
        if (typeof etag !== "string" || !etag) {
          throw new ApiError(400, "Invalid etag", "BAD_REQUEST");
        }
        return { PartNumber: partNumber, ETag: normalizeEtag(etag) };
      })
      .sort((a, b) => a.PartNumber - b.PartNumber);

    const seenPartNumbers = new Set<number>();
    for (const part of normalizedParts) {
      if (seenPartNumbers.has(part.PartNumber)) {
        throw new ApiError(400, "Duplicate partNumber in parts array", "BAD_REQUEST");
      }
      seenPartNumbers.add(part.PartNumber);
    }

    await deps.s3.send(
      new CompleteMultipartUploadCommand({
        Bucket: deps.config.s3Bucket,
        Key: record.s3Key,
        UploadId: uploadId,
        MultipartUpload: { Parts: normalizedParts },
      })
    );

    const head = await deps.s3.send(
      new HeadObjectCommand({
        Bucket: deps.config.s3Bucket,
        Key: record.s3Key,
      })
    );

    const size = typeof head.ContentLength === "number" ? head.ContentLength : undefined;
    const etag = typeof head.ETag === "string" ? head.ETag : undefined;
    const lastModified =
      head.LastModified instanceof Date ? head.LastModified.toISOString() : undefined;

    deps.store.update(imageId, {
      status: "complete",
      size,
      etag,
      lastModified,
    });

    reply.send({ ok: true, size, etag, lastModified });
  });

  app.get("/v1/images/:imageId/metadata", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    const path = buildStableImagePath(deps.config, record);
    const url = deps.config.cloudfrontDomain
      ? buildCloudFrontUrl({ cloudfrontDomain: deps.config.cloudfrontDomain, path })
      : buildSelfUrl(req, `/v1/images/${imageId}/range`);

    reply.send({
      size: record.size,
      etag: record.etag,
      lastModified: record.lastModified,
      url,
    });
  });

  app.get("/v1/images/:imageId/stream-url", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const path = buildStableImagePath(deps.config, record);

    let url: string;
    let auth: StreamAuth;

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);

      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path,
      });

      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);

      if (deps.config.cloudfrontAuthMode === "cookie") {
        const cookies = createSignedCookies({
          url: stableUrl,
          keyPairId: deps.config.cloudfrontKeyPairId,
          privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
          expiresAt,
          cookieDomain: deps.config.cloudfrontCookieDomain,
          cookiePath: deps.config.imageBasePath,
        });

        const setCookie = cookies.map(formatSetCookie);
        reply.header("set-cookie", setCookie);

        url = stableUrl;
        auth = { type: "cookie", cookies, expiresAt: expiresAt.toISOString() };
      } else {
        const signedUrl = createSignedUrl({
          url: stableUrl,
          keyPairId: deps.config.cloudfrontKeyPairId,
          privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
          expiresAt,
        });

        url = signedUrl;
        auth = { type: "url", expiresAt: expiresAt.toISOString() };
      }
    } else {
      // Local/dev fallback: stream via the range proxy endpoint on the same host.
      // This path is stable but does proxy bytes through the service.
      url = buildSelfUrl(req, `/v1/images/${imageId}/range`);
      auth = { type: "none" };
    }

    reply.send({
      url,
      auth,
      size: record.size,
      etag: record.etag,
    });
  });

  app.get("/v1/images/:imageId/range", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const requestedRange = typeof req.headers.range === "string" ? req.headers.range : undefined;
    if (requestedRange) {
      if (!requestedRange.startsWith("bytes=")) {
        throw new ApiError(400, "Only bytes ranges are supported", "BAD_REQUEST");
      }
      if (requestedRange.includes(",")) {
        throw new ApiError(
          400,
          "Only single-range requests are supported",
          "BAD_REQUEST"
        );
      }
    }

    let s3Res: GetObjectCommandOutput;
    try {
      s3Res = await deps.s3.send(
        new GetObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: record.s3Key,
          Range: requestedRange,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number") {
        if (maybeStatus === 416) {
          throw new ApiError(416, "Requested Range Not Satisfiable", "INVALID_RANGE");
        }
        if (maybeStatus === 404) {
          throw new ApiError(404, "Image object not found", "NOT_FOUND");
        }
        if (maybeStatus >= 400 && maybeStatus < 500) {
          throw new ApiError(maybeStatus, "S3 request rejected", "S3_ERROR");
        }
      }
      throw err;
    }

    if (!s3Res.Body) {
      throw new ApiError(502, "S3 did not return a response body", "S3_ERROR");
    }

    const proxy = buildRangeProxyResponse({ s3: s3Res });

    reply
      .status(proxy.statusCode)
      .headers(proxy.headers);

    return reply.send(s3Res.Body);
  });

  app.head("/v1/images/:imageId/range", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    let head: HeadObjectCommandOutput;
    try {
      head = await deps.s3.send(
        new HeadObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: record.s3Key,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number" && maybeStatus === 404) {
        throw new ApiError(404, "Image object not found", "NOT_FOUND");
      }
      throw err;
    }

    const proxy = buildRangeProxyResponse({
      s3: {
        ContentLength: head.ContentLength,
        ContentRange: undefined,
        ETag: head.ETag,
        LastModified: head.LastModified,
        ContentType: head.ContentType,
      },
    });

    reply.status(proxy.statusCode).headers(proxy.headers).send();
  });

  return app;
}
