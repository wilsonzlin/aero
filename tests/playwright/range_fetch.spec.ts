import { test, expect, type Page } from "@playwright/test";

import {
  buildTestImage,
  startDiskImageServer,
  startPageServer,
  type DiskImageServer,
  type PageServer,
} from "../fixtures/servers";

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

type RangeFetchFn = (url: string, rangeHeaderValue: string) => Promise<RangeFetchResult>;

function expectedBytes(start: number, endInclusive: number): number[] {
  const out: number[] = [];
  for (let i = start; i <= endInclusive; i += 1) out.push(i & 0xff);
  return out;
}

async function runRangeFetch(
  page: Page,
  opts: { pageOrigin: string; url: string; rangeHeaderValue: string },
): Promise<RangeFetchResult> {
  await page.goto(`${opts.pageOrigin}/`, { waitUntil: "load" });
  return await page.evaluate<RangeFetchResult, { targetUrl: string; range: string }>(
    async ({ targetUrl, range }) => {
      const rangeFetch = (window as unknown as { __rangeFetch: RangeFetchFn }).__rangeFetch;
      return await rangeFetch(targetUrl, range);
    },
    { targetUrl: opts.url, range: opts.rangeHeaderValue },
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

  test("preflight OPTIONS advertises Range and exposes Content-Range", async () => {
    const resp = await fetch(diskServer.url("/disk.img"), {
      method: "OPTIONS",
      headers: {
        Origin: pageServer.origin,
        "Access-Control-Request-Method": "GET",
        "Access-Control-Request-Headers": "range",
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

    diskServer.resetRequests();
  });

  test("cross-origin Range fetch succeeds and Content-Range is readable", async ({ page }) => {
    const start = 100;
    const endInclusive = 155;

    const result = await runRangeFetch(page, {
      pageOrigin: pageServer.origin,
      url: diskServer.url("/disk.img"),
      rangeHeaderValue: `bytes=${start}-${endInclusive}`,
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
    // Some engines preflight Range requests; others may treat `Range` as CORS-safelisted.
    if (preflight) {
      expect(headerValue(preflight.headers["access-control-request-headers"]) ?? "").toContain("range");
    }

    const rangeReq = diskServer.requests.find((req) => req.method === "GET");
    expect(headerValue(rangeReq?.headers.range), "Browser must send Range request header").toBe(
      `bytes=${start}-${endInclusive}`,
    );
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
        return await rangeFetch(targetUrl, range);
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
