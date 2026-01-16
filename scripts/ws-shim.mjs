// A tiny fallback implementation of the `ws` package.
//
// The upstream repo uses `ws` in a handful of Node-based utilities/tests, but
// our agent execution environment is offline and does not install `node_modules`.
//
// `scripts/ts-strip-loader.mjs` resolves bare `ws` imports to this file when the
// real package is unavailable. The goal is **compatibility**, not completeness:
// we implement enough of the API surface to run the repo's unit tests
// (`WebSocket`, `WebSocketServer`, and `createWebSocketStream`).
//
// This is deliberately dependency-free (Node built-ins only).

import { EventEmitter } from "node:events";
import http from "node:http";
import https from "node:https";
import { createHash, randomBytes } from "node:crypto";
import { Duplex } from "node:stream";
import { isValidHttpToken } from "../src/httpTokens.js";
import { formatOneLineUtf8 } from "../src/text.js";

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const utf8DecoderFatal = new TextDecoder("utf-8", { fatal: true });

const MAX_UPGRADE_URL_LEN = 8 * 1024;
const MAX_SUBPROTOCOL_HEADER_LEN = 4 * 1024;
const MAX_SUBPROTOCOL_TOKENS = 32;
const MAX_WS_KEY_LEN = 256;
// RFC 6455 close reason is limited to 123 bytes (125 total payload bytes incl. 2-byte code).
const MAX_WS_CLOSE_REASON_BYTES = 123;

function destroyQuietly(socket) {
  try {
    socket.destroy();
  } catch {
    // ignore
  }
}

function checkedClientUrl(address) {
  if (address instanceof URL) {
    const href = address.href;
    if (href.length > MAX_UPGRADE_URL_LEN) {
      throw new RangeError("WebSocket URL too long");
    }
    return address;
  }
  if (typeof address === "string") {
    if (address.length > MAX_UPGRADE_URL_LEN) {
      throw new RangeError("WebSocket URL too long");
    }
    return new URL(address);
  }
  throw new TypeError("Invalid WebSocket address");
}

function buildClientProtocolsHeader(protocols) {
  if (!protocols) return "";
  const list = normalizeProtocols(protocols);
  if (list.length === 0) return "";
  if (list.length > MAX_SUBPROTOCOL_TOKENS) {
    throw new RangeError("Too many subprotocols");
  }
  for (const p of list) {
    if (!isValidHttpToken(p)) {
      throw new TypeError("WebSocket subprotocol must be a valid token");
    }
  }
  let totalLen = 0;
  for (const p of list) {
    totalLen += p.length;
    if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) {
      throw new RangeError("Sec-WebSocket-Protocol too long");
    }
  }
  if (list.length > 1) {
    totalLen += 2 * (list.length - 1);
  }
  if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) {
    throw new RangeError("Sec-WebSocket-Protocol too long");
  }
  return list.join(", ");
}

function toBuffer(data) {
  if (Buffer.isBuffer(data)) return data;
  if (typeof data === "string") return Buffer.from(data, "utf8");
  if (data instanceof ArrayBuffer) return Buffer.from(new Uint8Array(data));
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);
  if (data === null) return Buffer.from("null", "utf8");
  switch (typeof data) {
    case "number":
    case "boolean":
    case "bigint":
    case "symbol":
    case "undefined":
      {
        const text = String(data);
        return Buffer.from(text, "utf8");
      }
    case "object":
    case "function":
    default:
      throw new TypeError("Unsupported WebSocket data type");
  }
}

function commaSeparatedTokens(value, { maxTokens }) {
  const out = [];
  const s = value.trim();
  if (s === "") return out;

  let i = 0;
  while (i < s.length) {
    while (i < s.length && (s[i] === " " || s[i] === "\t" || s[i] === ",")) i += 1;
    const start = i;
    while (i < s.length && s[i] !== ",") i += 1;
    const end = i;
    const token = s.slice(start, end).trim();
    if (token) {
      if (!isValidHttpToken(token)) return null;
      out.push(token);
      if (out.length > maxTokens) return null;
    }
    if (i < s.length && s[i] === ",") i += 1;
  }
  return out;
}

function parseProtocolsHeader(header) {
  let raw = "";
  if (typeof header === "string") {
    raw = header;
  } else if (Array.isArray(header)) {
    let totalLen = 0;
    for (const part of header) {
      if (typeof part !== "string") return null;
      totalLen += part.length;
      if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) return null;
    }
    // Account for the commas inserted by join().
    if (header.length > 1) totalLen += header.length - 1;
    if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) return null;
    raw = header.join(",");
  }
  if (raw.length > MAX_SUBPROTOCOL_HEADER_LEN) return null;
  if (!raw) return [];
  return commaSeparatedTokens(raw, { maxTokens: MAX_SUBPROTOCOL_TOKENS });
}

function normalizeProtocols(protocols) {
  if (!protocols) return [];
  if (typeof protocols === "string") return [protocols];
  if (Array.isArray(protocols)) return protocols.filter((p) => typeof p === "string" && p.length > 0);
  return [];
}

function maskPayload(payload, maskKey) {
  const out = Buffer.allocUnsafe(payload.length);
  for (let i = 0; i < payload.length; i++) {
    out[i] = payload[i] ^ maskKey[i % 4];
  }
  return out;
}

function encodeFrame(opcode, payload, { mask }) {
  const finOpcode = 0x80 | (opcode & 0x0f);
  const length = payload.length;

  let header;
  if (length < 126) {
    header = Buffer.alloc(2);
    header[0] = finOpcode;
    header[1] = (mask ? 0x80 : 0) | length;
  } else if (length < 65536) {
    header = Buffer.alloc(4);
    header[0] = finOpcode;
    header[1] = (mask ? 0x80 : 0) | 126;
    header.writeUInt16BE(length, 2);
  } else {
    // Payloads larger than 2^32 aren't realistic here; keep it simple.
    header = Buffer.alloc(10);
    header[0] = finOpcode;
    header[1] = (mask ? 0x80 : 0) | 127;
    header.writeUInt32BE(0, 2);
    header.writeUInt32BE(length >>> 0, 6);
  }

  if (!mask) {
    return Buffer.concat([header, payload]);
  }

  const maskKey = randomBytes(4);
  const masked = maskPayload(payload, maskKey);
  return Buffer.concat([header, maskKey, masked]);
}

function tryReadFrame(buffer, { maxPayloadBytes, expectMasked }) {
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
      return {
        frame: { fin: true, opcode: 0x8, payload: Buffer.from([0x03, 0xea]), masked: false },
        remaining: Buffer.alloc(0),
      };
    }
    length = combined;
  }

  if (length > maxPayloadBytes) {
    return {
      frame: { fin: true, opcode: 0x8, payload: Buffer.from([0x03, 0xf1]), masked: false },
      remaining: Buffer.alloc(0),
    };
  }

  let maskKey = null;
  if (masked) {
    if (buffer.length < offset + 4) return null;
    maskKey = buffer.subarray(offset, offset + 4);
    offset += 4;
  } else if (expectMasked) {
    return {
      frame: { fin: true, opcode: 0x8, payload: Buffer.from([0x03, 0xea]), masked: false },
      remaining: Buffer.alloc(0),
    };
  }

  if (buffer.length < offset + length) return null;

  let payload = buffer.subarray(offset, offset + length);
  const remaining = buffer.subarray(offset + length);

  if (masked && maskKey) {
    payload = maskPayload(payload, maskKey);
  }

  return { frame: { fin, opcode, payload, masked }, remaining };
}

function decodeClosePayload(payload) {
  if (payload.length < 2) return { code: 1000, reason: Buffer.alloc(0) };
  const code = payload.readUInt16BE(0);
  const reason = payload.subarray(2);
  return { code, reason };
}

function encodeClosePayload(code, reason) {
  const safeReason = reason ? formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES) : "";
  const reasonBuf = safeReason ? Buffer.from(safeReason, "utf8") : Buffer.alloc(0);
  const buf = Buffer.alloc(2 + reasonBuf.length);
  buf.writeUInt16BE(code, 0);
  reasonBuf.copy(buf, 2);
  return buf;
}

class BaseWebSocket extends EventEmitter {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;

  CONNECTING = BaseWebSocket.CONNECTING;
  OPEN = BaseWebSocket.OPEN;
  CLOSING = BaseWebSocket.CLOSING;
  CLOSED = BaseWebSocket.CLOSED;

  binaryType = "nodebuffer";
  protocol = "";

  /** @type {import('node:net').Socket | null} */
  #socket = null;
  #buffer = Buffer.alloc(0);
  #maxPayloadBytes = 16 * 1024 * 1024;
  #expectMasked = false;

  #fragmentedOpcode = null;
  #fragmentedChunks = [];
  #fragmentedBytes = 0;

  #closeEmitted = false;
  #closeSent = false;
  #readyState = BaseWebSocket.CONNECTING;

  /** @param {object} opts */
  _attachSocket(socket, head, opts) {
    this.#socket = socket;
    // Compatibility with the `ws` package: some tests reach into `_socket` to
    // simulate backpressure (pause/resume).
    this._socket = socket;
    this.#buffer = head && head.length > 0 ? Buffer.from(head) : Buffer.alloc(0);
    this.#maxPayloadBytes = opts.maxPayloadBytes ?? this.#maxPayloadBytes;
    this.#expectMasked = opts.expectMasked ?? false;
    this.protocol = opts.protocol ?? "";
    this.#readyState = BaseWebSocket.OPEN;

    socket.on("data", (data) => {
      this.#buffer = Buffer.concat([this.#buffer, data]);
      this.#drain();
    });
    socket.on("error", (err) => {
      this.emit("error", err);
      this.#finalizeClose(1006, Buffer.alloc(0));
    });
    socket.on("end", () => {
      this.#finalizeClose(1006, Buffer.alloc(0));
      // WebSocket does not support half-close. Ensure the underlying TCP
      // connection fully closes so servers awaiting `server.close()` do not hang.
      try {
        socket.destroy();
      } catch {
        // ignore
      }
    });
    socket.on("close", () => {
      this.#finalizeClose(1006, Buffer.alloc(0));
    });

    this.#drain();
  }

  get readyState() {
    return this.#readyState;
  }

  get bufferedAmount() {
    return this.#socket?.writableLength ?? 0;
  }

  send(data, optionsOrCb, cb) {
    if (typeof optionsOrCb === "function") cb = optionsOrCb;

    if (!this.#socket || this.#readyState !== BaseWebSocket.OPEN) {
      cb?.(new Error("WebSocket is not open"));
      return;
    }

    const isText = typeof data === "string";
    const opcode = isText ? 0x1 : 0x2;
    let payload;
    try {
      payload = toBuffer(data);
    } catch (err) {
      const e = err instanceof Error ? err : new Error("Failed to encode WebSocket payload");
      cb?.(e);
      this.emit("error", e);
      return;
    }
    const mask = !this.#expectMasked; // client masks, server doesn't.
    const frame = encodeFrame(opcode, payload, { mask });
    try {
      this.#socket.write(frame, cb ? () => cb?.() : undefined);
    } catch (err) {
      cb?.(err);
      this.emit("error", err);
    }
  }

  close(code = 1000, reason = "") {
    if (!this.#socket) return;
    if (this.#readyState === BaseWebSocket.CLOSING || this.#readyState === BaseWebSocket.CLOSED) return;
    this.#readyState = BaseWebSocket.CLOSING;

    const payload = encodeClosePayload(code, reason);
    const mask = !this.#expectMasked;
    const frame = encodeFrame(0x8, payload, { mask });
    this.#closeSent = true;
    try {
      this.#socket.write(frame, () => {
        try {
          this.#socket.end();
        } catch {
          // ignore
        }
      });
    } catch {
      try {
        this.#socket.destroy();
      } catch {
        // ignore
      }
    }
  }

  terminate() {
    if (!this.#socket) return;
    try {
      this.#socket.destroy();
    } catch {
      // ignore
    }
  }

  #emitMessage(opcode, payload) {
    const isBinary = opcode === 0x2;

    if (isBinary) {
      if (this.binaryType === "arraybuffer") {
        const ab = payload.buffer.slice(payload.byteOffset, payload.byteOffset + payload.byteLength);
        this.emit("message", ab, true);
      } else {
        this.emit("message", payload, true);
      }
      return;
    }

    // Text
    let text;
    try {
      text = utf8DecoderFatal.decode(payload);
    } catch {
      this.#sendCloseAndDrop(1007);
      return;
    }
    this.emit("message", text, false);
  }

  #finalizeClose(code, reason) {
    if (this.#closeEmitted) return;
    this.#closeEmitted = true;
    this.#readyState = BaseWebSocket.CLOSED;
    this.emit("close", code, reason);
  }

  #drain() {
    while (true) {
      const parsed = tryReadFrame(this.#buffer, {
        maxPayloadBytes: this.#maxPayloadBytes,
        expectMasked: this.#expectMasked,
      });
      if (!parsed) return;
      this.#buffer = parsed.remaining;
      this.#handleFrame(parsed.frame);
      if (this.#readyState === BaseWebSocket.CLOSED) return;
    }
  }

  #handleFrame(frame) {
    const { fin, opcode, payload } = frame;

    switch (opcode) {
      case 0x0: {
        if (this.#fragmentedOpcode === null) {
          this.#sendCloseAndDrop(1002);
          return;
        }
        this.#fragmentedChunks.push(payload);
        this.#fragmentedBytes += payload.length;
        if (this.#fragmentedBytes > this.#maxPayloadBytes) {
          this.#sendCloseAndDrop(1009);
          return;
        }
        if (fin) {
          const op = this.#fragmentedOpcode;
          const full = Buffer.concat(this.#fragmentedChunks);
          this.#fragmentedOpcode = null;
          this.#fragmentedChunks = [];
          this.#fragmentedBytes = 0;
          this.#emitMessage(op, full);
        }
        return;
      }
      case 0x1:
      case 0x2: {
        if (this.#fragmentedOpcode !== null) {
          this.#sendCloseAndDrop(1002);
          return;
        }
        if (fin) {
          this.#emitMessage(opcode, payload);
          return;
        }
        this.#fragmentedOpcode = opcode;
        this.#fragmentedChunks = [payload];
        this.#fragmentedBytes = payload.length;
        if (this.#fragmentedBytes > this.#maxPayloadBytes) {
          this.#sendCloseAndDrop(1009);
        }
        return;
      }
      case 0x8: {
        const { code, reason } = decodeClosePayload(payload);
        if (!this.#closeSent) {
          // Mirror the close frame per RFC6455.
          const mask = !this.#expectMasked;
          const response = encodeFrame(0x8, payload, { mask });
          this.#closeSent = true;
          try {
            this.#socket?.write(response, () => this.#socket?.end());
          } catch {
            // ignore
          }
        }
        this.#finalizeClose(code, reason);
        try {
          this.#socket?.end();
        } catch {
          // ignore
        }
        return;
      }
      case 0x9: {
        // Ping -> Pong
        const mask = !this.#expectMasked;
        try {
          this.#socket?.write(encodeFrame(0xA, payload, { mask }));
        } catch {
          // ignore
        }
        return;
      }
      case 0xA: {
        // Pong
        this.emit("pong", payload);
        return;
      }
      default: {
        this.#sendCloseAndDrop(1002);
      }
    }
  }

  #sendCloseAndDrop(code) {
    if (!this.#socket) return;
    const payload = Buffer.alloc(2);
    payload.writeUInt16BE(code, 0);
    const mask = !this.#expectMasked;
    try {
      this.#socket.write(encodeFrame(0x8, payload, { mask }), () => this.#socket?.destroy());
    } catch {
      try {
        this.#socket.destroy();
      } catch {
        // ignore
      }
    }
    this.#finalizeClose(code, Buffer.alloc(0));
  }
}

export class WebSocket extends BaseWebSocket {
  constructor(address, protocols, options) {
    super();

    if (typeof address !== "string" && !(address instanceof URL)) {
      // Internal constructor used by WebSocketServer.
      const socket = address;
      const opts = protocols ?? {};
      super._attachSocket(socket, options ?? Buffer.alloc(0), {
        expectMasked: true,
        maxPayloadBytes: opts.maxPayload ?? 16 * 1024 * 1024,
        protocol: opts.protocol ?? "",
      });
      queueMicrotask(() => this.emit("open"));
      return;
    }

    const url = checkedClientUrl(address);
    const opts = options && typeof options === "object" ? options : {};
    const protocolHeader = buildClientProtocolsHeader(protocols);

    const headers = {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": randomBytes(16).toString("base64"),
      ...(protocolHeader ? { "Sec-WebSocket-Protocol": protocolHeader } : {}),
      ...(opts.headers ?? {}),
    };

    const requestLib = url.protocol === "wss:" ? https : http;

    const req = requestLib.request(
      {
        protocol: url.protocol === "wss:" ? "https:" : "http:",
        hostname: url.hostname,
        port: url.port ? Number(url.port) : url.protocol === "wss:" ? 443 : 80,
        path: `${url.pathname}${url.search}`,
        headers,
      },
      (res) => {
        // Non-101 response.
        this.emit("unexpected-response", req, res);
      },
    );

    req.on("upgrade", (res, socket, head) => {
      const negotiated = res.headers["sec-websocket-protocol"];
      const protocol = typeof negotiated === "string" ? negotiated : "";
      super._attachSocket(socket, head, { expectMasked: false, protocol });
      this.emit("open");
    });

    req.on("error", (err) => {
      this.emit("error", err);
    });

    req.end();
  }
}

export class WebSocketServer extends EventEmitter {
  /** @type {http.Server | null} */
  #server = null;
  #internalServer = false;
  #options;
  #clients = new Set();
  #upgradeListener = null;

  constructor(options = {}) {
    super();
    this.#options = options;

    if (options.server) {
      this.#server = options.server;
      this.#attachToServer();
      return;
    }

    if (options.noServer) {
      return;
    }

    this.#server = http.createServer((req, res) => {
      res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
      res.end("not found\n");
    });
    this.#internalServer = true;
    this.#attachToServer();

    const host = options.host ?? "0.0.0.0";
    const port = options.port ?? 0;
    this.#server.listen(port, host, () => this.emit("listening"));
  }

  #attachToServer() {
    if (!this.#server) return;
    this.#upgradeListener = (req, socket, head) => {
      if (this.#options.path) {
        const rawUrl = req.url ?? "/";
        if (typeof rawUrl !== "string" || rawUrl.length > MAX_UPGRADE_URL_LEN) {
          socket.destroy();
          return;
        }
        let url;
        try {
          url = new URL(rawUrl, "http://localhost");
        } catch {
          socket.destroy();
          return;
        }
        if (url.pathname !== this.#options.path) {
          // Match `ws` behavior: path mismatch aborts the handshake.
          destroyQuietly(socket);
          return;
        }
      }

      this.handleUpgrade(req, socket, head, (ws) => {
        this.emit("connection", ws, req);
      });
    };

    this.#server.on("upgrade", this.#upgradeListener);
  }

  address() {
    return this.#server?.address?.() ?? null;
  }

  get clients() {
    return this.#clients;
  }

  handleUpgrade(req, socket, head, cb) {
    try {
      const key = req.headers["sec-websocket-key"];
      if (typeof key !== "string" || key === "" || key.length > MAX_WS_KEY_LEN) {
        socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
        return;
      }

      const offered = parseProtocolsHeader(req.headers["sec-websocket-protocol"]);
      if (offered === null) {
        socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
        return;
      }
      const protocols = new Set(offered);

      let selected = "";
      if (this.#options.handleProtocols) {
        const res = this.#options.handleProtocols(protocols, req);
        if (res === false || typeof res !== "string" || res.trim() === "") {
          socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
          return;
        }
        selected = res.trim();
        if (!protocols.has(selected)) {
          socket.end("HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
          return;
        }
      }

      const accept = createHash("sha1").update(key + WS_GUID).digest("base64");

      const headers = [
        "HTTP/1.1 101 Switching Protocols",
        "Upgrade: websocket",
        "Connection: Upgrade",
        `Sec-WebSocket-Accept: ${accept}`,
        ...(selected ? [`Sec-WebSocket-Protocol: ${selected}`] : []),
        "\r\n",
      ].join("\r\n");

      socket.write(headers);

      // The constructor overload with a socket is internal-only.
      const ws = new WebSocket(socket, { protocol: selected, maxPayload: this.#options.maxPayload }, head);

      this.#clients.add(ws);
      const release = () => this.#clients.delete(ws);
      ws.once("close", release);
      ws.once("error", release);

      cb(ws, req);
    } catch (err) {
      try {
        socket.destroy();
      } catch {
        // ignore
      }
      this.emit("error", err);
    }
  }

  close(cb) {
    for (const ws of this.#clients) {
      try {
        ws.terminate();
      } catch {
        // ignore
      }
    }
    this.#clients.clear();

    if (this.#server && this.#upgradeListener) {
      this.#server.off("upgrade", this.#upgradeListener);
    }

    if (this.#internalServer && this.#server) {
      this.#server.close(() => cb?.());
      return;
    }

    cb?.();
  }
}

export function createWebSocketStream(ws, opts = {}) {
  const highWaterMark = opts.highWaterMark ?? 16 * 1024;

  const stream = new Duplex({
    readableHighWaterMark: highWaterMark,
    writableHighWaterMark: highWaterMark,
    write(chunk, _enc, callback) {
      try {
        ws.send(chunk, (err) => callback(err));
      } catch (err) {
        callback(err);
      }
    },
    read() {
      // no-op: data is pushed from the WebSocket 'message' handler.
    },
    final(callback) {
      try {
        ws.close();
      } catch {
        // ignore
      }
      callback();
    },
  });

  ws.on("message", (data, isBinary) => {
    if (typeof data === "string") {
      stream.push(Buffer.from(data, "utf8"));
      return;
    }
    if (data instanceof ArrayBuffer) {
      stream.push(Buffer.from(new Uint8Array(data)));
      return;
    }
    if (Array.isArray(data)) {
      stream.push(Buffer.concat(data));
      return;
    }
    if (!isBinary) return;
    stream.push(Buffer.isBuffer(data) ? data : Buffer.from(data));
  });

  ws.once("close", () => stream.push(null));
  ws.once("error", (err) => stream.destroy(err));

  return stream;
}

export default WebSocket;
