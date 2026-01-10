import type { GetObjectCommandOutput } from "@aws-sdk/client-s3";

export interface RangeProxyResponse {
  statusCode: number;
  headers: Record<string, string>;
}

export function buildRangeProxyResponse(params: {
  s3: Pick<
    GetObjectCommandOutput,
    "ContentLength" | "ContentRange" | "ETag" | "LastModified" | "ContentType"
  >;
}): RangeProxyResponse {
  const headers: Record<string, string> = {
    "accept-ranges": "bytes",
  };

  if (params.s3.ContentType) {
    headers["content-type"] = params.s3.ContentType;
  }
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
  }
  return { statusCode: isPartial ? 206 : 200, headers };
}
