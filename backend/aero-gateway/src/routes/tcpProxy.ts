import { createHash } from "node:crypto";
import { lookup } from "node:dns/promises";
import net from "node:net";
import type http from "node:http";
import type { Duplex } from "node:stream";

import type { TcpTarget } from "../protocol/tcpTarget.js";
import { TcpTargetParseError, parseTcpTargetFromUrl } from "../protocol/tcpTarget.js";
import { validateTcpTargetPolicy, validateWsUpgradePolicy, type TcpProxyUpgradePolicy } from "./tcpPolicy.js";
import {
  evaluateTcpHostPolicy,
  parseTcpHostnameEgressPolicyFromEnv,
  type TcpHostPolicyDecision,
} from "../security/egressPolicy.js";
import { isPublicIpAddress } from "../security/ipPolicy.js";
import type { SessionConnectionTracker } from "../session.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

export const tcpProxyMetrics = {
  blockedByHostPolicy: 0,
  blockedByIpPolicy: 0,
};

type TcpProxyEgressMetricSink = Readonly<{
  blockedByHostPolicyTotal?: { inc: () => void };
  blockedByIpPolicyTotal?: { inc: () => void };
}>;

export class TcpProxyTargetError extends Error {
  readonly kind: "host-policy" | "ip-policy" | "dns";
  readonly statusCode: number;

  constructor(kind: "host-policy" | "ip-policy" | "dns", message: string, statusCode: number) {
    super(message);
    this.kind = kind;
    this.statusCode = statusCode;
  }
}

export function handleTcpProxyUpgrade(
  req: http.IncomingMessage,
  socket: Duplex,
  head: Buffer,
  opts: TcpProxyUpgradePolicy & {
    /**
     * Expected request pathname for this upgrade. Defaults to `/tcp`.
     *
     * The gateway may be deployed under a base-path prefix (e.g. `/aero/tcp`).
     * In those cases the HTTP server can route upgrades by pathname and then
     * pass that pathname here for an additional defense-in-depth check.
     */
    expectedPathname?: string;
    allowPrivateIps?: boolean;
    createConnection?: typeof net.createConnection;
    metrics?: TcpProxyEgressMetricSink;
    sessionId?: string;
    sessionConnections?: SessionConnectionTracker;
    maxMessageBytes?: number;
    connectTimeoutMs?: number;
    idleTimeoutMs?: number;
  } = {},
): void {
  const upgradeDecision = validateWsUpgradePolicy(req, opts);
  if (!upgradeDecision.ok) {
    respondHttp(socket, upgradeDecision.status, upgradeDecision.message);
    return;
  }

  let target: TcpTarget;
  try {
    const url = new URL(req.url ?? "", "http://localhost");
    const expectedPathname = opts.expectedPathname ?? "/tcp";
    if (url.pathname !== expectedPathname) {
      respondHttp(socket, 404, "Not Found");
      return;
    }
    target = parseTcpTargetFromUrl(url);
  } catch (err) {
    respondHttp(socket, 400, formatUpgradeError(err));
    return;
  }

  const targetDecision = validateTcpTargetPolicy(target.host, target.port, opts);
  if (!targetDecision.ok) {
    respondHttp(socket, targetDecision.status, targetDecision.message);
    return;
  }

  const keyHeader = req.headers["sec-websocket-key"];
  if (typeof keyHeader !== "string" || keyHeader === "") {
    respondHttp(socket, 400, "Missing required header: Sec-WebSocket-Key");
    return;
  }
  const key = keyHeader;

  void (async () => {
    let resolved: { ip: string; port: number; hostname?: string };
    try {
      resolved = await resolveTcpProxyTarget(target.host, target.port, {
        allowPrivateIps: opts.allowPrivateIps,
        metrics: opts.metrics,
      });
    } catch (err) {
      if (err instanceof TcpProxyTargetError) {
        respondHttp(socket, err.statusCode, err.message);
        return;
      }
      respondHttp(socket, 502, formatUpgradeError(err));
      return;
    }

    if (opts.sessionId && opts.sessionConnections) {
      if (!opts.sessionConnections.tryAcquire(opts.sessionId)) {
        respondHttp(socket, 429, "Too many concurrent connections");
        return;
      }

      let released = false;
      const release = () => {
        if (released) return;
        released = true;
        opts.sessionConnections!.release(opts.sessionId!);
      };
      socket.once("close", release);
      socket.once("error", release);
    }

    const accept = createHash("sha1").update(key + WS_GUID).digest("base64");
    socket.write(
      [
        "HTTP/1.1 101 Switching Protocols",
        "Upgrade: websocket",
        "Connection: Upgrade",
        `Sec-WebSocket-Accept: ${accept}`,
        "\r\n",
      ].join("\r\n"),
    );

    if ("setNoDelay" in socket && typeof socket.setNoDelay === "function") {
      socket.setNoDelay(true);
    }

    const connectTimeoutMs = opts.connectTimeoutMs ?? 10_000;
    const idleTimeoutMs = opts.idleTimeoutMs ?? 300_000;
    const createConnection = opts.createConnection ?? net.createConnection;
    const tcpSocket = createConnection({ host: resolved.ip, port: resolved.port });
    tcpSocket.setNoDelay(true);
    tcpSocket.setTimeout(idleTimeoutMs);
    tcpSocket.on("timeout", () => tcpSocket.destroy(new Error("TCP idle timeout")));

    const connectTimer = setTimeout(() => {
      tcpSocket.destroy(new Error("TCP connect timeout"));
    }, connectTimeoutMs);
    connectTimer.unref?.();
    tcpSocket.once("connect", () => clearTimeout(connectTimer));
    tcpSocket.once("error", () => clearTimeout(connectTimer));
    tcpSocket.once("close", () => clearTimeout(connectTimer));

    const bridge = new WebSocketTcpBridge(socket, tcpSocket, opts.maxMessageBytes ?? 1024 * 1024);
    bridge.start(head);
  })();
}

function respondHttp(socket: Duplex, status: number, message: string): void {
  const body = `${message}\n`;
  socket.end(
    [
      `HTTP/1.1 ${status} ${httpStatusText(status)}`,
      "Content-Type: text/plain; charset=utf-8",
      `Content-Length: ${Buffer.byteLength(body)}`,
      "Connection: close",
      "\r\n",
      body,
    ].join("\r\n"),
  );
}

function httpStatusText(status: number): string {
  switch (status) {
    case 400:
      return "Bad Request";
    case 401:
      return "Unauthorized";
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
    case 429:
      return "Too Many Requests";
    case 502:
      return "Bad Gateway";
    default:
      return "Error";
  }
}

function formatUpgradeError(err: unknown): string {
  if (err instanceof TcpTargetParseError) {
    return err.message;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return "Invalid request";
}

type ParsedFrame = {
  opcode: number;
  fin: boolean;
  payload: Buffer;
};

class WebSocketTcpBridge {
  private readonly wsSocket: Duplex;
  private readonly tcpSocket: net.Socket;
  private readonly maxMessageBytes: number;

  private wsBuffer: Buffer = Buffer.alloc(0);

  private fragmentedOpcode: number | null = null;
  private fragmentedChunks: Buffer[] = [];
  private fragmentedBytes = 0;

  private closed = false;

  constructor(wsSocket: Duplex, tcpSocket: net.Socket, maxMessageBytes: number) {
    this.wsSocket = wsSocket;
    this.tcpSocket = tcpSocket;
    this.maxMessageBytes = maxMessageBytes;
  }

  start(head: Buffer): void {
    if (head.length > 0) {
      this.wsBuffer = this.wsBuffer.length === 0 ? head : Buffer.concat([this.wsBuffer, head]);
    }

    this.wsSocket.on("data", (data) => {
      this.wsBuffer = this.wsBuffer.length === 0 ? data : Buffer.concat([this.wsBuffer, data]);
      this.drainWebSocketFrames();
    });
    this.wsSocket.on("error", () => this.close());
    this.wsSocket.on("close", () => this.close());
    this.wsSocket.on("end", () => this.close());

    this.tcpSocket.on("data", (data) => {
      this.sendFrame(0x2, data);
    });
    this.tcpSocket.on("error", () => this.close());
    this.tcpSocket.on("close", () => this.close());
    this.tcpSocket.on("end", () => this.close());

    this.drainWebSocketFrames();
  }

  private drainWebSocketFrames(): void {
    while (!this.closed) {
      const parsed = tryReadFrame(this.wsBuffer, this.maxMessageBytes);
      if (!parsed) return;
      this.wsBuffer = parsed.remaining;
      this.handleFrame(parsed.frame);
    }
  }

  private handleFrame(frame: ParsedFrame): void {
    switch (frame.opcode) {
      case 0x0: {
        // Continuation
        if (this.fragmentedOpcode === null) {
          this.closeWithProtocolError();
          return;
        }
        this.fragmentedChunks.push(frame.payload);
        this.fragmentedBytes += frame.payload.length;
        if (this.fragmentedBytes > this.maxMessageBytes) {
          this.closeWithMessageTooLarge();
          return;
        }
        if (frame.fin) {
          const payload = Buffer.concat(this.fragmentedChunks, this.fragmentedBytes);
          const opcode = this.fragmentedOpcode;
          this.fragmentedOpcode = null;
          this.fragmentedChunks = [];
          this.fragmentedBytes = 0;
          this.forwardPayload(opcode, payload);
        }
        return;
      }
      case 0x1:
      case 0x2: {
        // Text / Binary
        if (this.fragmentedOpcode !== null) {
          this.closeWithProtocolError();
          return;
        }
        if (frame.fin) {
          this.forwardPayload(frame.opcode, frame.payload);
          return;
        }
        this.fragmentedOpcode = frame.opcode;
        this.fragmentedChunks = [frame.payload];
        this.fragmentedBytes = frame.payload.length;
        if (this.fragmentedBytes > this.maxMessageBytes) {
          this.closeWithMessageTooLarge();
          return;
        }
        return;
      }
      case 0x8: {
        // Close
        this.sendFrame(0x8, frame.payload);
        this.close();
        return;
      }
      case 0x9: {
        // Ping
        this.sendFrame(0xA, frame.payload);
        return;
      }
      case 0xA: {
        // Pong
        return;
      }
      default: {
        this.closeWithProtocolError();
      }
    }
  }

  private forwardPayload(opcode: number, payload: Buffer): void {
    // v1: raw TCP bytes forwarded via binary frames.
    if (opcode === 0x1) {
      // Text frames are permitted by WebSocket, but Aero's TCP tunnel is binary.
      // Still forward the raw UTF-8 bytes to avoid surprising behaviour.
      this.tcpSocket.write(payload);
      return;
    }
    if (opcode === 0x2) {
      this.tcpSocket.write(payload);
      return;
    }
    this.closeWithProtocolError();
  }

  private sendFrame(opcode: number, payload: Buffer): void {
    if (this.closed) return;
    const frame = encodeFrame(opcode, payload);
    this.wsSocket.write(frame);
  }

  private closeWithProtocolError(): void {
    // 1002 = protocol error.
    this.sendFrame(0x8, Buffer.from([0x03, 0xea]));
    this.close();
  }

  private closeWithMessageTooLarge(): void {
    // 1009 = message too big.
    this.sendFrame(0x8, Buffer.from([0x03, 0xf1]));
    this.close();
  }

  private close(): void {
    if (this.closed) return;
    this.closed = true;

    this.wsSocket.destroy();
    this.tcpSocket.destroy();
  }
}

type TryReadFrameResult = { frame: ParsedFrame; remaining: Buffer };

function tryReadFrame(buffer: Buffer, maxPayloadBytes: number): TryReadFrameResult | null {
  if (buffer.length < 2) return null;

  const first = buffer[0];
  const second = buffer[1];

  const fin = (first & 0x80) !== 0;
  const opcode = first & 0x0f;

  const masked = (second & 0x80) !== 0;
  let length = second & 0x7f;
  let offset = 2;

  if (length === 126) {
    if (buffer.length < offset + 2) return null;
    length = buffer.readUInt16BE(offset);
    offset += 2;
  } else if (length === 127) {
    if (buffer.length < offset + 8) return null;
    const hi = buffer.readUInt32BE(offset);
    const lo = buffer.readUInt32BE(offset + 4);
    offset += 8;
    const combined = hi * 2 ** 32 + lo;
    if (!Number.isSafeInteger(combined)) {
      // Too large for a JS buffer anyway; treat as protocol error.
      return { frame: { fin: true, opcode: 0x8, payload: Buffer.alloc(0) }, remaining: Buffer.alloc(0) };
    }
    length = combined;
  }

  if (length > maxPayloadBytes) {
    // Close immediately without buffering untrusted payloads.
    return { frame: { fin: true, opcode: 0x8, payload: Buffer.from([0x03, 0xf1]) }, remaining: Buffer.alloc(0) };
  }

  let maskKey: Buffer | null = null;
  if (masked) {
    if (buffer.length < offset + 4) return null;
    maskKey = buffer.subarray(offset, offset + 4);
    offset += 4;
  }

  if (buffer.length < offset + length) return null;
  let payload = buffer.subarray(offset, offset + length);
  const remaining = buffer.subarray(offset + length);

  if (masked) {
    payload = unmask(payload, maskKey!);
  }

  // If we consumed the entire buffer, avoid keeping a reference to the backing allocation
  // via an empty subarray view.
  const remainingTrimmed = remaining.length === 0 ? Buffer.alloc(0) : remaining;
  return { frame: { fin, opcode, payload }, remaining: remainingTrimmed };
}

function unmask(payload: Buffer, key: Buffer): Buffer {
  const out = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; i++) {
    out[i] = payload[i] ^ key[i % 4];
  }
  return out;
}

function encodeFrame(opcode: number, payload: Buffer): Buffer {
  const finOpcode = 0x80 | (opcode & 0x0f);
  const length = payload.length;

  if (length < 126) {
    const out = Buffer.allocUnsafe(2 + length);
    out[0] = finOpcode;
    out[1] = length;
    payload.copy(out, 2);
    return out;
  }

  if (length < 65536) {
    const out = Buffer.allocUnsafe(4 + length);
    out[0] = finOpcode;
    out[1] = 126;
    out.writeUInt16BE(length, 2);
    payload.copy(out, 4);
    return out;
  }

  const out = Buffer.allocUnsafe(10 + length);
  out[0] = finOpcode;
  out[1] = 127;
  out.writeUInt32BE(0, 2);
  out.writeUInt32BE(length, 6);
  payload.copy(out, 10);
  return out;
}

export async function resolveTcpProxyTarget(
  rawHost: string,
  port: number,
  opts: Readonly<{
    allowPrivateIps?: boolean;
    env?: Record<string, string | undefined>;
    metrics?: TcpProxyEgressMetricSink;
  }> = {},
): Promise<{ ip: string; port: number; hostname?: string }> {
  const env = opts.env ?? process.env;
  // By default we block private/reserved IPs to prevent SSRF / local-network
  // probing via the browser-facing TCP proxy.
  //
  // For local development + E2E testing we allow opting out so the proxy can
  // reach loopback test servers (e.g. Playwright).
  const allowPrivateIps = opts.allowPrivateIps ?? env.TCP_ALLOW_PRIVATE_IPS === "1";

  const hostPolicy = parseTcpHostnameEgressPolicyFromEnv(env);
  const decision = evaluateTcpHostPolicy(rawHost, hostPolicy);
  if (!decision.allowed) {
    tcpProxyMetrics.blockedByHostPolicy++;
    opts.metrics?.blockedByHostPolicyTotal?.inc();
    const statusCode = decision.reason === "invalid-hostname" ? 400 : 403;
    throw new TcpProxyTargetError("host-policy", formatHostPolicyRejection(decision), statusCode);
  }

  if (decision.target.kind === "ip") {
    if (!allowPrivateIps && !isPublicIpAddress(decision.target.ip)) {
      tcpProxyMetrics.blockedByIpPolicy++;
      opts.metrics?.blockedByIpPolicyTotal?.inc();
      throw new TcpProxyTargetError("ip-policy", "Target IP is not allowed by IP egress policy", 403);
    }
    return { ip: decision.target.ip, port };
  }

  // Host policy is enforced before DNS. After that, still enforce IP egress
  // policy on the resolved targets, selecting the first allowed public IP.
  let addresses: { address: string }[];
  try {
    addresses = await lookup(decision.target.hostname, { all: true, verbatim: true });
  } catch (err) {
    throw new TcpProxyTargetError("dns", `DNS lookup failed for ${decision.target.hostname}`, 502);
  }

  for (const { address } of addresses) {
    if (allowPrivateIps || isPublicIpAddress(address)) {
      return { ip: address, port, hostname: decision.target.hostname };
    }
  }

  tcpProxyMetrics.blockedByIpPolicy++;
  opts.metrics?.blockedByIpPolicyTotal?.inc();
  throw new TcpProxyTargetError("ip-policy", "All resolved IPs are blocked by IP egress policy", 403);
}

function formatHostPolicyRejection(decision: Extract<TcpHostPolicyDecision, { allowed: false }>): string {
  return `${decision.reason}: ${decision.message}`;
}
