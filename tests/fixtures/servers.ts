import http from "node:http";
import { once } from "node:events";
import type { IncomingHttpHeaders, IncomingMessage, Server, ServerResponse } from "node:http";

const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;
const MAX_ORIGIN_LEN = 4 * 1024;
const MAX_CORS_REQUEST_HEADERS_LEN = 4 * 1024;
const MAX_RANGE_HEADER_LEN = 16 * 1024;

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
      const UTF8 = Object.freeze({ encoding: "utf-8" });
      const MAX_ERROR_BYTES = 512;
      const textEncoder = new TextEncoder();
      const textDecoder = new TextDecoder(UTF8.encoding);

      function formatOneLineUtf8(input, maxBytes) {
        if (!Number.isInteger(maxBytes) || maxBytes < 0) return "";
        if (maxBytes === 0) return "";
        const buf = new Uint8Array(maxBytes);
        let written = 0;
        let pendingSpace = false;
        for (const ch of String(input ?? "")) {
          const code = ch.codePointAt(0) ?? 0;
          const forbidden = code <= 0x1f || code === 0x7f || code === 0x85 || code === 0x2028 || code === 0x2029;
          if (forbidden || /\\s/u.test(ch)) {
            pendingSpace = written > 0;
            continue;
          }
          if (pendingSpace) {
            const spaceRes = textEncoder.encodeInto(" ", buf.subarray(written));
            if (spaceRes.written === 0) break;
            written += spaceRes.written;
            pendingSpace = false;
            if (written >= maxBytes) break;
          }
          const res = textEncoder.encodeInto(ch, buf.subarray(written));
          if (res.written === 0) break;
          written += res.written;
          if (written >= maxBytes) break;
        }
        return written === 0 ? "" : textDecoder.decode(buf.subarray(0, written));
      }

      function safeErrorMessageInput(err) {
        if (err === null) return "null";

        const t = typeof err;
        if (t === "string") return err;
        if (t === "number" || t === "boolean" || t === "bigint" || t === "symbol" || t === "undefined") return String(err);

        if (t === "object") {
          try {
            const msg = err && typeof err.message === "string" ? err.message : null;
            if (msg !== null) return msg;
          } catch {
            // ignore getters throwing
          }
        }

        // Avoid calling toString() on arbitrary objects/functions (can throw / be expensive).
        return "Error";
      }

      // Expose bounded helpers for other page.evaluate calls.
      window.__formatOneLineUtf8 = formatOneLineUtf8;
      window.__safeErrorMessageInput = safeErrorMessageInput;

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
          return { ok: false, error: formatOneLineUtf8(safeErrorMessageInput(err), MAX_ERROR_BYTES) || "Error" };
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

function asciiLowerCode(code: number): number {
  return code >= 0x41 && code <= 0x5a ? code + 0x20 : code;
}

function isTcharCode(code: number): boolean {
  // RFC 7230 tchar:
  // "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." / "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
  if (code >= 0x30 && code <= 0x39) return true; // 0-9
  if (code >= 0x41 && code <= 0x5a) return true; // A-Z
  if (code >= 0x61 && code <= 0x7a) return true; // a-z
  switch (code) {
    case 0x21: // !
    case 0x23: // #
    case 0x24: // $
    case 0x25: // %
    case 0x26: // &
    case 0x27: // '
    case 0x2a: // *
    case 0x2b: // +
    case 0x2d: // -
    case 0x2e: // .
    case 0x5e: // ^
    case 0x5f: // _
    case 0x60: // `
    case 0x7c: // |
    case 0x7e: // ~
      return true;
    default:
      return false;
  }
}

function isSafeHeaderListValue(value: string): boolean {
  // Allow only: tchar tokens separated by commas and optional whitespace.
  for (let i = 0; i < value.length; i += 1) {
    const code = value.charCodeAt(i);
    if (isTcharCode(code)) continue;
    if (code === 0x2c /* , */ || code === 0x20 /* space */ || code === 0x09 /* tab */) continue;
    return false;
  }
  return true;
}

function headerListHasToken(value: string, tokenLower: string): boolean {
  // Scans an RFC7230-ish token list like: "a, b, c".
  const targetLen = tokenLower.length;
  const valueLen = value.length;
  let i = 0;
  while (i < valueLen) {
    // Skip OWS and commas.
    while (i < valueLen) {
      const c = value.charCodeAt(i);
      if (c === 0x20 /* space */ || c === 0x09 /* tab */ || c === 0x2c /* , */) i += 1;
      else break;
    }
    const start = i;
    while (i < valueLen && isTcharCode(value.charCodeAt(i))) i += 1;
    const end = i;
    if (end > start && end - start === targetLen) {
      let match = true;
      for (let j = 0; j < targetLen; j += 1) {
        if (asciiLowerCode(value.charCodeAt(start + j)) !== tokenLower.charCodeAt(j)) {
          match = false;
          break;
        }
      }
      if (match) return true;
    }
    // Skip any trailing OWS before the next comma.
    while (i < valueLen) {
      const c = value.charCodeAt(i);
      if (c === 0x20 /* space */ || c === 0x09 /* tab */) i += 1;
      else break;
    }
    if (i < valueLen && value.charCodeAt(i) === 0x2c /* , */) i += 1;
  }
  return false;
}

function corsAllowHeadersValue(req: IncomingMessage): string {
  const requestedHeaders = req.headers["access-control-request-headers"];
  if (typeof requestedHeaders !== "string") return "Range";
  const requested = requestedHeaders;
  if (requested.length > MAX_CORS_REQUEST_HEADERS_LEN) return "Range";
  if (!isSafeHeaderListValue(requested)) return "Range";
  if (headerListHasToken(requested, "range")) return requested;
  return requested.trim() ? `Range, ${requested}` : "Range";
}

function setCorsHeaders(res: ServerResponse, req: IncomingMessage): void {
  const requestOrigin = typeof req.headers.origin === "string" ? req.headers.origin : undefined;
  // Echo the request origin if one is provided so that we can still pass CORS
  // checks even if clients opt into credentials in the future.
  if (requestOrigin && requestOrigin.length <= MAX_ORIGIN_LEN) {
    res.setHeader("Access-Control-Allow-Origin", requestOrigin);
    res.setHeader("Vary", "Origin");
  } else {
    res.setHeader("Access-Control-Allow-Origin", "*");
  }
  res.setHeader("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", corsAllowHeadersValue(req));
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

  // Increase the server-side header limit so we can exercise our own request
  // caps deterministically in tests (Node's default maxHeaderSize is otherwise
  // close enough to our Range cap that large requests can be rejected as 431
  // before reaching this handler).
  const server = http.createServer({ maxHeaderSize: 64 * 1024 }, (req, res) => {
    requests.push({ method: req.method ?? "", url: req.url ?? "", headers: req.headers });
    if (opts.enableCors) setCorsHeaders(res, req);

    const rawUrl = req.url ?? "/";
    if (typeof rawUrl !== "string") {
      res.statusCode = 400;
      res.end("Bad Request");
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      res.statusCode = 414;
      res.end("URI Too Long");
      return;
    }

    let url: URL;
    try {
      url = new URL(rawUrl, "http://localhost");
    } catch {
      res.statusCode = 400;
      res.end("Bad Request");
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      res.statusCode = 414;
      res.end("URI Too Long");
      return;
    }

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
      if (rangeHeader.length > MAX_RANGE_HEADER_LEN) {
        res.statusCode = 413;
        res.end();
        return;
      }
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
    const rawUrl = req.url ?? "/";
    if (typeof rawUrl !== "string") {
      res.statusCode = 400;
      res.end("Bad Request");
      return;
    }
    if (rawUrl.length > MAX_REQUEST_URL_LEN) {
      res.statusCode = 414;
      res.end("URI Too Long");
      return;
    }

    let url: URL;
    try {
      url = new URL(rawUrl, "http://localhost");
    } catch {
      res.statusCode = 400;
      res.end("Bad Request");
      return;
    }
    if (url.pathname.length > MAX_PATHNAME_LEN) {
      res.statusCode = 414;
      res.end("URI Too Long");
      return;
    }
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
