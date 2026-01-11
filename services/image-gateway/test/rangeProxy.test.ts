import { describe, expect, it } from "vitest";

import { buildRangeProxyResponse } from "../src/rangeProxy";

describe("range proxy", () => {
  it("returns 206 headers for a ranged response", () => {
    const lastModified = new Date("2020-01-01T00:00:00.000Z");
    const res = buildRangeProxyResponse({
      s3: {
        ContentRange: "bytes 0-9/100",
        ContentLength: 10,
        ETag: '"etag"',
        LastModified: lastModified,
        ContentType: "application/octet-stream",
      },
      crossOriginResourcePolicy: "same-site",
    });

    expect(res.statusCode).toBe(206);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["content-range"]).toBe("bytes 0-9/100");
    expect(res.headers["content-length"]).toBe("10");
    expect(res.headers["etag"]).toBe('"etag"');
    expect(res.headers["last-modified"]).toBe(lastModified.toUTCString());
  });

  it("returns 200 headers when no Range is requested", () => {
    const res = buildRangeProxyResponse({
      s3: {
        ContentLength: 100,
        ETag: '"etag"',
        ContentType: "application/octet-stream",
      },
      crossOriginResourcePolicy: "same-site",
    });

    expect(res.statusCode).toBe(200);
    expect(res.headers["accept-ranges"]).toBe("bytes");
    expect(res.headers["cache-control"]).toBe("no-transform");
    expect(res.headers["content-encoding"]).toBe("identity");
    expect(res.headers["content-type"]).toBe("application/octet-stream");
    expect(res.headers["x-content-type-options"]).toBe("nosniff");
    expect(res.headers["cross-origin-resource-policy"]).toBe("same-site");
    expect(res.headers["content-length"]).toBe("100");
    expect(res.headers).not.toHaveProperty("content-range");
  });

  it("emits 206 when Content-Range exists even if Content-Length is missing", () => {
    const res = buildRangeProxyResponse({
      s3: {
        ContentRange: "bytes 0-99/100",
      },
      crossOriginResourcePolicy: "same-site",
    });

    expect(res.statusCode).toBe(206);
    expect(res.headers["content-range"]).toBe("bytes 0-99/100");
    expect(res.headers).not.toHaveProperty("content-length");
  });

  it("falls back to application/octet-stream when the upstream does not provide a Content-Type", () => {
    const res = buildRangeProxyResponse({
      s3: {
        ContentLength: 100,
      },
      crossOriginResourcePolicy: "same-site",
    });

    expect(res.headers["content-type"]).toBe("application/octet-stream");
  });
});
