import { test, expect, type Page } from "@playwright/test";

import {
  buildTestImage,
  startDiskImageServer,
  startPageServer,
  type DiskImageServer,
  type PageServer,
} from "../../fixtures/servers";

const IMAGE_SIZE = 1024;
const IMAGE = buildTestImage(IMAGE_SIZE);

type RangeFetchOkResult = {
  ok: true;
  status: number;
  contentRange: string | null;
  acceptRanges: string | null;
  contentLength: string | null;
  contentType: string | null;
  bytes: number[];
};

type RangeFetchErrorResult = { ok: false; error: string };

type RangeFetchResult = RangeFetchOkResult | RangeFetchErrorResult;

type RangeFetchFn = (
  url: string,
  rangeHeaderValue: string,
  extraHeaders?: Record<string, string>,
) => Promise<RangeFetchResult>;

type HeadFetchOkResult = {
  ok: true;
  status: number;
  acceptRanges: string | null;
  contentLength: string | null;
};

type HeadFetchErrorResult = { ok: false; error: string };

type HeadFetchResult = HeadFetchOkResult | HeadFetchErrorResult;

function expectedBytes(start: number, endInclusive: number): number[] {
  const out: number[] = [];
  for (let i = start; i <= endInclusive; i += 1) out.push(i & 0xff);
  return out;
}

async function runRangeFetch(
  page: Page,
  opts: { pageOrigin: string; url: string; rangeHeaderValue: string; extraHeaders?: Record<string, string> },
): Promise<RangeFetchResult> {
  await page.goto(`${opts.pageOrigin}/`, { waitUntil: "load" });
  return await page.evaluate<
    RangeFetchResult,
    { targetUrl: string; range: string; extraHeaders?: Record<string, string> }
  >(
    async ({ targetUrl, range, extraHeaders }) => {
      const rangeFetch = (window as unknown as { __rangeFetch: RangeFetchFn }).__rangeFetch;
      return await rangeFetch(targetUrl, range, extraHeaders);
    },
    { targetUrl: opts.url, range: opts.rangeHeaderValue, extraHeaders: opts.extraHeaders },
  );
}

async function runHeadFetch(
  page: Page,
  opts: { pageOrigin: string; url: string; extraHeaders?: Record<string, string> },
): Promise<HeadFetchResult> {
  await page.goto(`${opts.pageOrigin}/`, { waitUntil: "load" });
  return await page.evaluate<HeadFetchResult, { targetUrl: string; extraHeaders?: Record<string, string> }>(
    async ({ targetUrl, extraHeaders }) => {
      const UTF8 = Object.freeze({ encoding: "utf-8" });
      const MAX_ERROR_BYTES = 512;

      function sanitizeOneLine(input: unknown): string {
        let out = "";
        let pendingSpace = false;
        for (const ch of String(input ?? "")) {
          const code = ch.codePointAt(0) ?? 0;
          const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
          if (forbidden || /\s/u.test(ch)) {
            pendingSpace = out.length > 0;
            continue;
          }
          if (pendingSpace) {
            out += " ";
            pendingSpace = false;
          }
          out += ch;
        }
        return out.trim();
      }

      function truncateUtf8(input: unknown, maxBytes: number): string {
        if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
        const s = String(input ?? "");
        const enc = new TextEncoder();
        const bytes = enc.encode(s);
        if (bytes.byteLength <= maxBytes) return s;
        let cut = maxBytes;
        while (cut > 0 && (bytes[cut] & 0xc0) === 0x80) cut -= 1;
        if (cut <= 0) return "";
        const dec = new TextDecoder(UTF8.encoding);
        return dec.decode(bytes.subarray(0, cut));
      }

      function formatOneLineUtf8(input: unknown, maxBytes: number): string {
        return truncateUtf8(sanitizeOneLine(input), maxBytes);
      }

      try {
        const response = await fetch(targetUrl, {
          method: "HEAD",
          mode: "cors",
          headers: extraHeaders ?? {},
        });
        return {
          ok: true,
          status: response.status,
          acceptRanges: response.headers.get("Accept-Ranges"),
          contentLength: response.headers.get("Content-Length"),
        };
      } catch (err) {
        return { ok: false, error: formatOneLineUtf8(err, MAX_ERROR_BYTES) || "Error" };
      }
    },
    { targetUrl: opts.url, extraHeaders: opts.extraHeaders },
  );
}

function splitHeaderList(value: string | null): string[] {
  if (!value) return [];
  return value
    .split(",")
    .map((entry) => entry.trim().toLowerCase())
    .filter(Boolean);
}

function headerValue(value: string | string[] | undefined): string | undefined {
  if (value === undefined) return undefined;
  return Array.isArray(value) ? value.join(",") : value;
}

test.describe("disk-image Range fetch (same origin)", () => {
  let server!: DiskImageServer;

  test.beforeAll(async () => {
    server = await startDiskImageServer({ data: IMAGE, enableCors: false, serveTestPage: true });
  });

  test.afterAll(async () => {
    await server.close();
  });

  test("returns 206 + Content-Range and exact bytes", async ({ page }) => {
    const start = 10;
    const endInclusive = 31;

    const result = await runRangeFetch(page, {
      pageOrigin: server.origin,
      url: server.url("/disk.img"),
      rangeHeaderValue: `bytes=${start}-${endInclusive}`,
    });

    if (!result.ok) throw new Error(`Range fetch failed: ${result.error}`);

    expect(result.status, "Range request must return 206 Partial Content").toBe(206);
    expect(result.contentRange, "Server must send Content-Range").toBe(
      `bytes ${start}-${endInclusive}/${IMAGE_SIZE}`,
    );
    expect(result.bytes, "Returned body must match the requested Range").toEqual(
      expectedBytes(start, endInclusive),
    );

    const rangeReq = server.requests.find(
      (req) => req.method === "GET" && req.url.startsWith("/disk.img"),
    );
    expect(headerValue(rangeReq?.headers.range), "Browser must send Range request header").toBe(
      `bytes=${start}-${endInclusive}`,
    );
  });
});

test.describe("disk-image Range fetch (cross origin CORS + preflight)", () => {
  let diskServer!: DiskImageServer;
  let pageServer!: PageServer;

  test.beforeAll(async () => {
    diskServer = await startDiskImageServer({ data: IMAGE, enableCors: true });
    pageServer = await startPageServer();
  });

  test.afterAll(async () => {
    await Promise.all([diskServer.close(), pageServer.close()]);
  });

  test.beforeEach(() => {
    diskServer.resetRequests();
  });

  test("preflight OPTIONS advertises Range and exposes Content-Range", async () => {
    const resp = await fetch(diskServer.url("/disk.img"), {
      method: "OPTIONS",
      headers: {
        Origin: pageServer.origin,
        "Access-Control-Request-Method": "GET",
        // Include a non-safelisted custom header so this probe matches the browser
        // request we use in the actual cross-origin Range fetch test.
        "Access-Control-Request-Headers": "range, x-force-preflight",
      },
    });

    expect(resp.status, "CORS preflight should succeed").toBeGreaterThanOrEqual(200);
    expect(resp.status, "CORS preflight should succeed").toBeLessThan(300);

    const allowOrigin = resp.headers.get("access-control-allow-origin");
    expect(
      allowOrigin === "*" || allowOrigin === pageServer.origin,
      `Access-Control-Allow-Origin must allow the page origin (got ${allowOrigin})`,
    ).toBe(true);

    const allowHeaders = splitHeaderList(resp.headers.get("access-control-allow-headers"));
    expect(allowHeaders, "Access-Control-Allow-Headers must include Range").toContain("range");
    expect(allowHeaders, "Access-Control-Allow-Headers must include X-Force-Preflight").toContain(
      "x-force-preflight",
    );

    const allowMethods = splitHeaderList(resp.headers.get("access-control-allow-methods"));
    expect(allowMethods, "Access-Control-Allow-Methods must include GET").toContain("get");
    expect(allowMethods, "Access-Control-Allow-Methods must include HEAD").toContain("head");
    expect(allowMethods, "Access-Control-Allow-Methods must include OPTIONS").toContain("options");

    const exposeHeaders = splitHeaderList(resp.headers.get("access-control-expose-headers"));
    expect(exposeHeaders, "Access-Control-Expose-Headers must include Content-Range").toContain(
      "content-range",
    );
    expect(exposeHeaders, "Access-Control-Expose-Headers must include Accept-Ranges").toContain(
      "accept-ranges",
    );
    expect(exposeHeaders, "Access-Control-Expose-Headers must include Content-Length").toContain(
      "content-length",
    );
  });

  test("cross-origin Range fetch succeeds and Content-Range is readable", async ({ page }) => {
    const start = 100;
    const endInclusive = 155;

    const result = await runRangeFetch(page, {
      pageOrigin: pageServer.origin,
      url: diskServer.url("/disk.img"),
      rangeHeaderValue: `bytes=${start}-${endInclusive}`,
      // Force a CORS preflight in every browser so we can assert OPTIONS behavior even
      // when a browser treats `Range` as CORS-safelisted.
      extraHeaders: { "X-Force-Preflight": "1" },
    });

    if (!result.ok) throw new Error(`Cross-origin Range fetch failed (likely CORS): ${result.error}`);

    expect(result.status, "Range request must return 206 Partial Content").toBe(206);
    expect(
      result.contentRange,
      "Content-Range must be present and exposed via Access-Control-Expose-Headers",
    ).toBe(`bytes ${start}-${endInclusive}/${IMAGE_SIZE}`);
    expect(result.acceptRanges, "Accept-Ranges must be exposed").toBe("bytes");
    expect(result.contentLength, "Content-Length must be exposed").toBe(String(endInclusive - start + 1));
    expect(result.bytes, "Returned body must match the requested Range").toEqual(
      expectedBytes(start, endInclusive),
    );

    const preflight = diskServer.requests.find((req) => req.method === "OPTIONS");
    expect(preflight, "Browser should send a CORS preflight OPTIONS request").toBeTruthy();
    expect(headerValue(preflight?.headers["access-control-request-headers"]) ?? "").toContain(
      "x-force-preflight",
    );

    const rangeReq = diskServer.requests.find((req) => req.method === "GET");
    expect(headerValue(rangeReq?.headers.range), "Browser must send Range request header").toBe(
      `bytes=${start}-${endInclusive}`,
    );
    expect(headerValue(rangeReq?.headers["x-force-preflight"]), "Browser must include extra header").toBe(
      "1",
    );
  });

  test("cross-origin HEAD exposes Content-Length and Accept-Ranges", async ({ page }) => {
    const result = await runHeadFetch(page, { pageOrigin: pageServer.origin, url: diskServer.url("/disk.img") });
    if (!result.ok) throw new Error(`Cross-origin HEAD failed (likely CORS): ${result.error}`);

    expect(result.status).toBe(200);
    expect(result.acceptRanges, "Accept-Ranges must be exposed").toBe("bytes");
    expect(result.contentLength, "Content-Length must be exposed").toBe(String(IMAGE_SIZE));

    const headReq = diskServer.requests.find((req) => req.method === "HEAD");
    expect(headReq, "Browser should issue a HEAD request").toBeTruthy();
    expect(headerValue(headReq?.headers.origin), "Browser must send Origin header").toBe(pageServer.origin);
  });
});

test.describe("disk-image Range fetch (cross origin + COOP/COEP)", () => {
  test.skip(!process.env.PLAYWRIGHT_COOP_COEP, "Set PLAYWRIGHT_COOP_COEP=1 to enable");

  let diskServer!: DiskImageServer;
  let pageServer!: PageServer;

  test.beforeAll(async () => {
    diskServer = await startDiskImageServer({ data: IMAGE, enableCors: true });
    pageServer = await startPageServer({ coopCoep: true });
  });

  test.afterAll(async () => {
    await Promise.all([diskServer.close(), pageServer.close()]);
  });

  test("COOP/COEP page can still perform cross-origin Range fetch with CORS", async ({ page }) => {
    const start = 2;
    const endInclusive = 9;

    await page.goto(`${pageServer.origin}/`, { waitUntil: "load" });
    expect(await page.evaluate(() => self.crossOriginIsolated), "Page should be crossOriginIsolated").toBe(
      true,
    );

    const result = await page.evaluate<RangeFetchResult, { targetUrl: string; range: string }>(
      async ({ targetUrl, range }) => {
        const rangeFetch = (window as unknown as { __rangeFetch: RangeFetchFn }).__rangeFetch;
        return await rangeFetch(targetUrl, range, { "X-Force-Preflight": "1" });
      },
      { targetUrl: diskServer.url("/disk.img"), range: `bytes=${start}-${endInclusive}` },
    );

    if (!result.ok) {
      throw new Error(`COOP/COEP cross-origin Range fetch failed (likely CORS/COEP): ${result.error}`);
    }

    expect(result.status).toBe(206);
    expect(result.contentRange).toBe(`bytes ${start}-${endInclusive}/${IMAGE_SIZE}`);
    expect(result.bytes).toEqual(expectedBytes(start, endInclusive));
  });
});
