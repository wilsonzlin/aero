import http from "node:http";
import { once } from "node:events";
import type { IncomingHttpHeaders, IncomingMessage, Server, ServerResponse } from "node:http";

export function buildTestImage(size: number): Buffer {
  const buf = Buffer.alloc(size);
  for (let i = 0; i < size; i += 1) buf[i] = i & 0xff;
  return buf;
}

function rangeTestPageHtml(): string {
  // The test runner calls `window.__rangeFetch()` from Playwright.
  return `<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Range fetch test</title>
  </head>
  <body>
    <script>
      window.__rangeFetch = async function (url, rangeHeaderValue, extraHeaders) {
        try {
          const headers = {
            ...(extraHeaders || {}),
            Range: rangeHeaderValue,
          };
          const response = await fetch(url, {
            mode: "cors",
            headers,
          });
          const bytes = new Uint8Array(await response.arrayBuffer());
          return {
            ok: true,
            status: response.status,
            contentRange: response.headers.get("Content-Range"),
            acceptRanges: response.headers.get("Accept-Ranges"),
            contentLength: response.headers.get("Content-Length"),
            contentType: response.headers.get("Content-Type"),
            bytes: Array.from(bytes),
          };
        } catch (err) {
          return { ok: false, error: String(err) };
        }
      };
    </script>
  </body>
</html>`;
}

type ListeningServer = {
  origin: string;
  port: number;
  close: () => Promise<void>;
};

async function listen(server: Server): Promise<ListeningServer> {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("Unexpected server address");
  }
  const origin = `http://${address.address}:${address.port}`;
  return {
    origin,
    port: address.port,
    close: async () =>
      await new Promise<void>((resolve, reject) => {
        server.close((err) => (err ? reject(err) : resolve()));
      }),
  };
}

function setCorsHeaders(res: ServerResponse, req: IncomingMessage): void {
  const requestOrigin = req.headers.origin;
  // Echo the request origin if one is provided so that we can still pass CORS
  // checks even if clients opt into credentials in the future.
  res.setHeader("Access-Control-Allow-Origin", requestOrigin ?? "*");
  if (requestOrigin) res.setHeader("Vary", "Origin");
  res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  const requestedHeaders = req.headers["access-control-request-headers"];
  const requestedHeadersValue = typeof requestedHeaders === "string" ? requestedHeaders : "";
  const hasRange = requestedHeadersValue.toLowerCase().includes("range");
  const allowHeaders = requestedHeadersValue
    ? hasRange
      ? requestedHeadersValue
      : `Range, ${requestedHeadersValue}`
    : "Range";
  res.setHeader("Access-Control-Allow-Headers", allowHeaders);
  res.setHeader("Access-Control-Expose-Headers", "Content-Range, Accept-Ranges, Content-Length");
}

type ParsedRange = { start: number; endInclusive: number };

function parseRangeHeader(rangeHeader: string, size: number): ParsedRange | null {
  // Only single-range requests are supported for these tests.
  const match = /^bytes=(\d+)-(\d+)?$/i.exec(rangeHeader.trim());
  if (!match) return null;

  const start = Number(match[1]);
  const endInclusive = match[2] === undefined || match[2] === "" ? size - 1 : Number(match[2]);

  if (!Number.isFinite(start) || !Number.isFinite(endInclusive)) return null;
  if (start < 0 || endInclusive < start || start >= size) return null;

  return { start, endInclusive: Math.min(endInclusive, size - 1) };
}

export type RequestRecord = { method: string; url: string; headers: IncomingHttpHeaders };

export type DiskImageServer = {
  origin: string;
  port: number;
  url: (path: string) => string;
  requests: RequestRecord[];
  resetRequests: () => void;
  close: () => Promise<void>;
};

export async function startDiskImageServer(opts: {
  data: Buffer;
  enableCors: boolean;
  serveTestPage?: boolean;
}): Promise<DiskImageServer> {
  const requests: RequestRecord[] = [];

  const server = http.createServer((req, res) => {
    requests.push({ method: req.method ?? "", url: req.url ?? "", headers: req.headers });

    const url = new URL(req.url ?? "/", "http://localhost");

    if (opts.serveTestPage && req.method === "GET" && url.pathname === "/") {
      res.statusCode = 200;
      res.setHeader("Content-Type", "text/html; charset=utf-8");
      res.end(rangeTestPageHtml());
      return;
    }

    if (url.pathname !== "/disk.img") {
      res.statusCode = 404;
      res.end("Not found");
      return;
    }

    if (opts.enableCors) setCorsHeaders(res, req);

    if (req.method === "OPTIONS") {
      res.statusCode = 204;
      res.end();
      return;
    }

    if (req.method !== "GET" && req.method !== "HEAD") {
      res.statusCode = 405;
      res.setHeader("Allow", "GET, HEAD, OPTIONS");
      res.end();
      return;
    }

    res.setHeader("Accept-Ranges", "bytes");
    res.setHeader("Content-Type", "application/octet-stream");
    res.setHeader("Cache-Control", "no-transform");
    res.setHeader("Content-Encoding", "identity");

    const rangeHeader = req.headers.range;
    if (typeof rangeHeader === "string") {
      const parsedRange = parseRangeHeader(rangeHeader, opts.data.length);
      if (!parsedRange) {
        res.statusCode = 416;
        res.setHeader("Content-Range", `bytes */${opts.data.length}`);
        res.end();
        return;
      }

      const body = opts.data.subarray(parsedRange.start, parsedRange.endInclusive + 1);

      res.statusCode = 206;
      res.setHeader(
        "Content-Range",
        `bytes ${parsedRange.start}-${parsedRange.endInclusive}/${opts.data.length}`,
      );
      res.setHeader("Content-Length", String(body.length));

      if (req.method === "HEAD") {
        res.end();
        return;
      }

      res.end(body);
      return;
    }

    // No Range header: return full content.
    res.statusCode = 200;
    res.setHeader("Content-Length", String(opts.data.length));
    if (req.method === "HEAD") {
      res.end();
      return;
    }
    res.end(opts.data);
  });

  const listening = await listen(server);

  return {
    origin: listening.origin,
    port: listening.port,
    url: (path) => `${listening.origin}${path}`,
    requests,
    resetRequests: () => {
      requests.length = 0;
    },
    close: listening.close,
  };
}

export type PageServer = { origin: string; port: number; close: () => Promise<void> };

export async function startPageServer({ coopCoep = false }: { coopCoep?: boolean } = {}): Promise<PageServer> {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname !== "/") {
      res.statusCode = 404;
      res.end("Not found");
      return;
    }

    if (coopCoep) {
      res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
      res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
    }

    res.statusCode = 200;
    res.setHeader("Content-Type", "text/html; charset=utf-8");
    res.end(rangeTestPageHtml());
  });

  return await listen(server);
}
