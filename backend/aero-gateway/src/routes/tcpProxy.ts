import { createHash } from "node:crypto";
import net from "node:net";
import type http from "node:http";
import type { Duplex } from "node:stream";

import type { TcpTarget } from "../protocol/tcpTarget.js";
import { TcpTargetParseError, parseTcpTargetFromUrl } from "../protocol/tcpTarget.js";
import { validateTcpTargetPolicy, validateWsUpgradePolicy, type TcpProxyUpgradePolicy } from "./tcpPolicy.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

export function handleTcpProxyUpgrade(
  req: http.IncomingMessage,
  socket: Duplex,
  head: Buffer,
  opts: TcpProxyUpgradePolicy = {},
): void {
  const upgradeDecision = validateWsUpgradePolicy(req, opts);
  if (!upgradeDecision.ok) {
    respondHttp(socket, upgradeDecision.status, upgradeDecision.message);
    return;
  }

  let target: TcpTarget;
  try {
    const url = new URL(req.url ?? "", "http://localhost");
    if (url.pathname !== "/tcp") {
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

  const key = req.headers["sec-websocket-key"];
  if (typeof key !== "string" || key === "") {
    respondHttp(socket, 400, "Missing required header: Sec-WebSocket-Key");
    return;
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

  const tcpSocket = net.createConnection({ host: target.host, port: target.port });
  tcpSocket.setNoDelay(true);

  const bridge = new WebSocketTcpBridge(socket, tcpSocket);
  bridge.start(head);
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
    case 403:
      return "Forbidden";
    case 404:
      return "Not Found";
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

  private wsBuffer: Buffer = Buffer.alloc(0);

  private fragmentedOpcode: number | null = null;
  private fragmentedChunks: Buffer[] = [];

  private closed = false;

  constructor(wsSocket: Duplex, tcpSocket: net.Socket) {
    this.wsSocket = wsSocket;
    this.tcpSocket = tcpSocket;
  }

  start(head: Buffer): void {
    if (head.length > 0) {
      this.wsBuffer = Buffer.concat([this.wsBuffer, head]);
    }

    this.wsSocket.on("data", (data) => {
      this.wsBuffer = Buffer.concat([this.wsBuffer, data]);
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
      const parsed = tryReadFrame(this.wsBuffer);
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
        if (frame.fin) {
          const payload = Buffer.concat(this.fragmentedChunks);
          const opcode = this.fragmentedOpcode;
          this.fragmentedOpcode = null;
          this.fragmentedChunks = [];
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

  private close(): void {
    if (this.closed) return;
    this.closed = true;

    this.wsSocket.destroy();
    this.tcpSocket.destroy();
  }
}

type TryReadFrameResult = { frame: ParsedFrame; remaining: Buffer };

function tryReadFrame(buffer: Buffer): TryReadFrameResult | null {
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

  return { frame: { fin, opcode, payload }, remaining };
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
    const header = Buffer.alloc(2);
    header[0] = finOpcode;
    header[1] = length;
    return Buffer.concat([header, payload]);
  }

  if (length < 65536) {
    const header = Buffer.alloc(4);
    header[0] = finOpcode;
    header[1] = 126;
    header.writeUInt16BE(length, 2);
    return Buffer.concat([header, payload]);
  }

  const header = Buffer.alloc(10);
  header[0] = finOpcode;
  header[1] = 127;
  header.writeUInt32BE(0, 2);
  header.writeUInt32BE(length, 6);
  return Buffer.concat([header, payload]);
}
