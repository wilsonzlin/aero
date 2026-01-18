import fs from "node:fs";
import path from "node:path";
import { pipeline } from "node:stream/promises";
import { promisify } from "node:util";
import dns from "node:dns/promises";
import { getAuthTokenFromRequest, isOriginAllowed, isTokenAllowed } from "./auth.js";
import { isHostAllowed, isIpAllowed } from "./policy.js";
import { formatOneLineError } from "./text.js";
import { isExpectedStreamAbort } from "../../src/stream_abort.js";
import { tryGetProp, tryGetStringProp } from "../../src/safe_props.js";

const stat = promisify(fs.stat);
const MAX_REQUEST_URL_LEN = 8 * 1024;
const MAX_PATHNAME_LEN = 4 * 1024;

function trySetHeader(res, name, value) {
  try {
    res.setHeader(name, value);
    return true;
  } catch {
    return false;
  }
}

function isResponseDestroyed(res) {
  try {
    return res.destroyed === true;
  } catch {
    // Fail closed: if we can't observe state, treat it as already destroyed.
    return true;
  }
}

function setCrossOriginIsolationHeaders(res) {
  trySetHeader(res, "Cross-Origin-Opener-Policy", "same-origin");
  trySetHeader(res, "Cross-Origin-Embedder-Policy", "require-corp");
  trySetHeader(res, "Cross-Origin-Resource-Policy", "same-origin");
  trySetHeader(res, "Origin-Agent-Cluster", "?1");
}

function setCommonSecurityHeaders(res) {
  trySetHeader(res, "X-Content-Type-Options", "nosniff");
  trySetHeader(res, "Referrer-Policy", "no-referrer");
  trySetHeader(res, "Permissions-Policy", "camera=(), geolocation=(), microphone=(self), usb=(self)");
}

function setContentSecurityPolicy(res) {
  // Aero relies on dynamic WebAssembly compilation for its WASM-based JIT tier.
  // CSP controls this via `script-src 'wasm-unsafe-eval'` (preferred over 'unsafe-eval').
  trySetHeader(
    res,
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

function sendText(res, statusCode, message) {
  // Defensive: response encoding must never throw.
  let body;
  let code = statusCode;
  try {
    body = Buffer.from(String(message), "utf8");
  } catch {
    code = 500;
    body = Buffer.from("Internal Server Error\n", "utf8");
  }

  try {
    res.statusCode = code;
    res.setHeader("Content-Type", "text/plain; charset=utf-8");
    res.setHeader("Content-Length", body.length);
    res.setHeader("Cache-Control", "no-store");
    res.end(body);
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

function sendJson(res, statusCode, value) {
  // Defensive: response encoding must never throw. If JSON.stringify throws
  // (cyclic data, hostile toJSON, etc.), fall back to a stable 500 JSON body.
  let body;
  let code = statusCode;
  try {
    body = Buffer.from(JSON.stringify(value), "utf8");
  } catch {
    code = 500;
    body = Buffer.from(`{"error":"internal server error"}\n`, "utf8");
  }

  try {
    res.statusCode = code;
    res.setHeader("Content-Type", "application/json; charset=utf-8");
    res.setHeader("Content-Length", body.length);
    res.setHeader("Cache-Control", "no-store");
    res.end(body);
  } catch {
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

async function handleDnsLookup(req, res, url, { config, logger, metrics }) {
  const token = getAuthTokenFromRequest(req, url.searchParams);
  if (!isTokenAllowed(token, config.tokens)) {
    sendText(res, 401, "Unauthorized");
    return;
  }
  const origin = tryGetProp(tryGetProp(req, "headers"), "origin");
  if (!isOriginAllowed(origin, config.allowedOrigins)) {
    sendText(res, 403, "Forbidden");
    return;
  }

  const name = url.searchParams.get("name");
  if (!name) {
    sendText(res, 400, "Missing name");
    return;
  }
  if (!isHostAllowed(name, config.allowHosts)) {
    sendText(res, 403, "Host is not allowlisted");
    return;
  }

  metrics.increment("dnsLookupsTotal");
  const answers = await dns.lookup(name, { all: true });
  const filtered = answers.filter((a) => isIpAllowed(a.address, config.allowPrivateRanges));
  if (filtered.length === 0) {
    sendText(res, 403, "DNS resolved to blocked address range");
    return;
  }

  logger.info("dns_lookup", { name, answerCount: filtered.length });

  sendJson(res, 200, { name, addresses: filtered });
}

async function handleStatic(req, res, url, { config, logger }) {
  let pathname = url.pathname;
  if (pathname === "/") pathname = "/index.html";

  // Prevent directory traversal via path normalization.
  const rootDir = path.resolve(config.staticDir);
  const targetPath = path.resolve(rootDir, "." + pathname);
  if (!targetPath.startsWith(rootDir + path.sep) && targetPath !== rootDir) {
    sendText(res, 400, "Bad path");
    return;
  }

  let st;
  try {
    st = await stat(targetPath);
  } catch {
    sendText(res, 404, "Not found");
    return;
  }
  if (!st.isFile()) {
    sendText(res, 404, "Not found");
    return;
  }

  const stream = fs.createReadStream(targetPath);
  try {
    await new Promise((resolve, reject) => {
      const onOpen = () => {
        cleanup();
        resolve();
      };
      const onError = (err) => {
        cleanup();
        reject(err);
      };
      const cleanup = () => {
        stream.off("open", onOpen);
        stream.off("error", onError);
      };
      stream.once("open", onOpen);
      stream.once("error", onError);
    });
  } catch (err) {
    // Avoid leaking error details to clients; keep bounded one-line logs for debugging.
    logger.error("static_stream_error", { err: formatOneLineError(err, 512) });
    sendText(res, 500, "Internal Server Error");
    return;
  }

  if (isResponseDestroyed(res)) {
    try {
      stream.destroy();
    } catch {
      // ignore
    }
    return;
  }

  try {
    res.statusCode = 200;
    res.setHeader("Content-Type", guessContentType(targetPath));
    res.setHeader("Content-Length", st.size);
  } catch {
    try {
      stream.destroy();
    } catch {
      // ignore
    }
    try {
      res.destroy();
    } catch {
      // ignore
    }
    return;
  }
  try {
    await pipeline(stream, res);
  } catch (err) {
    // Treat client disconnects/aborts as expected; avoid noisy error logs.
    if (!isExpectedStreamAbort(err)) {
      logger.error("static_stream_error", { err: formatOneLineError(err, 512) });
    }
    try {
      res.destroy();
    } catch {
      // ignore
    }
  }
}

export function createHttpHandler({ config, logger, metrics }) {
  return (req, res) => {
    void (async () => {
      setCrossOriginIsolationHeaders(res);
      setCommonSecurityHeaders(res);
      setContentSecurityPolicy(res);

      const method = tryGetStringProp(req, "method") ?? "GET";
      const rawUrl = tryGetProp(req, "url");
      if (typeof rawUrl !== "string" || rawUrl === "" || rawUrl.trim() !== rawUrl) {
        sendText(res, 400, "Bad Request");
        return;
      }
      if (rawUrl.length > MAX_REQUEST_URL_LEN) {
        sendText(res, 414, "URI Too Long");
        return;
      }

      let url;
      try {
        url = new URL(rawUrl, "http://localhost");
      } catch {
        sendText(res, 400, "Bad Request");
        return;
      }
      if (url.pathname.length > MAX_PATHNAME_LEN) {
        sendText(res, 414, "URI Too Long");
        return;
      }

      if (method === "GET" && url.pathname === "/healthz") {
        sendText(res, 200, "ok");
        return;
      }

      if (method === "GET" && url.pathname === "/metrics") {
        const body = metrics.toPrometheus();
        sendText(res, 200, body);
        return;
      }

      if (method === "GET" && url.pathname === "/api/dns/lookup") {
        await handleDnsLookup(req, res, url, { config, logger, metrics });
        return;
      }

      await handleStatic(req, res, url, { config, logger });
    })().catch((err) => {
      logger.error("http_error", { err: formatOneLineError(err, 512) });
      sendText(res, 500, "Internal Server Error");
    });
  };
}
