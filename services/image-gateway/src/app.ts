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
import { formatOneLineUtf8 } from "./text";

export interface BuildAppDeps {
  config: Config;
  s3: S3Client;
  store: ImageStore;
}

const S3_MULTIPART_MAX_PARTS = 10_000;
const CHUNK_INDEX_WIDTH = 8;
const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_CORS_REQUEST_HEADERS_LEN = 4 * 1024;
const MAX_RANGE_HEADER_LEN = 16 * 1024;
const MAX_IF_NONE_MATCH_LEN = 16 * 1024;
const MAX_IF_MODIFIED_SINCE_LEN = 128;
const MAX_IF_RANGE_LEN = 256;
const MAX_FORWARD_VALUE_LEN = 4 * 1024;
const MAX_PROTO_VALUE_LEN = 64;
const MAX_UPLOAD_ID_LEN = 1024;
const MAX_ETAG_LEN = 4 * 1024;

function firstCommaSeparatedValue(raw: string): string {
  const idx = raw.indexOf(",");
  return (idx === -1 ? raw : raw.slice(0, idx)).trim();
}

function headerValueString(raw: unknown, maxLen: number): string | undefined {
  if (typeof raw === "string") return raw.length <= maxLen ? raw : undefined;
  if (Array.isArray(raw) && raw.length === 1 && typeof raw[0] === "string") {
    return raw[0].length <= maxLen ? raw[0] : undefined;
  }
  return undefined;
}

function isSafeHeaderListValue(value: string): boolean {
  // Conservative: allow RFC7230 token characters, commas, and whitespace.
  //
  // token = 1*tchar
  // tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
  //         "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
  for (let i = 0; i < value.length; i++) {
    const c = value.charCodeAt(i);
    // whitespace: HTAB or SP
    if (c === 0x09 || c === 0x20 || c === 0x2c) continue; // \t, space, comma
    if (c >= 0x30 && c <= 0x39) continue; // 0-9
    if (c >= 0x41 && c <= 0x5a) continue; // A-Z
    if (c >= 0x61 && c <= 0x7a) continue; // a-z
    if (
      c === 0x21 || // !
      c === 0x23 || // #
      c === 0x24 || // $
      c === 0x25 || // %
      c === 0x26 || // &
      c === 0x27 || // '
      c === 0x2a || // *
      c === 0x2b || // +
      c === 0x2d || // -
      c === 0x2e || // .
      c === 0x5e || // ^
      c === 0x5f || // _
      c === 0x60 || // `
      c === 0x7c || // |
      c === 0x7e // ~
    ) {
      continue;
    }
    return false;
  }
  return true;
}

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
  if (/^w\//i.test(trimmed)) {
    const opaque = trimmed.slice(2).trim();
    return `W/${normalizeOpaqueTag(opaque)}`;
  }
  return normalizeOpaqueTag(trimmed);
}

function normalizeOpaqueTag(value: string): string {
  const trimmed = value.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) return trimmed;
  return `"${trimmed.replace(/"/g, "")}"`;
}

function ensureNoTransformCacheControl(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) return "no-transform";
  // Defensive: avoid unbounded parsing on unexpected huge metadata values.
  if (trimmed.length > 4 * 1024) return "no-transform";
  const directives = trimmed.split(",").map((directive) => directive.trim().toLowerCase());
  if (directives.includes("no-transform")) return trimmed;
  return `${trimmed}, no-transform`;
}

function stripWeakEtagPrefix(value: string): string {
  const trimmed = value.trim();
  return trimmed.replace(/^w\//i, "");
}

function isWeakEtag(value: string): boolean {
  return value.trim().toLowerCase().startsWith("w/");
}

function ifNoneMatchMatches(ifNoneMatch: string, currentEtag: string): boolean {
  const raw = ifNoneMatch.trim();
  if (!raw) return false;
  if (raw === "*") return true;

  const current = stripWeakEtagPrefix(normalizeEtag(currentEtag));
  let start = 0;
  let inQuotes = false;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (ch === '"') {
      inQuotes = !inQuotes;
      continue;
    }
    if (ch === "," && !inQuotes) {
      const tag = raw.slice(start, i).trim();
      if (tag === "*") return true;
      if (tag && stripWeakEtagPrefix(normalizeEtag(tag)) === current) return true;
      start = i + 1;
    }
  }
  const tag = raw.slice(start).trim();
  if (tag === "*") return true;
  if (tag && stripWeakEtagPrefix(normalizeEtag(tag)) === current) return true;
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

function parseHttpDate(value: string): Date | undefined {
  const millis = Date.parse(value);
  if (!Number.isFinite(millis)) return undefined;
  return new Date(millis);
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

function parseChunkObjectNameParam(raw: string): string {
  const match = raw.match(/^(\d+)(?:\.bin)?$/);
  if (!match) {
    throw new ApiError(400, "chunkIndex must be a non-negative integer", "BAD_REQUEST");
  }
  const digits = match[1]!;
  // Defensive bound: avoid building pathological S3 keys from attacker-controlled huge path
  // segments. Keep aligned with the chunked format spec (`chunkIndexWidth <= 32`).
  if (digits.length > 32) {
    throw new ApiError(400, "chunkIndex is too long", "BAD_REQUEST");
  }

  // If the caller used the `.bin` form, preserve the digit width as-is to support variable
  // `chunkIndexWidth` manifests (e.g. `00.bin`..`99.bin`).
  if (raw.endsWith(".bin")) return digits;

  // For the numeric form (`/chunks/42`), continue to use the gateway's canonical width.
  const chunkIndex = Number.parseInt(digits, 10);
  if (!Number.isFinite(chunkIndex) || !Number.isInteger(chunkIndex) || chunkIndex < 0) {
    throw new ApiError(400, "chunkIndex must be a non-negative integer", "BAD_REQUEST");
  }
  return formatChunkObjectName(chunkIndex);
}

function getChunkObjectKey(record: ImageRecord, chunkObjectName: string): string | undefined {
  const prefix = getChunkedBasePrefix(record);
  if (prefix === undefined) return undefined;
  return `${prefix}chunks/${chunkObjectName}.bin`;
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
      "accept-ranges,content-range,content-length,content-encoding,etag,last-modified"
    );

  if (allowOrigin !== "*") {
    const existing = headerValueString(reply.getHeader("vary"), MAX_FORWARD_VALUE_LEN);
    const tokens = new Set<string>();
    if (existing) {
      for (const raw of existing.split(",")) {
        const t = raw.trim();
        if (!t) continue;
        if (t === "*") {
          reply.header("vary", "*");
          reply.header("access-control-allow-credentials", "true");
          return;
        }
        tokens.add(t.toLowerCase());
      }
    }
    if (!tokens.has("origin")) {
      const next = existing ? `${existing}, Origin` : "Origin";
      reply.header("vary", next);
    }
    reply.header("access-control-allow-credentials", "true");
  }
}

function applyCorsPreflight(req: FastifyRequest, reply: FastifyReply): void {
  const requestedHeaders = headerValueString(
    req.headers["access-control-request-headers"],
    MAX_CORS_REQUEST_HEADERS_LEN
  );
  reply
    .header("access-control-allow-methods", "GET,HEAD,POST,OPTIONS")
    .header(
      "access-control-allow-headers",
      requestedHeaders && isSafeHeaderListValue(requestedHeaders)
        ? requestedHeaders
        : "range,if-range,content-type,x-user-id"
    )
    .header("access-control-max-age", "86400");

  const existing = headerValueString(reply.getHeader("vary"), MAX_FORWARD_VALUE_LEN);
  if (existing && existing.trim() === "*") return;
  const next = [
    ...(existing ? [existing] : []),
    "Origin",
    "Access-Control-Request-Method",
    "Access-Control-Request-Headers",
  ].join(", ");
  reply.header("vary", next);
}

function buildSelfUrl(req: FastifyRequest, path: string): string {
  const rawHost = headerValueString(
    req.headers["x-forwarded-host"] ?? req.headers.host,
    MAX_FORWARD_VALUE_LEN
  );
  const host = rawHost ? firstCommaSeparatedValue(rawHost) : undefined;

  const rawProto = headerValueString(req.headers["x-forwarded-proto"], MAX_PROTO_VALUE_LEN);
  const protoCandidate = rawProto ? firstCommaSeparatedValue(rawProto).toLowerCase() : undefined;
  const proto = protoCandidate === "http" || protoCandidate === "https" ? protoCandidate : req.protocol;

  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  if (!host) return normalizedPath;
  // Defensive: avoid emitting obviously malformed absolute URLs when forwarded headers are junk.
  for (let i = 0; i < host.length; i++) {
    const c = host.charCodeAt(i);
    if (c <= 0x20 || c === 0x2f || c === 0x5c) {
      return normalizedPath;
    }
  }
  return `${proto}://${host}${normalizedPath}`;
}

export function buildApp(deps: BuildAppDeps): FastifyInstance {
  const app = fastify({
    logger: process.env.NODE_ENV !== "test",
  });

  const MAX_CLIENT_ERROR_MESSAGE_BYTES = 512;

  function formatClientErrorMessage(message: unknown): string {
    const formatted = formatOneLineUtf8(message, MAX_CLIENT_ERROR_MESSAGE_BYTES);
    return formatted || "Request failed";
  }

  app.addHook("onRequest", async (req, reply) => {
    applyCorsHeaders(reply, deps.config);

    const rawUrl = req.raw.url;
    if (typeof rawUrl !== "string") {
      return reply.status(400).send({ error: { code: "BAD_REQUEST", message: "Bad Request" } });
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      return reply.status(414).send({ error: { code: "URL_TOO_LONG", message: "URL too long" } });
    }

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
        : err instanceof ApiError
          ? formatClientErrorMessage(err.message)
          : "Request failed";

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
    if (uploadId.length > MAX_UPLOAD_ID_LEN) {
      throw new ApiError(400, "uploadId is too long", "BAD_REQUEST");
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
    if (uploadId.length > MAX_UPLOAD_ID_LEN) {
      throw new ApiError(400, "uploadId is too long", "BAD_REQUEST");
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
        if (etag.length > MAX_ETAG_LEN) {
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

    // Manifest responses should be treated like other disk streaming endpoints: for compatibility
    // with native clients and tooling, require identity (or absent) encoding and always respond
    // with `Content-Encoding: identity`.
    assertIdentityContentEncoding(s3Res.ContentEncoding);
    const headers = buildChunkedProxyHeaders({
      contentType: s3Res.ContentType ?? "application/json",
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

    assertIdentityContentEncoding(head.ContentEncoding);
    const headers = buildChunkedProxyHeaders({
      contentType: head.ContentType ?? "application/json",
      cacheControl: buildChunkedCacheControl(deps.config),
      crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      contentEncoding: "identity",
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

    const chunkObjectName = parseChunkObjectNameParam(params.chunkIndex);
    const key = getChunkObjectKey(record, chunkObjectName);
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

    const chunkObjectName = parseChunkObjectNameParam(params.chunkIndex);
    const key = getChunkObjectKey(record, chunkObjectName);
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

    const ifNoneMatchRaw =
      typeof req.headers["if-none-match"] === "string" ? req.headers["if-none-match"] : undefined;
    const ifNoneMatch =
      ifNoneMatchRaw && ifNoneMatchRaw.length <= MAX_IF_NONE_MATCH_LEN ? ifNoneMatchRaw : undefined;
    const ifModifiedSince =
      typeof req.headers["if-modified-since"] === "string"
        ? req.headers["if-modified-since"].slice(0, MAX_IF_MODIFIED_SINCE_LEN + 1)
        : undefined;
    const ifModifiedSinceSafe =
      ifModifiedSince && ifModifiedSince.length <= MAX_IF_MODIFIED_SINCE_LEN
        ? ifModifiedSince
        : undefined;

    const rawRange = typeof req.headers.range === "string" ? req.headers.range : undefined;
    if (rawRange && rawRange.length > MAX_RANGE_HEADER_LEN) {
      const headers = buildRangeProxyHeaders({
        contentType: DISK_BYTES_CONTENT_TYPE,
        crossOriginResourcePolicy: deps.config.crossOriginResourcePolicy,
      });
      reply.status(413).headers(headers).send();
      return;
    }
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

    const ifRangeRaw =
      requestedRange && typeof req.headers["if-range"] === "string" ? req.headers["if-range"] : undefined;
    const ifRange =
      ifRangeRaw && ifRangeRaw.length <= MAX_IF_RANGE_LEN ? ifRangeRaw : undefined;
    if (ifRangeRaw && !ifRange) {
      requestedRange = undefined;
    }

    let ifMatch: string | undefined;
    let ifUnmodifiedSince: Date | undefined;
    if (ifRange) {
      const ifRangeTrimmed = ifRange.trim();
      const ifRangeLooksLikeEtag =
        ifRangeTrimmed.startsWith('"') || /^w\//i.test(ifRangeTrimmed);
      const ifRangeDate = ifRangeLooksLikeEtag ? undefined : parseHttpDate(ifRangeTrimmed);

      let currentEtag = record.etag;
      let currentLastModified = record.lastModified ? new Date(record.lastModified) : undefined;

      // Fall back to an S3 HEAD if we don't have the validator required by If-Range.
      const needEtag = ifRangeLooksLikeEtag;
      const needLastModified = !ifRangeLooksLikeEtag;
      if ((needEtag && !currentEtag) || (needLastModified && !currentLastModified)) {
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
        deps.store.update(imageId, {
          etag: currentEtag,
          size: typeof head.ContentLength === "number" ? head.ContentLength : undefined,
          lastModified:
            head.LastModified instanceof Date ? head.LastModified.toISOString() : undefined,
        });
      }

      if (ifRangeLooksLikeEtag) {
        if (!currentEtag) {
          throw new ApiError(502, "Unable to determine current image ETag", "S3_ERROR");
        }

        const normalizedIfRange = normalizeEtag(ifRangeTrimmed);
        const normalizedCurrentEtag = normalizeEtag(currentEtag);

        // RFC 9110 If-Range semantics: strong comparison, and weak validators must not match.
        if (
          isWeakEtag(normalizedIfRange) ||
          isWeakEtag(normalizedCurrentEtag) ||
          normalizedIfRange !== normalizedCurrentEtag
        ) {
          requestedRange = undefined;
        } else {
          // Enforce If-Range semantics atomically at S3: if the object changes between our
          // validation step and GetObject, S3 will return 412 instead of streaming bytes for the
          // new version.
          ifMatch = normalizedCurrentEtag;
        }
      } else {
        // If-Range can also be an HTTP-date. Serve the range only if the resource has not been
        // modified since the supplied time. Compare at second granularity (HTTP-date resolution).
        if (!ifRangeDate || !currentLastModified) {
          requestedRange = undefined;
        } else {
          const resourceSeconds = Math.floor(currentLastModified.getTime() / 1000);
          const sinceSeconds = Math.floor(ifRangeDate.getTime() / 1000);
          if (resourceSeconds > sinceSeconds) {
            requestedRange = undefined;
          } else {
            // Enforce If-Range atomically at S3 with an `If-Unmodified-Since` condition.
            ifUnmodifiedSince = ifRangeDate;
          }
        }
      }
    }

    // Conditional requests (RFC 9110): If-None-Match dominates If-Modified-Since.
    if (ifNoneMatch || ifModifiedSinceSafe) {
      let currentEtag = record.etag;
      let currentLastModified = record.lastModified ? new Date(record.lastModified) : undefined;
      let currentCacheControl: string | undefined;

      if (!currentEtag || (ifModifiedSinceSafe && !currentLastModified)) {
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
        ifModifiedSinceSafe &&
        currentLastModified &&
        ifModifiedSinceAllowsNotModified(ifModifiedSinceSafe, currentLastModified)
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

    let s3Res: GetObjectCommandOutput | undefined;
    try {
      s3Res = await deps.s3.send(
        new GetObjectCommand({
          Bucket: deps.config.s3Bucket,
          Key: record.s3Key,
          Range: requestedRange,
          ...(ifMatch ? { IfMatch: ifMatch } : {}),
          ...(ifUnmodifiedSince ? { IfUnmodifiedSince: ifUnmodifiedSince } : {}),
        })
      );
    } catch (err) {
      let handled = false;
      const maybeStatus = (
        err as Partial<{ $metadata?: { httpStatusCode?: unknown } }>
      ).$metadata?.httpStatusCode;
      if (typeof maybeStatus === "number") {
        if (maybeStatus === 412) {
          // IfMatch/IfUnmodifiedSince failed (most likely due to an If-Range check). Per RFC 9110,
          // ignore Range and return the full representation to avoid mixed-version bytes.
          requestedRange = undefined;
          ifMatch = undefined;
          ifUnmodifiedSince = undefined;
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
          handled = true;
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
      if (!handled) throw err;
    }

    if (!s3Res) {
      throw new ApiError(502, "S3 did not return a response", "S3_ERROR");
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

    const ifNoneMatchRaw =
      typeof req.headers["if-none-match"] === "string" ? req.headers["if-none-match"] : undefined;
    const ifNoneMatch =
      ifNoneMatchRaw && ifNoneMatchRaw.length <= MAX_IF_NONE_MATCH_LEN ? ifNoneMatchRaw : undefined;
    const ifModifiedSince =
      typeof req.headers["if-modified-since"] === "string"
        ? req.headers["if-modified-since"].slice(0, MAX_IF_MODIFIED_SINCE_LEN + 1)
        : undefined;
    const ifModifiedSinceSafe =
      ifModifiedSince && ifModifiedSince.length <= MAX_IF_MODIFIED_SINCE_LEN
        ? ifModifiedSince
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
      ifModifiedSinceSafe &&
      head.LastModified instanceof Date &&
      ifModifiedSinceAllowsNotModified(ifModifiedSinceSafe, head.LastModified)
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
