import http from "node:http";
import net from "node:net";
import dgram from "node:dgram";
import dns from "node:dns/promises";
import { PassThrough, type Duplex } from "node:stream";
import { createWebSocketStream, WebSocketServer, type WebSocket } from "ws";
import ipaddr from "ipaddr.js";
import { loadConfigFromEnv, type ProxyConfig } from "./config";
import { formatError, log } from "./logger";
import { resolveAndAuthorizeTarget } from "./security";
import { decodeBase64UrlToBuffer, decodeDnsHeader, decodeFirstQuestion, encodeDnsResponse, normalizeDnsName, type DnsAnswer } from "./dnsMessage";
import {
  TCP_MUX_HEADER_BYTES,
  TCP_MUX_SUBPROTOCOL,
  TcpMuxCloseFlags,
  TcpMuxErrorCode,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  type TcpMuxFrame
} from "./tcpMuxProtocol";
import { decodeUdpRelayFrame, encodeUdpRelayV1Datagram, encodeUdpRelayV2Datagram } from "./udpRelayProtocol";

export interface RunningProxyServer {
  server: http.Server;
  config: ProxyConfig;
  listenAddress: string;
  close: () => Promise<void>;
}

type MetricsProto = "tcp" | "tcp_mux" | "udp";
type MetricsErrorKind = "denied" | "error";

interface ProxyServerMetrics {
  connectionActiveInc: (proto: MetricsProto) => void;
  connectionActiveDec: (proto: MetricsProto) => void;
  tcpMuxStreamsActiveInc: () => void;
  tcpMuxStreamsActiveDec: (delta?: number) => void;
  udpBindingsActiveInc: () => void;
  udpBindingsActiveDec: (delta?: number) => void;
  addBytesIn: (proto: MetricsProto, bytes: number) => void;
  addBytesOut: (proto: MetricsProto, bytes: number) => void;
  incConnectionError: (kind: MetricsErrorKind) => void;
  prometheusText: () => string;
}

function createProxyServerMetrics(): ProxyServerMetrics {
  const connectionsActive: Record<MetricsProto, number> = {
    tcp: 0,
    tcp_mux: 0,
    udp: 0
  };
  const bytesInTotal: Record<MetricsProto, bigint> = {
    tcp: 0n,
    tcp_mux: 0n,
    udp: 0n
  };
  const bytesOutTotal: Record<MetricsProto, bigint> = {
    tcp: 0n,
    tcp_mux: 0n,
    udp: 0n
  };
  const connectionErrorsTotal: Record<MetricsErrorKind, bigint> = {
    denied: 0n,
    error: 0n
  };
  let udpBindingsActive = 0;
  let tcpMuxStreamsActive = 0;

  const clampNonNegative = (n: number): number => (n < 0 ? 0 : n);

  const prometheusText = (): string => {
    const lines: string[] = [];

    lines.push("# HELP net_proxy_connections_active Active relay WebSocket connections.");
    lines.push("# TYPE net_proxy_connections_active gauge");
    lines.push(`net_proxy_connections_active{proto="tcp"} ${connectionsActive.tcp}`);
    lines.push(`net_proxy_connections_active{proto="tcp_mux"} ${connectionsActive.tcp_mux}`);
    lines.push(`net_proxy_connections_active{proto="udp"} ${connectionsActive.udp}`);

    lines.push("# HELP net_proxy_tcp_connections_active Active TCP relay connections (outbound TCP sockets).");
    lines.push("# TYPE net_proxy_tcp_connections_active gauge");
    lines.push(`net_proxy_tcp_connections_active{proto="tcp"} ${connectionsActive.tcp}`);
    lines.push(`net_proxy_tcp_connections_active{proto="tcp_mux"} ${tcpMuxStreamsActive}`);

    lines.push("# HELP net_proxy_udp_bindings_active Active UDP bindings in multiplexed /udp mode.");
    lines.push("# TYPE net_proxy_udp_bindings_active gauge");
    lines.push(`net_proxy_udp_bindings_active ${udpBindingsActive}`);

    lines.push("# HELP net_proxy_bytes_in_total Total bytes received from the client (towards target sockets), by protocol.");
    lines.push("# TYPE net_proxy_bytes_in_total counter");
    lines.push(`net_proxy_bytes_in_total{proto="tcp"} ${bytesInTotal.tcp}`);
    lines.push(`net_proxy_bytes_in_total{proto="tcp_mux"} ${bytesInTotal.tcp_mux}`);
    lines.push(`net_proxy_bytes_in_total{proto="udp"} ${bytesInTotal.udp}`);

    lines.push("# HELP net_proxy_bytes_out_total Total bytes sent to the client (from target sockets), by protocol.");
    lines.push("# TYPE net_proxy_bytes_out_total counter");
    lines.push(`net_proxy_bytes_out_total{proto="tcp"} ${bytesOutTotal.tcp}`);
    lines.push(`net_proxy_bytes_out_total{proto="tcp_mux"} ${bytesOutTotal.tcp_mux}`);
    lines.push(`net_proxy_bytes_out_total{proto="udp"} ${bytesOutTotal.udp}`);

    lines.push("# HELP net_proxy_connection_errors_total Total connection errors (denied by policy or failed to connect).");
    lines.push("# TYPE net_proxy_connection_errors_total counter");
    lines.push(`net_proxy_connection_errors_total{kind="denied"} ${connectionErrorsTotal.denied}`);
    lines.push(`net_proxy_connection_errors_total{kind="error"} ${connectionErrorsTotal.error}`);

    return `${lines.join("\n")}\n`;
  };

  return {
    connectionActiveInc: (proto) => {
      connectionsActive[proto] = clampNonNegative(connectionsActive[proto] + 1);
    },
    connectionActiveDec: (proto) => {
      connectionsActive[proto] = clampNonNegative(connectionsActive[proto] - 1);
    },
    tcpMuxStreamsActiveInc: () => {
      tcpMuxStreamsActive = clampNonNegative(tcpMuxStreamsActive + 1);
    },
    tcpMuxStreamsActiveDec: (delta = 1) => {
      tcpMuxStreamsActive = clampNonNegative(tcpMuxStreamsActive - delta);
    },
    udpBindingsActiveInc: () => {
      udpBindingsActive = clampNonNegative(udpBindingsActive + 1);
    },
    udpBindingsActiveDec: (delta = 1) => {
      udpBindingsActive = clampNonNegative(udpBindingsActive - delta);
    },
    addBytesIn: (proto, bytes) => {
      if (!Number.isFinite(bytes) || bytes <= 0) return;
      bytesInTotal[proto] += BigInt(bytes);
    },
    addBytesOut: (proto, bytes) => {
      if (!Number.isFinite(bytes) || bytes <= 0) return;
      bytesOutTotal[proto] += BigInt(bytes);
    },
    incConnectionError: (kind) => {
      connectionErrorsTotal[kind] += 1n;
    },
    prometheusText
  };
}

function truncateCloseReason(reason: string, maxBytes = 123): string {
  const buf = Buffer.from(reason, "utf8");
  if (buf.length <= maxBytes) return reason;

  let truncated = buf.subarray(0, maxBytes).toString("utf8");
  while (Buffer.byteLength(truncated, "utf8") > maxBytes) {
    truncated = truncated.slice(0, -1);
  }
  return truncated;
}

function wsCloseSafe(ws: WebSocket, code: number, reason: string): void {
  const safeReason = truncateCloseReason(reason);
  ws.close(code, safeReason);
}

function rejectWsUpgrade(socket: Duplex, status: number, message: string): void {
  const statusText = status === 400 ? "Bad Request" : "Error";
  const body = `${message}\n`;
  socket.end(
    [
      `HTTP/1.1 ${status} ${statusText}`,
      "Content-Type: text/plain; charset=utf-8",
      `Content-Length: ${Buffer.byteLength(body)}`,
      "Connection: close",
      "\r\n",
      body
    ].join("\r\n")
  );
}

function stripOptionalIpv6Brackets(host: string): string {
  const trimmed = host.trim();
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function parseTargetQuery(url: URL): { host: string; port: number; portRaw: string } | { error: string } {
  const hostRaw = url.searchParams.get("host");
  const portRaw = url.searchParams.get("port");
  if (hostRaw !== null && portRaw !== null) {
    const port = Number(portRaw);
    if (hostRaw.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      return { error: "Invalid host or port" };
    }
    return { host: stripOptionalIpv6Brackets(hostRaw), port, portRaw };
  }

  const target = url.searchParams.get("target");
  if (target === null || target.trim() === "") {
    return { error: "Missing host/port (or target)" };
  }

  const t = target.trim();
  let host = "";
  let portPart = "";
  if (t.startsWith("[")) {
    const closeIdx = t.indexOf("]");
    if (closeIdx === -1) return { error: "Invalid target (missing closing ] for IPv6)" };
    host = t.slice(1, closeIdx);
    const rest = t.slice(closeIdx + 1);
    if (!rest.startsWith(":")) return { error: "Invalid target (missing :port)" };
    portPart = rest.slice(1);
  } else {
    const colonIdx = t.lastIndexOf(":");
    if (colonIdx === -1) return { error: "Invalid target (missing :port)" };
    host = t.slice(0, colonIdx);
    portPart = t.slice(colonIdx + 1);
  }

  const port = Number(portPart);
  if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
    return { error: "Invalid target host or port" };
  }

  return { host: stripOptionalIpv6Brackets(host), port, portRaw: portPart };
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, label: string): Promise<T> {
  let handle: NodeJS.Timeout | null = null;
  const timeout = new Promise<never>((_resolve, reject) => {
    handle = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs}ms`)), timeoutMs);
    handle.unref();
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (handle) clearTimeout(handle);
  }
}

async function readRequestBodyWithLimit(
  req: http.IncomingMessage,
  maxBytes: number
): Promise<{ body: Buffer; tooLarge: boolean }> {
  const chunks: Buffer[] = [];
  let storedBytes = 0;
  let totalBytes = 0;

  return await new Promise((resolve, reject) => {
    req.on("error", reject);
    req.on("data", (chunk: Buffer) => {
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      totalBytes += buf.length;
      const remaining = maxBytes - storedBytes;
      if (remaining <= 0) return;
      const slice = buf.length > remaining ? buf.subarray(0, remaining) : buf;
      chunks.push(slice);
      storedBytes += slice.length;
    });
    req.on("end", () => {
      resolve({ body: Buffer.concat(chunks, storedBytes), tooLarge: totalBytes > maxBytes });
    });
  });
}

function clampInt(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.floor(value)));
}

function sendDnsMessage(res: http.ServerResponse, statusCode: number, message: Buffer): void {
  res.writeHead(statusCode, {
    "content-type": "application/dns-message",
    "cache-control": "no-store",
    "content-length": message.length
  });
  res.end(message);
}

function setDohCorsHeaders(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  config: ProxyConfig,
  opts: { allowMethods?: string } = {}
): void {
  if (config.dohCorsAllowOrigins.length === 0) return;

  const requestOrigin = req.headers.origin;
  if (config.dohCorsAllowOrigins.includes("*")) {
    res.setHeader("Access-Control-Allow-Origin", "*");
  } else if (typeof requestOrigin === "string" && config.dohCorsAllowOrigins.includes(requestOrigin)) {
    res.setHeader("Access-Control-Allow-Origin", requestOrigin);
    res.setHeader("Vary", "Origin");
  } else {
    return;
  }

  if (opts.allowMethods) {
    res.setHeader("Access-Control-Allow-Methods", opts.allowMethods);
  }

  const requestedHeaders = req.headers["access-control-request-headers"];
  const requestedHeadersValue = typeof requestedHeaders === "string" ? requestedHeaders : "";
  // Always allow Content-Type for RFC8484 POST, even if the client didn't send a preflight header.
  const allowHeaders = requestedHeadersValue ? `Content-Type, ${requestedHeadersValue}` : "Content-Type";
  res.setHeader("Access-Control-Allow-Headers", allowHeaders);

  // Allow browsers to read Content-Length cross-origin (useful for client-side size enforcement).
  res.setHeader("Access-Control-Expose-Headers", "Content-Length");

  // Cache preflight results in the browser to avoid an extra roundtrip for each DNS query during
  // local development.
  res.setHeader("Access-Control-Max-Age", "600");

  // Private Network Access (PNA) support: some browsers require an explicit opt-in response when a
  // secure context fetches a private-network target (e.g. localhost).
  const reqPrivateNetwork = req.headers["access-control-request-private-network"];
  const privateNetworkValue = typeof reqPrivateNetwork === "string" ? reqPrivateNetwork : "";
  if (privateNetworkValue.trim().toLowerCase() === "true") {
    res.setHeader("Access-Control-Allow-Private-Network", "true");
  }
}

function maxBase64UrlLenForBytes(byteLength: number): number {
  const n = clampInt(byteLength, 0, Number.MAX_SAFE_INTEGER);
  const fullTriplets = Math.floor(n / 3);
  const rem = n % 3;
  if (rem === 0) return fullTriplets * 4;
  if (rem === 1) return fullTriplets * 4 + 2;
  return fullTriplets * 4 + 3;
}

function base64UrlPrefixForHeader(base64url: string, maxChars = 16): string {
  let len = Math.min(base64url.length, maxChars);
  // `decodeBase64UrlToBuffer` rejects lengths with `len % 4 === 1`.
  if (len % 4 === 1) len -= 1;
  if (len <= 0) return "";
  return base64url.slice(0, len);
}

async function handleDnsQuery(req: http.IncomingMessage, res: http.ServerResponse, url: URL, config: ProxyConfig): Promise<void> {
  if (req.method !== "GET" && req.method !== "POST") {
    sendDnsMessage(res, 405, encodeDnsResponse({ id: 0, rcode: 1 }));
    return;
  }

  let query: Buffer;
  let tooLarge = false;
  try {
    if (req.method === "GET") {
      const dnsParam = url.searchParams.get("dns");
      if (!dnsParam) {
        sendDnsMessage(res, 400, encodeDnsResponse({ id: 0, rcode: 1 }));
        return;
      }
      // Avoid decoding arbitrarily large `dns` query params into buffers. For valid base64url,
      // the encoded length is strictly monotonic with decoded byte length, so we can enforce
      // `dohMaxQueryBytes` before decoding the full message.
      const maxEncodedLen = maxBase64UrlLenForBytes(config.dohMaxQueryBytes);
      if (dnsParam.length > maxEncodedLen) {
        tooLarge = true;
        // Best-effort decode of the DNS header (first 12 bytes) so we can preserve query ID/flags
        // in the 413 response without allocating the entire message.
        const prefix = base64UrlPrefixForHeader(dnsParam, 16);
        try {
          query = prefix ? decodeBase64UrlToBuffer(prefix) : Buffer.alloc(0);
        } catch {
          query = Buffer.alloc(0);
        }
      } else {
        query = decodeBase64UrlToBuffer(dnsParam);
      }
    } else {
      const contentType = (req.headers["content-type"] ?? "").split(";", 1)[0]?.trim().toLowerCase();
      if (contentType !== "application/dns-message") {
        sendDnsMessage(res, 415, encodeDnsResponse({ id: 0, rcode: 1 }));
        return;
      }
      const bodyResult = await readRequestBodyWithLimit(req, config.dohMaxQueryBytes);
      query = bodyResult.body;
      tooLarge = bodyResult.tooLarge;
    }
  } catch {
    sendDnsMessage(res, 400, encodeDnsResponse({ id: 0, rcode: 1 }));
    return;
  }

  if (tooLarge || query.length > config.dohMaxQueryBytes) {
    let id = 0;
    let queryFlags = 0;
    try {
      const header = decodeDnsHeader(query);
      id = header.id;
      queryFlags = header.flags;
    } catch {
      if (query.length >= 2) id = query.readUInt16BE(0);
      if (query.length >= 4) queryFlags = query.readUInt16BE(2);
    }
    sendDnsMessage(res, 413, encodeDnsResponse({ id, queryFlags, rcode: 1 }));
    return;
  }

  let id = 0;
  let queryFlags = 0;
  try {
    const header = decodeDnsHeader(query);
    id = header.id;
    queryFlags = header.flags;
  } catch {
    if (query.length >= 2) id = query.readUInt16BE(0);
    if (query.length >= 4) queryFlags = query.readUInt16BE(2);
  }

  let question;
  try {
    question = decodeFirstQuestion(query, { maxQnameLength: config.dohMaxQnameLength });
  } catch {
    // FORMERR
    sendDnsMessage(res, 400, encodeDnsResponse({ id, queryFlags, rcode: 1 }));
    return;
  }

  // Only IN is supported.
  if (question.class !== 1) {
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers: [] }));
    return;
  }

  const qtype = question.type;
  // Supported: A (1) and AAAA (28). Other qtypes return NOERROR with no answers.
  if (qtype !== 1 && qtype !== 28) {
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers: [] }));
    return;
  }

  const ttl = clampInt(config.dohAnswerTtlSeconds, 0, config.dohMaxAnswerTtlSeconds);
  const maxAnswers = clampInt(config.dohMaxAnswers, 0, 256);

  let answers: DnsAnswer[] = [];
  try {
    const qname = normalizeDnsName(question.name);
    const family = qtype === 1 ? 4 : 6;
    const resolved = await withTimeout(dns.lookup(qname, { family, all: true, verbatim: true }), config.dnsTimeoutMs, "dns lookup");
    for (const addr of resolved) {
      if (answers.length >= maxAnswers) break;
      try {
        const parsed = ipaddr.parse(stripIpv6ZoneIndex(addr.address));
        const bytes = Buffer.from(parsed.toByteArray());
        if (qtype === 1 && bytes.length !== 4) continue;
        if (qtype === 28 && bytes.length !== 16) continue;
        answers.push({ type: qtype, class: 1, ttl, rdata: bytes });
      } catch {
        // ignore bad addresses
      }
    }

    if (answers.length === 0) {
      sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 2, question: question.wire, answers: [] }));
      return;
    }
  } catch {
    // SERVFAIL (DNS error, not HTTP error)
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 2, question: question.wire, answers: [] }));
    return;
  }

  sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers }));
}

async function handleDnsJson(req: http.IncomingMessage, res: http.ServerResponse, url: URL, config: ProxyConfig): Promise<void> {
  const rawName = url.searchParams.get("name") ?? "";
  const rawType = url.searchParams.get("type") ?? "A";
  const name = normalizeDnsName(rawName);
  if (!name) {
    const body = JSON.stringify({ error: "missing name" });
    res.writeHead(400, { "content-type": "application/json; charset=utf-8", "content-length": Buffer.byteLength(body) });
    res.end(body);
    return;
  }
  if (Buffer.byteLength(name, "utf8") > config.dohMaxQnameLength) {
    const body = JSON.stringify({ error: "name too long" });
    res.writeHead(400, { "content-type": "application/json; charset=utf-8", "content-length": Buffer.byteLength(body) });
    res.end(body);
    return;
  }

  let qtype: number;
  const typeNorm = rawType.trim().toUpperCase();
  if (typeNorm === "A" || typeNorm === "1") {
    qtype = 1;
  } else if (typeNorm === "AAAA" || typeNorm === "28") {
    qtype = 28;
  } else if (typeNorm === "CNAME" || typeNorm === "5") {
    qtype = 5;
  } else {
    const body = JSON.stringify({ error: "unsupported type" });
    res.writeHead(400, { "content-type": "application/json; charset=utf-8", "content-length": Buffer.byteLength(body) });
    res.end(body);
    return;
  }

  const ttl = clampInt(config.dohAnswerTtlSeconds, 0, config.dohMaxAnswerTtlSeconds);
  const maxAnswers = clampInt(config.dohMaxAnswers, 0, 256);

  let status = 0;
  let answer: Array<{ name: string; type: number; TTL: number; data: string }> = [];
  try {
    if (qtype === 1) {
      const resolved = await withTimeout(dns.lookup(name, { family: 4, all: true, verbatim: true }), config.dnsTimeoutMs, "dns lookup");
      for (const addr of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 1, TTL: ttl, data: addr.address });
      }
    } else if (qtype === 28) {
      const resolved = await withTimeout(dns.lookup(name, { family: 6, all: true, verbatim: true }), config.dnsTimeoutMs, "dns lookup");
      for (const addr of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 28, TTL: ttl, data: addr.address });
      }
    } else {
      const resolved = await withTimeout(dns.resolveCname(name), config.dnsTimeoutMs, "dns cname lookup");
      for (const cname of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 5, TTL: ttl, data: cname });
      }
    }
  } catch {
    status = 2; // SERVFAIL
    answer = [];
  }

  const payload = JSON.stringify({
    Status: status,
    TC: false,
    RD: true,
    RA: true,
    AD: false,
    CD: false,
    Question: [{ name, type: qtype }],
    Answer: answer
  });
  res.writeHead(200, {
    "content-type": "application/dns-json; charset=utf-8",
    "cache-control": "no-store",
    "content-length": Buffer.byteLength(payload)
  });
  res.end(payload);
}

export async function startProxyServer(overrides: Partial<ProxyConfig> = {}): Promise<RunningProxyServer> {
  const config: ProxyConfig = { ...loadConfigFromEnv(), ...overrides };
  const metrics = createProxyServerMetrics();

  const server = http.createServer((req, res) => {
    void (async () => {
      let url: URL;
      try {
        url = new URL(req.url ?? "/", "http://localhost");
      } catch {
        const body = JSON.stringify({ error: "invalid url" });
        res.writeHead(400, {
          "content-type": "application/json; charset=utf-8",
          "content-length": Buffer.byteLength(body)
        });
        res.end(body);
        return;
      }

      if (req.method === "GET" && url.pathname === "/healthz") {
        const body = JSON.stringify({ ok: true });
        res.writeHead(200, {
          "content-type": "application/json; charset=utf-8",
          "content-length": Buffer.byteLength(body)
        });
        res.end(body);
        return;
      }

        if (req.method === "GET" && url.pathname === "/metrics") {
          const body = metrics.prometheusText();
          res.writeHead(200, {
            "content-type": "text/plain; version=0.0.4; charset=utf-8",
            "content-length": Buffer.byteLength(body),
            "cache-control": "no-store"
          });
          res.end(body);
          return;
        }

        if (url.pathname === "/dns-query" || url.pathname === "/dns-json") {
          const allowMethods = url.pathname === "/dns-query" ? "GET, POST, OPTIONS" : "GET, OPTIONS";
          setDohCorsHeaders(req, res, config, { allowMethods });
          if (req.method === "OPTIONS") {
            res.writeHead(204);
            res.end();
            return;
          }
        }

        if (url.pathname === "/dns-query") {
          await handleDnsQuery(req, res, url, config);
          return;
        }

      if (req.method === "GET" && url.pathname === "/dns-json") {
        await handleDnsJson(req, res, url, config);
        return;
      }

      res.writeHead(404, { "content-type": "application/json; charset=utf-8" });
      res.end(JSON.stringify({ error: "not found" }));
    })().catch((err) => {
      log("error", "connect_error", { proto: "http", err: formatError(err) });
      if (!res.headersSent) {
        res.writeHead(500, { "content-type": "application/json; charset=utf-8" });
      }
      res.end(JSON.stringify({ error: "internal server error" }));
    });
  });

  const wss = new WebSocketServer({ noServer: true, maxPayload: config.wsMaxPayloadBytes });
  const wssMux = new WebSocketServer({
    noServer: true,
    maxPayload: config.wsMaxPayloadBytes,
    handleProtocols: (protocols) => (protocols.has(TCP_MUX_SUBPROTOCOL) ? TCP_MUX_SUBPROTOCOL : false)
  });
  let nextConnId = 1;

  server.on("upgrade", (req, socket, head) => {
    let url: URL;
    try {
      url = new URL(req.url ?? "/", "http://localhost");
    } catch {
      socket.destroy();
      return;
    }
    if (url.pathname === "/tcp-mux") {
      const protocolHeader = req.headers["sec-websocket-protocol"];
      const offered = Array.isArray(protocolHeader) ? protocolHeader.join(",") : protocolHeader ?? "";
      const protocols = offered
        .split(",")
        .map((p) => p.trim())
        .filter((p) => p.length > 0);
      if (!protocols.includes(TCP_MUX_SUBPROTOCOL)) {
        rejectWsUpgrade(socket, 400, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
        return;
      }

      wssMux.handleUpgrade(req, socket, head, (ws) => {
        wssMux.emit("connection", ws, req);
      });
      return;
    }

    if (url.pathname !== "/tcp" && url.pathname !== "/udp") {
      socket.destroy();
      return;
    }

    wss.handleUpgrade(req, socket, head, (ws) => {
      wss.emit("connection", ws, req);
    });
  });

  wss.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const url = new URL(req.url ?? "/", "http://localhost");
    const proto = url.pathname === "/udp" ? "udp" : "tcp";

    const clientAddress = req.socket.remoteAddress ?? null;

    // `/udp` can operate in one of two modes:
      // 1) Per-target (legacy): `/udp?host=...&port=...` (or `target=...`) where WS messages are raw datagrams.
      // 2) Multiplexed (new): `/udp` with no target params, using v1/v2 framing (see proxy/webrtc-udp-relay/PROTOCOL.md).
      const hasHost = url.searchParams.has("host");
      const hasPort = url.searchParams.has("port");
      const hasTarget = url.searchParams.has("target");
      if (proto === "udp" && !hasHost && !hasPort && !hasTarget) {
        log("info", "connect_requested", { connId, proto, mode: "multiplexed", clientAddress });
        log("info", "connect_accepted", { connId, proto, mode: "multiplexed", clientAddress });
        void handleUdpRelayMultiplexed(ws, connId, config, metrics);
        return;
      }

      const parsedTarget = parseTargetQuery(url);
      if ("error" in parsedTarget) {
        log("warn", "connect_denied", {
          connId,
          proto,
          clientAddress,
          reason: parsedTarget.error
        });
        metrics.incConnectionError("denied");
        wsCloseSafe(ws, 1008, parsedTarget.error);
        return;
      }

    const { host, port, portRaw } = parsedTarget;

    log("info", "connect_requested", { connId, proto, host, port: portRaw, clientAddress });

    void (async () => {
      try {
        const decision = await resolveAndAuthorizeTarget(host, port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });

        if (!decision.allowed) {
          log("warn", "connect_denied", { connId, proto, host, port, clientAddress, reason: decision.reason });
          metrics.incConnectionError("denied");
          wsCloseSafe(ws, 1008, decision.reason);
          return;
        }

        log("info", "connect_accepted", {
          connId,
          proto,
          host,
          port,
          clientAddress,
          resolvedAddress: decision.target.resolvedAddress,
          family: decision.target.family,
          decision: decision.target.decision
        });

        if (proto === "udp") {
          await handleUdpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config, metrics);
        } else {
          await handleTcpRelay(ws, connId, decision.target.resolvedAddress, decision.target.family, port, config, metrics);
        }
      } catch (err) {
        log("error", "connect_error", { connId, proto, host, port, clientAddress, err: formatError(err) });
        metrics.incConnectionError("error");
        wsCloseSafe(ws, 1011, "Proxy error");
      }
    })();
  });

  wssMux.on("connection", (ws, req) => {
    const connId = nextConnId++;
    const clientAddress = req.socket.remoteAddress ?? null;
    handleTcpMuxRelay(ws, connId, clientAddress, config, metrics);
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(config.listenPort, config.listenHost, () => resolve());
  });

  const addr = server.address();
  const listenAddress =
    typeof addr === "string" ? addr : `http://${addr?.address ?? config.listenHost}:${addr?.port ?? config.listenPort}`;

  log("info", "proxy_start", { listenAddress, open: config.open, allow: config.allow });

  return {
    server,
    config,
    listenAddress,
    close: async () => {
      log("info", "proxy_stop");
      await Promise.all([
        new Promise<void>((resolve) => wss.close(() => resolve())),
        new Promise<void>((resolve) => wssMux.close(() => resolve()))
      ]);
      await new Promise<void>((resolve, reject) => server.close((err) => (err ? reject(err) : resolve())));
    }
  };
}

type TcpMuxStreamState = {
  id: number;
  host: string;
  port: number;
  socket: net.Socket | null;
  connected: boolean;
  clientFin: boolean;
  clientFinSent: boolean;
  serverFin: boolean;
  pendingWrites: Buffer[];
  pendingWriteBytes: number;
  writePaused: boolean;
  connectTimer: NodeJS.Timeout | null;
};

function handleTcpMuxRelay(
  ws: WebSocket,
  connId: number,
  clientAddress: string | null,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): void {
  if (ws.protocol !== TCP_MUX_SUBPROTOCOL) {
    metrics.incConnectionError("denied");
    wsCloseSafe(ws, 1002, `Missing required subprotocol: ${TCP_MUX_SUBPROTOCOL}`);
    return;
  }

  metrics.connectionActiveInc("tcp_mux");

  const wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });

  const muxParser = new TcpMuxFrameParser(config.tcpMuxMaxFramePayloadBytes);
  const streams = new Map<number, TcpMuxStreamState>();
  const usedStreamIds = new Set<number>();

  let bytesIn = 0;
  let bytesOut = 0;
  let pausedForWsBackpressure = false;
  let closed = false;

  const pauseAllTcpReads = () => {
    if (pausedForWsBackpressure) return;
    pausedForWsBackpressure = true;
    for (const stream of streams.values()) {
      stream.socket?.pause();
    }
  };

  const resumeAllTcpReads = () => {
    if (!pausedForWsBackpressure) return;
    pausedForWsBackpressure = false;
    for (const stream of streams.values()) {
      stream.socket?.resume();
    }
  };

  const closeAll = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp_mux");

    for (const streamId of [...streams.keys()]) {
      destroyStream(streamId);
    }

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "tcp-mux",
      why,
      bytesIn,
      bytesOut,
      clientAddress,
      wsCode,
      wsReason
    });
  };

  const sendMuxFrame = (msgType: TcpMuxMsgType, streamId: number, payload?: Buffer) => {
    if (closed) return;
    if (ws.readyState !== ws.OPEN) return;
    const frame = encodeTcpMuxFrame(msgType, streamId, payload);
    const ok = wsStream.write(frame);
    if (!ok) {
      pauseAllTcpReads();
    }
  };

  const sendStreamError = (streamId: number, code: TcpMuxErrorCode, message: string) => {
    sendMuxFrame(TcpMuxMsgType.ERROR, streamId, encodeTcpMuxErrorPayload(code, message));
  };

  const enqueueStreamWrite = (stream: TcpMuxStreamState, chunk: Buffer) => {
    stream.pendingWrites.push(chunk);
    stream.pendingWriteBytes += chunk.length;
    if (stream.pendingWriteBytes > config.tcpMuxMaxStreamBufferedBytes) {
      sendStreamError(stream.id, TcpMuxErrorCode.STREAM_BUFFER_OVERFLOW, "stream buffered too much data");
      destroyStream(stream.id);
    }
  };

  const flushStreamWrites = (stream: TcpMuxStreamState) => {
    if (closed) return;
    if (!stream.socket) return;
    if (!stream.connected) return;
    if (stream.writePaused) return;

    while (stream.pendingWrites.length > 0) {
      const chunk = stream.pendingWrites.shift()!;
      stream.pendingWriteBytes -= chunk.length;
      const ok = stream.socket.write(chunk);
      if (!ok) {
        stream.writePaused = true;
        break;
      }
    }

    if (stream.clientFin && !stream.clientFinSent && stream.pendingWrites.length === 0) {
      stream.clientFinSent = true;
      stream.socket.end();
    }
  };

  const destroyStream = (streamId: number) => {
    const stream = streams.get(streamId);
    if (!stream) return;
    streams.delete(streamId);
    if (stream.connectTimer) {
      clearTimeout(stream.connectTimer);
    }
    if (stream.socket) {
      metrics.tcpMuxStreamsActiveDec();
      stream.socket.removeAllListeners();
      stream.socket.destroy();
    }
  };

  const handleOpen = (frame: TcpMuxFrame) => {
    if (frame.streamId === 0) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id=0 is reserved");
      return;
    }
    if (usedStreamIds.has(frame.streamId)) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream_id already used");
      return;
    }

    if (streams.size >= config.tcpMuxMaxStreams) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.STREAM_LIMIT_EXCEEDED, "max streams exceeded");
      return;
    }

    let target: { host: string; port: number };
    try {
      target = decodeTcpMuxOpenPayload(frame.payload);
    } catch (err) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    const host = stripOptionalIpv6Brackets(target.host);
    const port = target.port;

    if (host.trim() === "" || !Number.isInteger(port) || port < 1 || port > 65535) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "Invalid host or port");
      return;
    }

    usedStreamIds.add(frame.streamId);

    log("info", "connect_requested", {
      connId,
      proto: "tcp-mux",
      streamId: frame.streamId,
      host,
      port,
      clientAddress
    });

    const stream: TcpMuxStreamState = {
      id: frame.streamId,
      host,
      port,
      socket: null,
      connected: false,
      clientFin: false,
      clientFinSent: false,
      serverFin: false,
      pendingWrites: [],
      pendingWriteBytes: 0,
      writePaused: false,
      connectTimer: null
    };

    streams.set(stream.id, stream);

    void (async () => {
      let decision;
      try {
        decision = await resolveAndAuthorizeTarget(host, port, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });
      } catch (err) {
        metrics.incConnectionError("error");
        sendStreamError(stream.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(stream.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host,
          port,
          clientAddress,
          err: formatError(err)
        });
        return;
      }

      if (closed) return;
      const current = streams.get(stream.id);
      if (!current) return;

      if (!decision.allowed) {
        metrics.incConnectionError("denied");
        sendStreamError(stream.id, TcpMuxErrorCode.POLICY_DENIED, decision.reason);
        destroyStream(stream.id);
        log("warn", "connect_denied", {
          connId,
          proto: "tcp-mux",
          streamId: stream.id,
          host,
          port,
          clientAddress,
          reason: decision.reason
        });
        return;
      }

      log("info", "connect_accepted", {
        connId,
        proto: "tcp-mux",
        streamId: stream.id,
        host,
        port,
        clientAddress,
        resolvedAddress: decision.target.resolvedAddress,
        family: decision.target.family,
        decision: decision.target.decision
      });

      const tcpSocket = net.createConnection({
        host: decision.target.resolvedAddress,
        family: decision.target.family,
        port,
        allowHalfOpen: true
      });
      tcpSocket.setNoDelay(true);
      metrics.tcpMuxStreamsActiveInc();

      current.socket = tcpSocket;
      if (pausedForWsBackpressure) {
        tcpSocket.pause();
      }

      const connectTimer = setTimeout(() => {
        tcpSocket.destroy(new Error(`Connect timeout after ${config.connectTimeoutMs}ms`));
      }, config.connectTimeoutMs);
      connectTimer.unref();
      current.connectTimer = connectTimer;

      tcpSocket.once("connect", () => {
        clearTimeout(connectTimer);
        current.connectTimer = null;
        current.connected = true;
        flushStreamWrites(current);
      });

      tcpSocket.on("data", (chunk) => {
        bytesOut += chunk.length;
        metrics.addBytesOut("tcp_mux", chunk.length);
        sendMuxFrame(TcpMuxMsgType.DATA, current.id, chunk);
      });

      tcpSocket.on("drain", () => {
        current.writePaused = false;
        flushStreamWrites(current);
      });

      tcpSocket.on("end", () => {
        if (current.serverFin) return;
        current.serverFin = true;
        sendMuxFrame(TcpMuxMsgType.CLOSE, current.id, encodeTcpMuxClosePayload(TcpMuxCloseFlags.FIN));
      });

      tcpSocket.on("error", (err) => {
        metrics.incConnectionError("error");
        sendStreamError(current.id, TcpMuxErrorCode.DIAL_FAILED, (err as Error).message);
        destroyStream(current.id);
        log("error", "connect_error", {
          connId,
          proto: "tcp-mux",
          streamId: current.id,
          host,
          port,
          clientAddress,
          err: formatError(err)
        });
      });

      tcpSocket.on("close", () => {
        metrics.tcpMuxStreamsActiveDec();
        streams.delete(current.id);
        if (current.connectTimer) {
          clearTimeout(current.connectTimer);
          current.connectTimer = null;
        }
      });
    })();
  };

  const handleData = (frame: TcpMuxFrame) => {
    const stream = streams.get(frame.streamId);
    if (!stream) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.UNKNOWN_STREAM, "unknown stream");
      return;
    }
    if (stream.clientFin) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, "stream is half-closed (client FIN)");
      return;
    }

    bytesIn += frame.payload.length;
    metrics.addBytesIn("tcp_mux", frame.payload.length);

    if (!stream.socket || !stream.connected || stream.writePaused) {
      enqueueStreamWrite(stream, frame.payload);
      return;
    }

    const ok = stream.socket.write(frame.payload);
    if (!ok) {
      stream.writePaused = true;
    }
  };

  const handleClose = (frame: TcpMuxFrame) => {
    const stream = streams.get(frame.streamId);
    if (!stream) return;

    let flags: number;
    try {
      flags = decodeTcpMuxClosePayload(frame.payload).flags;
    } catch (err) {
      sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, (err as Error).message);
      return;
    }

    if ((flags & TcpMuxCloseFlags.RST) !== 0) {
      destroyStream(frame.streamId);
      return;
    }

    if ((flags & TcpMuxCloseFlags.FIN) !== 0) {
      stream.clientFin = true;
      flushStreamWrites(stream);
    }
  };

  const handleMuxFrame = (frame: TcpMuxFrame) => {
    switch (frame.msgType) {
      case TcpMuxMsgType.OPEN: {
        handleOpen(frame);
        return;
      }
      case TcpMuxMsgType.DATA: {
        handleData(frame);
        return;
      }
      case TcpMuxMsgType.CLOSE: {
        handleClose(frame);
        return;
      }
      case TcpMuxMsgType.ERROR: {
        // Not used by v1 clients; ignore.
        return;
      }
      case TcpMuxMsgType.PING: {
        sendMuxFrame(TcpMuxMsgType.PONG, frame.streamId, frame.payload);
        return;
      }
      case TcpMuxMsgType.PONG: {
        return;
      }
      default: {
        sendStreamError(frame.streamId, TcpMuxErrorCode.PROTOCOL_ERROR, `Unknown msg_type ${frame.msgType}`);
      }
    }
  };

  // Drain the `createWebSocketStream` readable side so it doesn't pause the underlying WebSocket.
  // We handle incoming messages via `ws.on("message")` so we can reliably detect text vs binary.
  wsStream.on("data", () => {
    // ignore
  });

  ws.on("message", (data, isBinary) => {
    if (closed) return;
    if (!isBinary) {
      closeAll("ws_text", 1003, "WebSocket text messages are not supported");
      return;
    }

    const buf = Buffer.isBuffer(data)
      ? data
      : Array.isArray(data)
        ? Buffer.concat(data)
        : Buffer.from(data as ArrayBuffer);

    let frames: TcpMuxFrame[];
    try {
      frames = muxParser.push(buf);
    } catch {
      closeAll("protocol_error", 1002, "Protocol error");
      return;
    }

    for (const frame of frames) {
      handleMuxFrame(frame);
    }

    // Avoid unbounded buffering if the peer sends an incomplete frame or never finishes a
    // max-sized payload. The only legitimate "pending" state is a single partial frame.
    if (muxParser.pendingBytes() > TCP_MUX_HEADER_BYTES + config.tcpMuxMaxFramePayloadBytes) {
      closeAll("protocol_error", 1002, "Protocol error");
    }
  });

  wsStream.on("drain", () => {
    if (closed) return;
    resumeAllTcpReads();
  });

  wsStream.on("error", (err) => {
    closeAll("ws_stream_error", 1011, "WebSocket stream error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });

  ws.once("close", (code, reason) => {
    wsStream.destroy();
    closeAll("ws_close", code, reason.toString());
  });

  ws.once("error", (err) => {
    closeAll("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp-mux", clientAddress, err: formatError(err) });
  });
}

async function handleTcpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  if (ws.readyState !== ws.OPEN) return;

  const wsStream = createWebSocketStream(ws, { highWaterMark: config.wsStreamHighWaterMarkBytes });

  metrics.connectionActiveInc("tcp");

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;

  const tcpSocket = net.createConnection({ host: address, port, family });
  tcpSocket.setNoDelay(true);

  const connectTimer = setTimeout(() => {
    tcpSocket.destroy(new Error(`Connect timeout after ${config.connectTimeoutMs}ms`));
  }, config.connectTimeoutMs);
  connectTimer.unref();

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp");

    clearTimeout(connectTimer);

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    tcpSocket.destroy();
    wsStream.destroy();

    log("info", "conn_close", {
      connId,
      proto: "tcp",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("tcp");
    clearTimeout(connectTimer);
    tcpSocket.destroy();
    wsStream.destroy();

    log("info", "conn_close", {
      connId,
      proto: "tcp",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  tcpSocket.once("connect", () => {
    clearTimeout(connectTimer);
  });

  tcpSocket.once("error", (err) => {
    closeBoth("tcp_error", 1011, "TCP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "tcp", err: formatError(err) });
  });

  tcpSocket.once("close", () => {
    if (!closed) {
      closeBoth("tcp_close", 1000, "TCP closed");
    }
  });

  const fromWs = new PassThrough();
  const fromTcp = new PassThrough();

  fromWs.on("data", (chunk) => {
    bytesIn += chunk.length;
    metrics.addBytesIn("tcp", chunk.length);
  });
  fromTcp.on("data", (chunk) => {
    bytesOut += chunk.length;
    metrics.addBytesOut("tcp", chunk.length);
  });

  wsStream.pipe(fromWs).pipe(tcpSocket);
  tcpSocket.pipe(fromTcp).pipe(wsStream);
}

async function handleUdpRelay(
  ws: WebSocket,
  connId: number,
  address: string,
  family: 4 | 6,
  port: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  if (ws.readyState !== ws.OPEN) return;

  const socket = dgram.createSocket(family === 6 ? "udp6" : "udp4");
  socket.connect(port, address);

  metrics.connectionActiveInc("udp");

  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;

  const closeBoth = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");
    try {
      socket.close();
    } catch {
      // ignore
    }

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");
    try {
      socket.close();
    } catch {
      // ignore
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeBoth("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("error", (err) => {
    closeBoth("udp_error", 1011, "UDP error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", err: formatError(err) });
  });

  socket.on("message", (msg) => {
    bytesOut += msg.length;
    metrics.addBytesOut("udp", msg.length);
    if (ws.readyState !== ws.OPEN) return;

    if (ws.bufferedAmount > config.udpWsBufferedAmountLimitBytes) {
      log("warn", "udp_drop_backpressure", {
        connId,
        bufferedAmount: ws.bufferedAmount,
        limit: config.udpWsBufferedAmountLimitBytes,
        droppedBytes: msg.length
      });
      return;
    }

    ws.send(msg);
  });

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);
    bytesIn += buf.length;
    metrics.addBytesIn("udp", buf.length);
    socket.send(buf);
  });
}

function stripIpv6ZoneIndex(address: string): string {
  const idx = address.indexOf("%");
  if (idx === -1) return address;
  return address.slice(0, idx);
}

type UdpRelayBindingKey = `${4 | 6}:${number}`;

interface UdpRelayBinding {
  key: UdpRelayBindingKey;
  guestPort: number;
  addressFamily: 4 | 6;
  socket: dgram.Socket;
  lastActiveMs: number;
  allowedRemotes: Map<string, number>;
  lastAllowedPruneMs: number;
}

function makeUdpRelayBindingKey(guestPort: number, addressFamily: 4 | 6): UdpRelayBindingKey {
  return `${addressFamily}:${guestPort}`;
}

async function handleUdpRelayMultiplexed(
  ws: WebSocket,
  connId: number,
  config: ProxyConfig,
  metrics: ProxyServerMetrics
): Promise<void> {
  const bindings = new Map<UdpRelayBindingKey, UdpRelayBinding>();
  let bytesIn = 0;
  let bytesOut = 0;
  let closed = false;
  let gcTimer: NodeJS.Timeout | null = null;
  let clientSupportsV2 = false;

  metrics.connectionActiveInc("udp");

  const remoteAllowlistEnabled = config.udpRelayInboundFilterMode === "address_and_port";
  const remoteAllowlistIdleTimeoutMs = config.udpRelayBindingIdleTimeoutMs;
  const maxAllowedRemotesBeforePrune = 1024;

  const remoteKey = (ipBytes: Uint8Array, port: number): string => `${Buffer.from(ipBytes).toString("hex")}:${port}`;

  const pruneAllowedRemotes = (binding: UdpRelayBinding, now: number) => {
    if (!remoteAllowlistEnabled) return;
    if (remoteAllowlistIdleTimeoutMs > 0) {
      if (
        binding.allowedRemotes.size <= maxAllowedRemotesBeforePrune &&
        binding.lastAllowedPruneMs !== 0 &&
        now - binding.lastAllowedPruneMs <= remoteAllowlistIdleTimeoutMs
      ) {
        return;
      }

      const cutoff = now - remoteAllowlistIdleTimeoutMs;
      for (const [key, ts] of binding.allowedRemotes) {
        if (ts < cutoff) {
          binding.allowedRemotes.delete(key);
        }
      }
      binding.lastAllowedPruneMs = now;
      return;
    }

    // No idle timeout: still cap memory growth.
    if (binding.allowedRemotes.size > maxAllowedRemotesBeforePrune) {
      binding.allowedRemotes.clear();
      binding.lastAllowedPruneMs = now;
    }
  };

  const allowRemote = (binding: UdpRelayBinding, ipBytes: Uint8Array, port: number, now: number) => {
    if (!remoteAllowlistEnabled) return;
    pruneAllowedRemotes(binding, now);
    binding.allowedRemotes.set(remoteKey(ipBytes, port), now);
  };

  const remoteAllowed = (binding: UdpRelayBinding, ipBytes: Uint8Array, port: number, now: number): boolean => {
    if (!remoteAllowlistEnabled) return true;
    const key = remoteKey(ipBytes, port);
    const last = binding.allowedRemotes.get(key);
    if (last === undefined) return false;

    if (remoteAllowlistIdleTimeoutMs > 0 && now - last > remoteAllowlistIdleTimeoutMs) {
      binding.allowedRemotes.delete(key);
      return false;
    }

    // Refresh timestamp to keep active flows alive.
    binding.allowedRemotes.set(key, now);
    return true;
  };

  const closeAll = (why: string, wsCode: number, wsReason: string) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");

    if (gcTimer) {
      clearInterval(gcTimer);
      gcTimer = null;
    }

    const bindingCount = bindings.size;
    if (bindingCount > 0) {
      metrics.udpBindingsActiveDec(bindingCount);
    }
    for (const binding of bindings.values()) {
      try {
        binding.socket.close();
      } catch {
        // ignore
      }
    }
    bindings.clear();

    if (ws.readyState === ws.OPEN) {
      wsCloseSafe(ws, wsCode, wsReason);
    }

    log("info", "conn_close", {
      connId,
      proto: "udp",
      mode: "multiplexed",
      why,
      bytesIn,
      bytesOut,
      wsCode,
      wsReason
    });
  };

  ws.once("close", (code, reason) => {
    if (closed) return;
    closed = true;
    metrics.connectionActiveDec("udp");

    if (gcTimer) {
      clearInterval(gcTimer);
      gcTimer = null;
    }

    const bindingCount = bindings.size;
    if (bindingCount > 0) {
      metrics.udpBindingsActiveDec(bindingCount);
    }
    for (const binding of bindings.values()) {
      try {
        binding.socket.close();
      } catch {
        // ignore
      }
    }
    bindings.clear();

    log("info", "conn_close", {
      connId,
      proto: "udp",
      mode: "multiplexed",
      why: "ws_close",
      bytesIn,
      bytesOut,
      wsCode: code,
      wsReason: reason.toString()
    });
  });

  ws.once("error", (err) => {
    closeAll("ws_error", 1011, "WebSocket error");
    metrics.incConnectionError("error");
    log("error", "connect_error", { connId, proto: "udp", mode: "multiplexed", err: formatError(err) });
  });

  if (config.udpRelayBindingIdleTimeoutMs > 0) {
    const gcIntervalMs = Math.max(1_000, Math.min(10_000, Math.floor(config.udpRelayBindingIdleTimeoutMs / 2)));
    gcTimer = setInterval(() => {
      if (closed) return;
      const now = Date.now();
      for (const [key, binding] of bindings) {
        if (now - binding.lastActiveMs <= config.udpRelayBindingIdleTimeoutMs) continue;
        bindings.delete(key);
        metrics.udpBindingsActiveDec();
        try {
          binding.socket.close();
        } catch {
          // ignore
        }
      }
    }, gcIntervalMs);
    gcTimer.unref();
  }

  const getOrCreateBinding = (guestPort: number, addressFamily: 4 | 6): UdpRelayBinding | null => {
    if (closed) return null;
    const key = makeUdpRelayBindingKey(guestPort, addressFamily);
    const existing = bindings.get(key);
    if (existing) return existing;

    if (bindings.size >= config.udpRelayMaxBindingsPerConnection) {
      closeAll("udp_max_bindings", 1008, "Too many UDP bindings");
      return null;
    }

    const socket = dgram.createSocket(addressFamily === 6 ? "udp6" : "udp4");
    const binding: UdpRelayBinding = {
      key,
      guestPort,
      addressFamily,
      socket,
      lastActiveMs: Date.now(),
      allowedRemotes: new Map(),
      lastAllowedPruneMs: 0
    };

    socket.on("error", (err) => {
      metrics.incConnectionError("error");
      log("error", "connect_error", { connId, proto: "udp", mode: "multiplexed", err: formatError(err), guestPort, addressFamily });
      const removed = bindings.delete(key);
      if (removed) {
        metrics.udpBindingsActiveDec();
      }
      try {
        socket.close();
      } catch {
        // ignore
      }
    });

    socket.on("message", (msg, rinfo) => {
      const now = Date.now();
      binding.lastActiveMs = now;

      if (msg.length > config.udpRelayMaxPayloadBytes) return;
      if (ws.readyState !== ws.OPEN) return;

      let frame: Uint8Array;
      try {
        const addr = stripIpv6ZoneIndex(rinfo.address);
        const parsed = ipaddr.parse(addr);
        const ipBytes = new Uint8Array(parsed.toByteArray());

        if (addressFamily === 4) {
          if (ipBytes.length !== 4) return;
          if (!remoteAllowed(binding, ipBytes, rinfo.port, now)) return;
          if (config.udpRelayPreferV2 && clientSupportsV2) {
            frame = encodeUdpRelayV2Datagram(
              {
                guestPort,
                remoteIp: ipBytes,
                remotePort: rinfo.port,
                payload: msg
              },
              { maxPayload: config.udpRelayMaxPayloadBytes }
            );
          } else {
            frame = encodeUdpRelayV1Datagram(
              {
                guestPort,
                remoteIpv4: [ipBytes[0]!, ipBytes[1]!, ipBytes[2]!, ipBytes[3]!],
                remotePort: rinfo.port,
                payload: msg
              },
              { maxPayload: config.udpRelayMaxPayloadBytes }
            );
          }
        } else {
          if (ipBytes.length !== 16) return;
          if (!remoteAllowed(binding, ipBytes, rinfo.port, now)) return;
          frame = encodeUdpRelayV2Datagram(
            {
              guestPort,
              remoteIp: ipBytes,
              remotePort: rinfo.port,
              payload: msg
            },
            { maxPayload: config.udpRelayMaxPayloadBytes }
          );
        }
      } catch {
        return;
      }

      bytesOut += msg.length;
      metrics.addBytesOut("udp", msg.length);

      if (ws.bufferedAmount > config.udpWsBufferedAmountLimitBytes) {
        log("warn", "udp_drop_backpressure", {
          connId,
          bufferedAmount: ws.bufferedAmount,
          limit: config.udpWsBufferedAmountLimitBytes,
          droppedBytes: frame.length
        });
        return;
      }

      ws.send(frame);
    });

    bindings.set(key, binding);
    metrics.udpBindingsActiveInc();
    return binding;
  };

  ws.on("message", (data, isBinary) => {
    if (!isBinary) return;
    const buf = Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer);

    void (async () => {
      if (closed) return;
      let guestPort: number;
      let remotePort: number;
      let addressFamily: 4 | 6;
      let remoteIpBytes: Uint8Array;
      let payload: Uint8Array;

      try {
        const decoded = decodeUdpRelayFrame(buf, { maxPayload: config.udpRelayMaxPayloadBytes });
        if (decoded.version === 1) {
          guestPort = decoded.guestPort;
          remotePort = decoded.remotePort;
          addressFamily = 4;
          remoteIpBytes = Uint8Array.from(decoded.remoteIpv4);
          payload = decoded.payload;
        } else {
          clientSupportsV2 = true;
          guestPort = decoded.guestPort;
          remotePort = decoded.remotePort;
          addressFamily = decoded.addressFamily;
          remoteIpBytes = decoded.remoteIp;
          payload = decoded.payload;
        }
      } catch {
        return;
      }

      if (closed) return;
      if (remotePort < 1 || remotePort > 65535) return;
      if (payload.length > config.udpRelayMaxPayloadBytes) return;

      let remoteAddress: string;
      try {
        remoteAddress = ipaddr.fromByteArray(Array.from(remoteIpBytes)).toString();
      } catch {
        return;
      }

      let decision;
      try {
        decision = await resolveAndAuthorizeTarget(remoteAddress, remotePort, {
          open: config.open,
          allowlist: config.allow,
          dnsTimeoutMs: config.dnsTimeoutMs
        });
      } catch (err) {
        metrics.incConnectionError("error");
        log("error", "connect_error", {
          connId,
          proto: "udp",
          mode: "multiplexed",
          err: formatError(err),
          remoteAddress,
          remotePort,
          guestPort
        });
        closeAll("policy_error", 1011, "Proxy error");
        return;
      }
      if (closed) return;
      if (!decision.allowed) {
        metrics.incConnectionError("denied");
        return;
      }

      if (addressFamily === 4 && decision.target.family !== 4) return;
      if (addressFamily === 6 && decision.target.family !== 6) return;

      const binding = getOrCreateBinding(guestPort, addressFamily);
      if (!binding) return;
      const now = Date.now();
      binding.lastActiveMs = now;
      allowRemote(binding, remoteIpBytes, remotePort, now);

      bytesIn += payload.length;
      metrics.addBytesIn("udp", payload.length);

      // Send the raw UDP payload to the decoded destination.
      try {
        binding.socket.send(payload, remotePort, remoteAddress);
      } catch {
        // ignore
      }
    })();
  });
}
