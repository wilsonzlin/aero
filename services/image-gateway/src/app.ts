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
import { DISK_BYTES_CONTENT_TYPE, buildRangeProxyHeaders, buildRangeProxyResponse } from "./rangeProxy";
import type { ImageRecord, ImageStore } from "./store";
import { buildImageObjectKey } from "./s3";

export interface BuildAppDeps {
  config: Config;
  s3: S3Client;
  store: ImageStore;
}

const S3_MULTIPART_MAX_PARTS = 10_000;
const CHUNK_INDEX_WIDTH = 8;

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

function ensureNoTransformCacheControl(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "no-transform";
  const directives = trimmed.split(",").map((directive) => directive.trim().toLowerCase());
  if (directives.includes("no-transform")) return trimmed;
  return `${trimmed}, no-transform`;
}

function stripWeakEtagPrefix(value: string): string {
  const trimmed = value.trim();
  return trimmed.replace(/^w\//i, "");
}

function ifNoneMatchMatches(ifNoneMatch: string, currentEtag: string): boolean {
  const raw = ifNoneMatch.trim();
  if (!raw) return false;
  if (raw === "*") return true;

  const current = stripWeakEtagPrefix(normalizeEtag(currentEtag));
  for (const part of raw.split(",")) {
    const candidate = part.trim();
    if (!candidate) continue;
    if (candidate === "*") return true;
    if (stripWeakEtagPrefix(normalizeEtag(candidate)) === current) return true;
  }
  return false;
}

function ifModifiedSinceAllowsNotModified(ifModifiedSince: string, lastModified: Date): boolean {
  const imsMillis = Date.parse(ifModifiedSince);
  if (!Number.isFinite(imsMillis)) return false;
  // HTTP-date has 1-second resolution. S3/MinIO may return LastModified with sub-second
  // precision, but if we compare millisecond timestamps directly we'll sometimes fail to return
  // 304 even when the client used our own Last-Modified header value.
  const lastSeconds = Math.floor(lastModified.getTime() / 1000);
  const imsSeconds = Math.floor(imsMillis / 1000);
  return lastSeconds <= imsSeconds;
}

function assertIdentityContentEncoding(value: string | undefined): void {
  if (!value) return;
  const normalized = value.trim().toLowerCase();
  if (normalized === "identity") return;
  throw new ApiError(
    502,
    `S3 returned Content-Encoding (${value}), but disk streaming requires identity`,
    "S3_ERROR"
  );
}

function parseSingleByteRangeHeader(
  value: string
): { start: bigint; end?: bigint; normalized: string } | undefined {
  const trimmed = value.trim();
  const match = /^bytes=(\d+)-(\d*)$/i.exec(trimmed);
  if (!match) return undefined;
  const startStr = match[1];
  const endStr = match[2];
  const start = BigInt(startStr);
  const end = endStr ? BigInt(endStr) : undefined;
  if (end !== undefined && end < start) return undefined;
  return {
    start,
    end,
    normalized: `bytes=${startStr}-${endStr}`,
  };
}

async function sendRangeNotSatisfiable(params: {
  reply: FastifyReply;
  config: Config;
  s3: S3Client;
  store: ImageStore;
  imageId: string;
  record: ImageRecord;
}): Promise<void> {
  let totalSize = params.record.size;
  if (typeof totalSize !== "number") {
    let head: HeadObjectCommandOutput;
    try {
      head = await params.s3.send(
        new HeadObjectCommand({
          Bucket: params.config.s3Bucket,
          Key: params.record.s3Key,
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

    totalSize = typeof head.ContentLength === "number" ? head.ContentLength : undefined;
    const etag = typeof head.ETag === "string" ? head.ETag : undefined;
    const lastModified =
      head.LastModified instanceof Date ? head.LastModified.toISOString() : undefined;

    params.store.update(params.imageId, {
      size: totalSize,
      etag,
      lastModified,
    });
  }

  const headers = buildRangeProxyHeaders({
    contentType: undefined,
    crossOriginResourcePolicy: params.config.crossOriginResourcePolicy,
  });
  if (typeof totalSize === "number") {
    headers["content-range"] = `bytes */${totalSize}`;
  }

  params.reply.status(416).headers(headers).send();
}

function sendIfRangePreconditionFailed(
  reply: FastifyReply,
  params: { etag: string; crossOriginResourcePolicy: Config["crossOriginResourcePolicy"] }
): void {
  reply
    .status(412)
    .headers(
      buildRangeProxyHeaders({
        contentType: "application/json",
        crossOriginResourcePolicy: params.crossOriginResourcePolicy,
      })
    )
    .header("etag", normalizeEtag(params.etag))
    .send({
      error: {
        code: "PRECONDITION_FAILED",
        message: "If-Range does not match current ETag",
      },
    });
}

function sendNotModified(params: {
  reply: FastifyReply;
  etag?: string;
  lastModified?: Date;
  cacheControl?: string;
  crossOriginResourcePolicy: Config["crossOriginResourcePolicy"];
}): void {
  const headers = buildRangeProxyHeaders({
    contentType: DISK_BYTES_CONTENT_TYPE,
    crossOriginResourcePolicy: params.crossOriginResourcePolicy,
  });
  if (params.etag) headers["etag"] = normalizeEtag(params.etag);
  if (params.lastModified) headers["last-modified"] = params.lastModified.toUTCString();
  if (params.cacheControl) {
    headers["cache-control"] = ensureNoTransformCacheControl(params.cacheControl);
  }

  params.reply.status(304).headers(headers).send();
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

function normalizeS3Key(value: string): string {
  return value.replace(/^\/+/, "");
}

function normalizeS3Prefix(value: string): string {
  let prefix = normalizeS3Key(value);
  if (prefix && !prefix.endsWith("/")) prefix = `${prefix}/`;
  return prefix;
}

function getChunkedBasePrefix(record: ImageRecord): string | undefined {
  if (record.chunkedManifestKey) {
    const key = normalizeS3Key(record.chunkedManifestKey);
    const lastSlash = key.lastIndexOf("/");
    return lastSlash === -1 ? "" : key.slice(0, lastSlash + 1);
  }
  if (record.chunkedPrefix) {
    return normalizeS3Prefix(record.chunkedPrefix);
  }
  return undefined;
}

function getChunkedManifestKey(record: ImageRecord): string | undefined {
  if (record.chunkedManifestKey) {
    return normalizeS3Key(record.chunkedManifestKey);
  }
  const prefix = getChunkedBasePrefix(record);
  if (prefix === undefined) return undefined;
  return `${prefix}manifest.json`;
}

function formatChunkObjectName(chunkIndex: number): string {
  if (!Number.isInteger(chunkIndex) || chunkIndex < 0) {
    throw new ApiError(400, "chunkIndex must be a non-negative integer", "BAD_REQUEST");
  }
  const maxIndex = 10 ** CHUNK_INDEX_WIDTH - 1;
  if (chunkIndex > maxIndex) {
    throw new ApiError(400, `chunkIndex must be <= ${maxIndex}`, "BAD_REQUEST");
  }
  return String(chunkIndex).padStart(CHUNK_INDEX_WIDTH, "0");
}

function parseChunkIndexParam(raw: string): number {
  const match = raw.match(/^(\d+)(?:\.bin)?$/);
  if (!match) {
    throw new ApiError(400, "chunkIndex must be a non-negative integer", "BAD_REQUEST");
  }
  const chunkIndex = Number.parseInt(match[1]!, 10);
  if (!Number.isFinite(chunkIndex) || !Number.isInteger(chunkIndex) || chunkIndex < 0) {
    throw new ApiError(400, "chunkIndex must be a non-negative integer", "BAD_REQUEST");
  }
  return chunkIndex;
}

function getChunkObjectKey(record: ImageRecord, chunkIndex: number): string | undefined {
  const prefix = getChunkedBasePrefix(record);
  if (prefix === undefined) return undefined;
  const name = formatChunkObjectName(chunkIndex);
  return `${prefix}chunks/${name}.bin`;
}

function buildChunkedCacheControl(config: Config): string {
  // Mirror the main disk image cache policy for chunked artifacts, but always enforce `no-transform`
  // (chunks are binary disk bytes; intermediaries must not apply transforms/compression).
  return ensureNoTransformCacheControl(config.imageCacheControl);
}

function buildChunkedProxyHeaders(params: {
  contentType: string;
  cacheControl: string;
  crossOriginResourcePolicy: Config["crossOriginResourcePolicy"];
  contentEncoding?: string;
}): Record<string, string> {
  const headers: Record<string, string> = {
    "cache-control": params.cacheControl,
    "content-type": params.contentType,
    "x-content-type-options": "nosniff",
    "cross-origin-resource-policy": params.crossOriginResourcePolicy,
  };
  if (params.contentEncoding) {
    headers["content-encoding"] = params.contentEncoding;
  }
  return headers;
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
        CacheControl: deps.config.imageCacheControl,
        ContentEncoding: "identity",
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
    let chunked:
      | {
          delivery: "chunked";
          manifestUrl: string;
        }
      | undefined;

    const chunkedManifestKey = getChunkedManifestKey(record);

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);

      const imagePrefixPath = `${deps.config.imageBasePath}/${record.ownerId}/${record.id}/${record.version}/*`;
      const policyUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path: imagePrefixPath,
      });

      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path,
      });

      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);

      if (deps.config.cloudfrontAuthMode === "cookie") {
        const cookies = createSignedCookies({
          url: policyUrl,
          keyPairId: deps.config.cloudfrontKeyPairId,
          privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
          expiresAt,
          cookieDomain: deps.config.cloudfrontCookieDomain,
          cookiePath: deps.config.imageBasePath,
          cookieSameSite: deps.config.cloudfrontCookieSameSite,
          cookiePartitioned: deps.config.cloudfrontCookiePartitioned,
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

      if (chunkedManifestKey) {
        if (deps.config.cloudfrontAuthMode === "cookie") {
          const stableManifestUrl = buildCloudFrontUrl({
            cloudfrontDomain: deps.config.cloudfrontDomain,
            path: `/${chunkedManifestKey}`,
          });
          chunked = { delivery: "chunked", manifestUrl: stableManifestUrl };
        } else {
          // CloudFront signed URLs embed auth in the query string, but relative URL resolution does not
          // propagate query params to chunk URLs. Use the gateway chunk endpoints (which redirect
          // to signed CloudFront URLs) so clients can fetch chunks with plain GETs.
          chunked = {
            delivery: "chunked",
            manifestUrl: buildSelfUrl(req, `/v1/images/${imageId}/chunked/manifest`),
          };
        }
      }
    } else {
      // Local/dev fallback: stream via the range proxy endpoint on the same host.
      // This path is stable but does proxy bytes through the service.
      url = buildSelfUrl(req, `/v1/images/${imageId}/range`);
      auth = { type: "none" };

      if (chunkedManifestKey) {
        chunked = {
          delivery: "chunked",
          manifestUrl: buildSelfUrl(req, `/v1/images/${imageId}/chunked/manifest`),
        };
      }
    }

    reply.header("cache-control", "no-store").send({
      url,
      auth,
      size: record.size,
      etag: record.etag,
      chunked,
    });
  });

  app.get("/v1/images/:imageId/chunked/manifest", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const key = getChunkedManifestKey(record);
    if (!key) {
      throw new ApiError(404, "Chunked manifest not available", "NOT_FOUND");
    }

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);
      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path: `/${key}`,
      });
      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);
      const url =
        deps.config.cloudfrontAuthMode === "url"
          ? createSignedUrl({
              url: stableUrl,
              keyPairId: deps.config.cloudfrontKeyPairId,
              privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
              expiresAt,
            })
          : stableUrl;
      reply.header("cache-control", "no-store").redirect(url, 307);
      return;
    }

    let s3Res: GetObjectCommandOutput;
    try {
      s3Res = await deps.s3.send(
        new GetObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: key,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number") {
        if (maybeStatus === 404) {
          throw new ApiError(404, "Chunked manifest not found", "NOT_FOUND");
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

    const headers = buildChunkedProxyHeaders({
      contentType: s3Res.ContentType ?? "application/json",
      cacheControl: buildChunkedCacheControl(deps.config),
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      contentEncoding: s3Res.ContentEncoding,
    });
    if (typeof s3Res.ContentLength === "number") {
      headers["content-length"] = String(s3Res.ContentLength);
    }
    if (s3Res.ETag) headers["etag"] = s3Res.ETag;
    if (s3Res.LastModified) headers["last-modified"] = s3Res.LastModified.toUTCString();

    reply.status(200).headers(headers);
    return reply.send(s3Res.Body);
  });

  app.head("/v1/images/:imageId/chunked/manifest", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const key = getChunkedManifestKey(record);
    if (!key) {
      throw new ApiError(404, "Chunked manifest not available", "NOT_FOUND");
    }

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);
      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path: `/${key}`,
      });
      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);
      const url =
        deps.config.cloudfrontAuthMode === "url"
          ? createSignedUrl({
              url: stableUrl,
              keyPairId: deps.config.cloudfrontKeyPairId,
              privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
              expiresAt,
            })
          : stableUrl;
      reply.header("cache-control", "no-store").redirect(url, 307);
      return;
    }

    let head: HeadObjectCommandOutput;
    try {
      head = await deps.s3.send(
        new HeadObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: key,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number" && maybeStatus === 404) {
        throw new ApiError(404, "Chunked manifest not found", "NOT_FOUND");
      }
      throw err;
    }

    const headers = buildChunkedProxyHeaders({
      contentType: head.ContentType ?? "application/json",
      cacheControl: buildChunkedCacheControl(deps.config),
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      contentEncoding: head.ContentEncoding,
    });
    if (typeof head.ContentLength === "number") headers["content-length"] = String(head.ContentLength);
    if (head.ETag) headers["etag"] = head.ETag;
    if (head.LastModified) headers["last-modified"] = head.LastModified.toUTCString();

    reply.status(200).headers(headers).send();
  });

  app.get("/v1/images/:imageId/chunked/chunks/:chunkIndex", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const params = req.params as { imageId: string; chunkIndex: string };
    const record = requireImage(deps.store, params.imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const chunkIndex = parseChunkIndexParam(params.chunkIndex);
    const key = getChunkObjectKey(record, chunkIndex);
    if (!key) {
      throw new ApiError(404, "Chunked image not available", "NOT_FOUND");
    }

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);
      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path: `/${key}`,
      });
      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);
      const url =
        deps.config.cloudfrontAuthMode === "url"
          ? createSignedUrl({
              url: stableUrl,
              keyPairId: deps.config.cloudfrontKeyPairId,
              privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
              expiresAt,
            })
          : stableUrl;
      reply.header("cache-control", "no-store").redirect(url, 307);
      return;
    }

    let s3Res: GetObjectCommandOutput;
    try {
      s3Res = await deps.s3.send(
        new GetObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: key,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number") {
        if (maybeStatus === 404) {
          throw new ApiError(404, "Chunk object not found", "NOT_FOUND");
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

    assertIdentityContentEncoding(s3Res.ContentEncoding);
    const headers = buildChunkedProxyHeaders({
      contentType: s3Res.ContentType ?? "application/octet-stream",
      cacheControl: buildChunkedCacheControl(deps.config),
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      contentEncoding: "identity",
    });
    if (typeof s3Res.ContentLength === "number") {
      headers["content-length"] = String(s3Res.ContentLength);
    }
    if (s3Res.ETag) headers["etag"] = s3Res.ETag;
    if (s3Res.LastModified) headers["last-modified"] = s3Res.LastModified.toUTCString();

    reply.status(200).headers(headers);
    return reply.send(s3Res.Body);
  });

  app.head("/v1/images/:imageId/chunked/chunks/:chunkIndex", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const params = req.params as { imageId: string; chunkIndex: string };
    const record = requireImage(deps.store, params.imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const chunkIndex = parseChunkIndexParam(params.chunkIndex);
    const key = getChunkObjectKey(record, chunkIndex);
    if (!key) {
      throw new ApiError(404, "Chunked image not available", "NOT_FOUND");
    }

    if (deps.config.cloudfrontDomain) {
      assertCloudFrontSigningConfiguredForConfig(deps.config);
      const stableUrl = buildCloudFrontUrl({
        cloudfrontDomain: deps.config.cloudfrontDomain,
        path: `/${key}`,
      });
      const expiresAt = new Date(Date.now() + deps.config.cloudfrontSignedTtlSeconds * 1000);
      const url =
        deps.config.cloudfrontAuthMode === "url"
          ? createSignedUrl({
              url: stableUrl,
              keyPairId: deps.config.cloudfrontKeyPairId,
              privateKeyPem: deps.config.cloudfrontPrivateKeyPem,
              expiresAt,
            })
          : stableUrl;
      reply.header("cache-control", "no-store").redirect(url, 307);
      return;
    }

    let head: HeadObjectCommandOutput;
    try {
      head = await deps.s3.send(
        new HeadObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: key,
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number" && maybeStatus === 404) {
        throw new ApiError(404, "Chunk object not found", "NOT_FOUND");
      }
      throw err;
    }

    assertIdentityContentEncoding(head.ContentEncoding);
    const headers = buildChunkedProxyHeaders({
      contentType: head.ContentType ?? "application/octet-stream",
      cacheControl: buildChunkedCacheControl(deps.config),
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      contentEncoding: "identity",
    });
    if (typeof head.ContentLength === "number") {
      headers["content-length"] = String(head.ContentLength);
    }
    if (head.ETag) headers["etag"] = head.ETag;
    if (head.LastModified) headers["last-modified"] = head.LastModified.toUTCString();

    reply.status(200).headers(headers).send();
  });

  app.get("/v1/images/:imageId/range", async (req, reply) => {
    const callerUserId = getCallerUserId(req, deps.config);
    const imageId = (req.params as { imageId: string }).imageId;
    const record = requireImage(deps.store, imageId);
    assertOwner(record, callerUserId);

    if (record.status !== "complete") {
      throw new ApiError(409, "Image is not complete", "INVALID_STATE");
    }

    const ifNoneMatch =
      typeof req.headers["if-none-match"] === "string" ? req.headers["if-none-match"] : undefined;
    const ifModifiedSince =
      typeof req.headers["if-modified-since"] === "string"
        ? req.headers["if-modified-since"]
        : undefined;

    const rawRange = typeof req.headers.range === "string" ? req.headers.range : undefined;
    const parsedRange = rawRange ? parseSingleByteRangeHeader(rawRange) : undefined;
    let requestedRange = parsedRange?.normalized;
    if (rawRange && !requestedRange) {
      await sendRangeNotSatisfiable({
        reply,
        config: deps.config,
        s3: deps.s3,
        store: deps.store,
        imageId,
        record,
      });
      return;
    }

    const ifRange = requestedRange && typeof req.headers["if-range"] === "string" ? req.headers["if-range"] : undefined;

    let ifMatch: string | undefined;
    if (ifRange) {
      const normalizedIfRange = normalizeEtag(ifRange);
      let currentEtag = record.etag;
      let currentLastModified = record.lastModified ? new Date(record.lastModified) : undefined;
      let currentCacheControl: string | undefined;

      // Fall back to an S3 HEAD if we don't have an in-memory validator.
      if (!currentEtag || (ifModifiedSince && !currentLastModified)) {
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

        currentEtag = typeof head.ETag === "string" ? head.ETag : undefined;
        const size = typeof head.ContentLength === "number" ? head.ContentLength : undefined;
        const lastModified =
          head.LastModified instanceof Date ? head.LastModified.toISOString() : undefined;
        currentLastModified = head.LastModified instanceof Date ? head.LastModified : undefined;
        currentCacheControl = head.CacheControl;

        deps.store.update(imageId, {
          etag: currentEtag,
          size,
          lastModified,
        });
      }

      if (!currentEtag) {
        throw new ApiError(502, "Unable to determine current image ETag", "S3_ERROR");
      }

      const normalizedCurrentEtag = normalizeEtag(currentEtag);
      if (normalizedIfRange !== normalizedCurrentEtag) {
        // RFC 9110 If-Range semantics: ignore Range and return the full representation to avoid
        // serving mixed-version bytes.
        requestedRange = undefined;
      } else {
        // Enforce If-Range semantics atomically at S3: if the object changes between our
        // validation step and GetObject, S3 will return 412 instead of streaming bytes for the
        // new version.
        ifMatch = normalizedCurrentEtag;
      }
    }

    // Conditional requests (RFC 9110): If-None-Match dominates If-Modified-Since.
    if (ifNoneMatch || ifModifiedSince) {
      let currentEtag = record.etag;
      let currentLastModified = record.lastModified ? new Date(record.lastModified) : undefined;
      let currentCacheControl: string | undefined;

      if (!currentEtag || (ifModifiedSince && !currentLastModified)) {
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

        currentEtag = typeof head.ETag === "string" ? head.ETag : undefined;
        currentLastModified = head.LastModified instanceof Date ? head.LastModified : undefined;
        currentCacheControl = head.CacheControl;

        deps.store.update(imageId, {
          etag: currentEtag,
          size: typeof head.ContentLength === "number" ? head.ContentLength : undefined,
          lastModified: head.LastModified instanceof Date ? head.LastModified.toISOString() : undefined,
        });
      }

      if (ifNoneMatch && currentEtag && ifNoneMatchMatches(ifNoneMatch, currentEtag)) {
        sendNotModified({
          reply,
          etag: currentEtag,
          lastModified: currentLastModified,
          cacheControl: currentCacheControl ?? deps.config.imageCacheControl,
          crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
        });
        return;
      }

      if (
        !ifNoneMatch &&
        ifModifiedSince &&
        currentLastModified &&
        ifModifiedSinceAllowsNotModified(ifModifiedSince, currentLastModified)
      ) {
        sendNotModified({
          reply,
          etag: currentEtag,
          lastModified: currentLastModified,
          cacheControl: currentCacheControl ?? deps.config.imageCacheControl,
          crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
        });
        return;
      }
    }

    if (
      parsedRange &&
      requestedRange &&
      typeof record.size === "number" &&
      parsedRange.start >= BigInt(record.size)
    ) {
      await sendRangeNotSatisfiable({
        reply,
        config: deps.config,
        s3: deps.s3,
        store: deps.store,
        imageId,
        record,
      });
      return;
    }

    let s3Res: GetObjectCommandOutput;
    try {
      s3Res = await deps.s3.send(
        new GetObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: record.s3Key,
          Range: requestedRange,
          ...(ifMatch ? { IfMatch: ifMatch } : {}),
        })
      );
    } catch (err) {
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number") {
        if (maybeStatus === 412) {
          // IfMatch failed (most likely due to an If-Range check). Per RFC 9110, ignore Range and
          // return the full representation to avoid mixed-version bytes.
          requestedRange = undefined;
          ifMatch = undefined;
          try {
            s3Res = await deps.s3.send(
              new GetObjectCommand({
                Bucket: deps.config.s3Bucket,
                Key: record.s3Key,
              })
            );
          } catch (retryErr) {
            const retryStatus = (
              retryErr as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
            ).$metadata?.httpStatusCode;
            if (typeof retryStatus === "number" && retryStatus === 404) {
              throw new ApiError(404, "Image object not found", "NOT_FOUND");
            }
            throw retryErr;
          }
        } else if (maybeStatus === 416) {
          await sendRangeNotSatisfiable({
            reply,
            config: deps.config,
            s3: deps.s3,
            store: deps.store,
            imageId,
            record,
          });
          return;
        } else if (maybeStatus === 404) {
          throw new ApiError(404, "Image object not found", "NOT_FOUND");
        } else if (maybeStatus >= 400 && maybeStatus < 500) {
          throw new ApiError(maybeStatus, "S3 request rejected", "S3_ERROR");
        }
      }
      throw err;
    }

    if (!s3Res.Body) {
      throw new ApiError(502, "S3 did not return a response body", "S3_ERROR");
    }

    if (requestedRange && !s3Res.ContentRange) {
      // If Range was requested, we must not accidentally stream the entire object.
      // Some backends may ignore malformed Range headers and return 200 without Content-Range.
      throw new ApiError(
        502,
        "S3 did not return Content-Range for a ranged request",
        "S3_ERROR"
      );
    }

    const proxy = buildRangeProxyResponse({
      s3: s3Res,
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
    });
    if (s3Res.CacheControl) {
      proxy.headers["cache-control"] = ensureNoTransformCacheControl(s3Res.CacheControl);
    }
    assertIdentityContentEncoding(s3Res.ContentEncoding);

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

    const ifNoneMatch =
      typeof req.headers["if-none-match"] === "string" ? req.headers["if-none-match"] : undefined;
    const ifModifiedSince =
      typeof req.headers["if-modified-since"] === "string"
        ? req.headers["if-modified-since"]
        : undefined;

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

    if (ifNoneMatch && head.ETag && ifNoneMatchMatches(ifNoneMatch, head.ETag)) {
      sendNotModified({
        reply,
        etag: head.ETag,
        lastModified: head.LastModified instanceof Date ? head.LastModified : undefined,
        cacheControl: head.CacheControl ?? deps.config.imageCacheControl,
        crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      });
      return;
    }
    if (
      !ifNoneMatch &&
      ifModifiedSince &&
      head.LastModified instanceof Date &&
      ifModifiedSinceAllowsNotModified(ifModifiedSince, head.LastModified)
    ) {
      sendNotModified({
        reply,
        etag: head.ETag,
        lastModified: head.LastModified,
        cacheControl: head.CacheControl ?? deps.config.imageCacheControl,
        crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      });
      return;
    }

    const proxy = buildRangeProxyResponse({
      s3: {
        ContentLength: head.ContentLength,
        ContentRange: undefined,
        ETag: head.ETag,
        LastModified: head.LastModified,
        ContentType: head.ContentType,
      },
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
    });
    if (head.CacheControl) {
      proxy.headers["cache-control"] = ensureNoTransformCacheControl(head.CacheControl);
    }
    assertIdentityContentEncoding(head.ContentEncoding);

    reply.status(proxy.statusCode).headers(proxy.headers).send();
  });

  return app;
}
