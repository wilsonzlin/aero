import fs from "node:fs";
import path from "node:path";
import { promisify } from "node:util";
import dns from "node:dns/promises";
import { getAuthTokenFromRequest, isOriginAllowed, isTokenAllowed } from "./auth.js";
import { isHostAllowed, isIpAllowed } from "./policy.js";

const stat = promisify(fs.stat);
const MAX_REQUEST_URL_LEN = 8 * 1024;

function setCrossOriginIsolationHeaders(res) {
  res.setHeader("Cross-Origin-Opener-Policy", "same-origin");
  res.setHeader("Cross-Origin-Embedder-Policy", "require-corp");
  res.setHeader("Cross-Origin-Resource-Policy", "same-origin");
  res.setHeader("Origin-Agent-Cluster", "?1");
}

function setCommonSecurityHeaders(res) {
  res.setHeader("X-Content-Type-Options", "nosniff");
  res.setHeader("Referrer-Policy", "no-referrer");
  res.setHeader("Permissions-Policy", "camera=(), geolocation=(), microphone=(self), usb=(self)");
}

function setContentSecurityPolicy(res) {
  // Aero relies on dynamic WebAssembly compilation for its WASM-based JIT tier.
  // CSP controls this via `script-src 'wasm-unsafe-eval'` (preferred over 'unsafe-eval').
  res.setHeader(
    "Content-Security-Policy",
    "default-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; connect-src 'self' https://aero-gateway.invalid wss://aero-gateway.invalid; img-src 'self' data: blob:; style-src 'self'; font-src 'self'",
  );
}

function guessContentType(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  switch (ext) {
    case ".html":
      return "text/html; charset=utf-8";
    case ".js":
      return "text/javascript; charset=utf-8";
    case ".css":
      return "text/css; charset=utf-8";
    case ".json":
      return "application/json; charset=utf-8";
    case ".wasm":
      return "application/wasm";
    case ".png":
      return "image/png";
    case ".jpg":
    case ".jpeg":
      return "image/jpeg";
    case ".svg":
      return "image/svg+xml";
    case ".txt":
      return "text/plain; charset=utf-8";
    default:
      return "application/octet-stream";
  }
}

async function handleDnsLookup(req, res, url, { config, logger, metrics }) {
  const token = getAuthTokenFromRequest(req, url.searchParams);
  if (!isTokenAllowed(token, config.tokens)) {
    res.statusCode = 401;
    res.end("Unauthorized");
    return;
  }
  if (!isOriginAllowed(req.headers.origin, config.allowedOrigins)) {
    res.statusCode = 403;
    res.end("Forbidden");
    return;
  }

  const name = url.searchParams.get("name");
  if (!name) {
    res.statusCode = 400;
    res.end("Missing name");
    return;
  }
  if (!isHostAllowed(name, config.allowHosts)) {
    res.statusCode = 403;
    res.end("Host is not allowlisted");
    return;
  }

  metrics.increment("dnsLookupsTotal");
  const answers = await dns.lookup(name, { all: true });
  const filtered = answers.filter((a) => isIpAllowed(a.address, config.allowPrivateRanges));
  if (filtered.length === 0) {
    res.statusCode = 403;
    res.end("DNS resolved to blocked address range");
    return;
  }

  logger.info("dns_lookup", { name, answerCount: filtered.length });

  res.statusCode = 200;
  res.setHeader("Content-Type", "application/json; charset=utf-8");
  res.setHeader("Cache-Control", "no-store");
  res.end(JSON.stringify({ name, addresses: filtered }));
}

async function handleStatic(req, res, url, { config }) {
  let pathname = url.pathname;
  if (pathname === "/") pathname = "/index.html";

  // Prevent directory traversal via path normalization.
  const rootDir = path.resolve(config.staticDir);
  const targetPath = path.resolve(rootDir, "." + pathname);
  if (!targetPath.startsWith(rootDir + path.sep) && targetPath !== rootDir) {
    res.statusCode = 400;
    res.end("Bad path");
    return;
  }

  let st;
  try {
    st = await stat(targetPath);
  } catch {
    res.statusCode = 404;
    res.end("Not found");
    return;
  }
  if (!st.isFile()) {
    res.statusCode = 404;
    res.end("Not found");
    return;
  }

  res.statusCode = 200;
  res.setHeader("Content-Type", guessContentType(targetPath));
  res.setHeader("Content-Length", st.size);
  fs.createReadStream(targetPath).pipe(res);
}

export function createHttpHandler({ config, logger, metrics }) {
  return (req, res) => {
    void (async () => {
      setCrossOriginIsolationHeaders(res);
      setCommonSecurityHeaders(res);
      setContentSecurityPolicy(res);

      const rawUrl = req.url ?? "/";
      if (typeof rawUrl !== "string") {
        res.statusCode = 400;
        res.setHeader("Content-Type", "text/plain; charset=utf-8");
        res.end("Bad Request");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        res.statusCode = 414;
        res.setHeader("Content-Type", "text/plain; charset=utf-8");
        res.end("URI Too Long");
        return;
      }

      const url = new URL(rawUrl, "http://localhost");

      if (req.method === "GET" && url.pathname === "/healthz") {
        res.statusCode = 200;
        res.setHeader("Content-Type", "text/plain; charset=utf-8");
        res.end("ok");
        return;
      }

      if (req.method === "GET" && url.pathname === "/metrics") {
        res.statusCode = 200;
        res.setHeader("Content-Type", "text/plain; charset=utf-8");
        res.end(metrics.toPrometheus());
        return;
      }

      if (req.method === "GET" && url.pathname === "/api/dns/lookup") {
        await handleDnsLookup(req, res, url, { config, logger, metrics });
        return;
      }

      await handleStatic(req, res, url, { config });
    })().catch((err) => {
      logger.error("http_error", { err: err?.message ?? String(err) });
      if (!res.headersSent) res.statusCode = 500;
      res.end("Internal Server Error");
    });
  };
}
