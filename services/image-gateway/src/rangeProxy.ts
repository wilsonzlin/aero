import type { GetObjectCommandOutput } from "@aws-sdk/client-s3";

import type { CrossOriginResourcePolicy } from "./config";

export interface RangeProxyResponse {
  statusCode: number;
  headers: Record<string, string>;
}

export const DISK_BYTES_CONTENT_TYPE = "application/octet-stream";
const MAX_CONTENT_RANGE_LEN = 256;

function computeContentLengthFromContentRange(contentRange: string): string | undefined {
  if (contentRange.length > MAX_CONTENT_RANGE_LEN) return undefined;
  const match = /^bytes (\d+)-(\d+)\/(\d+|\*)$/i.exec(contentRange.trim());
  if (!match) return undefined;
  const start = BigInt(match[1]);
  const end = BigInt(match[2]);
  if (end < start) return undefined;
  return (end - start + 1n).toString();
}

export function buildRangeProxyHeaders(params: {
  contentType: string | undefined;
  crossOriginResourcePolicy: CrossOriginResourcePolicy;
}): Record<string, string> {
  const headers: Record<string, string> = {
    "cache-control": "no-transform",
    "accept-ranges": "bytes",
    "content-encoding": "identity",
    "content-type": params.contentType ?? DISK_BYTES_CONTENT_TYPE,
    "x-content-type-options": "nosniff",
    "cross-origin-resource-policy": params.crossOriginResourcePolicy,
  };
  return headers;
}

export function buildRangeProxyResponse(params: {
  s3: Pick<
    GetObjectCommandOutput,
    "ContentLength" | "ContentRange" | "ETag" | "LastModified" | "ContentType"
  >;
  crossOriginResourcePolicy: CrossOriginResourcePolicy;
}): RangeProxyResponse {
  const headers = buildRangeProxyHeaders({
    contentType: params.s3.ContentType,
    crossOriginResourcePolicy: params.crossOriginResourcePolicy,
  });

  if (params.s3.ETag) {
    headers["etag"] = params.s3.ETag;
  }
  if (params.s3.LastModified) {
    headers["last-modified"] = params.s3.LastModified.toUTCString();
  }

  const isPartial = Boolean(params.s3.ContentRange);
  if (params.s3.ContentRange) {
    headers["content-range"] = params.s3.ContentRange;
  }

  if (typeof params.s3.ContentLength === "number") {
    headers["content-length"] = String(params.s3.ContentLength);
  } else if (params.s3.ContentRange) {
    const inferred = computeContentLengthFromContentRange(params.s3.ContentRange);
    if (inferred) headers["content-length"] = inferred;
  }
  return { statusCode: isPartial ? 206 : 200, headers };
}
