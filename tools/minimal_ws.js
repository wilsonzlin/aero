import http from "node:http";
import https from "node:https";
import net from "node:net";
import { EventEmitter } from "node:events";
import { randomBytes } from "node:crypto";
import { formatOneLineUtf8 } from "../src/text.js";
import { isValidHttpToken } from "../src/httpTokens.js";
import { rejectHttpUpgrade } from "../src/http_upgrade_reject.js";
import { computeWebSocketAccept, encodeWebSocketHandshakeResponse } from "../src/ws_handshake_response.js";
import { endThenDestroyQuietly } from "../src/socket_end_then_destroy.js";
import { destroyBestEffort, writeCaptureErrorBestEffort } from "../src/socket_safe.js";

const utf8DecoderFatal = new TextDecoder("utf-8", { fatal: true });

const MAX_SUBPROTOCOL_HEADER_LEN = 4 * 1024;
const MAX_SUBPROTOCOL_TOKENS = 32;
const MAX_WS_KEY_LEN = 256;
const MAX_WS_URL_LEN = 8 * 1024;
// RFC 6455 close reason is limited to 123 bytes (125 total payload bytes incl. 2-byte code).
const MAX_WS_CLOSE_REASON_BYTES = 123;

function trySocketWrite(socket, data, cb) {
  const args = typeof cb === "function" ? [data, cb] : [data];
  const res = writeCaptureErrorBestEffort(socket, ...args);
  if (!res.err) return res.ok;
  if (typeof cb === "function") queueMicrotask(() => cb(res.err));
  return false;
}

function trySocketDestroy(socket) {
  destroyBestEffort(socket);
}

function commaSeparatedTokens(raw, { maxLen, maxTokens }) {
  if (typeof raw !== "string") return null;
  if (raw.length > maxLen) return null;
  if (raw.length === 0) return [];

  const out = [];
  let i = 0;
  while (i < raw.length) {
    let end = raw.indexOf(",", i);
    if (end === -1) end = raw.length;

    let start = i;
    while (start < end && raw.charCodeAt(start) <= 0x20) start++;
    while (end > start && raw.charCodeAt(end - 1) <= 0x20) end--;

    if (end > start) {
      if (out.length >= maxTokens) return null;
      const token = raw.slice(start, end);
      if (!isValidHttpToken(token)) return null;
      out.push(token);
    }

    i = end + 1;
  }

  return out;
}

function parseProtocolsHeader(header) {
  if (typeof header === "string") {
    return commaSeparatedTokens(header, { maxLen: MAX_SUBPROTOCOL_HEADER_LEN, maxTokens: MAX_SUBPROTOCOL_TOKENS });
  }
  if (!Array.isArray(header)) return [];

  let totalLen = 0;
  const out = [];
  for (const part of header) {
    if (typeof part !== "string") return null;
    totalLen += part.length;
    if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) return null;

    const tokens = commaSeparatedTokens(part, {
      maxLen: MAX_SUBPROTOCOL_HEADER_LEN,
      maxTokens: MAX_SUBPROTOCOL_TOKENS - out.length,
    });
    if (tokens === null) return null;
    out.push(...tokens);
    if (out.length >= MAX_SUBPROTOCOL_TOKENS) break;
  }

  return out;
}

function encodeFrame(opcode, payload, { mask }) {
  const payloadBuf = Buffer.isBuffer(payload) ? payload : Buffer.from(payload);
  const payloadLen = payloadBuf.length;

  const finOpcode = 0x80 | (opcode & 0x0f);

  let lenField = 0;
  let extLen = Buffer.alloc(0);
  if (payloadLen < 126) {
    lenField = payloadLen;
  } else if (payloadLen <= 0xffff) {
    lenField = 126;
    extLen = Buffer.allocUnsafe(2);
    extLen.writeUInt16BE(payloadLen, 0);
  } else {
    lenField = 127;
    extLen = Buffer.allocUnsafe(8);
    // Note: we only need the low 32-bits for our use-cases (unit tests),
    // but write the full u64 per RFC6455.
    extLen.writeUInt32BE(0, 0);
    extLen.writeUInt32BE(payloadLen >>> 0, 4);
  }

  const maskBit = mask ? 0x80 : 0x00;
  const lenByte = maskBit | (lenField & 0x7f);

  if (!mask) {
    return Buffer.concat([Buffer.from([finOpcode, lenByte]), extLen, payloadBuf]);
  }

  const maskKey = randomBytes(4);
  const masked = Buffer.allocUnsafe(payloadLen);
  for (let i = 0; i < payloadLen; i++) masked[i] = payloadBuf[i] ^ maskKey[i & 3];
  return Buffer.concat([Buffer.from([finOpcode, lenByte]), extLen, maskKey, masked]);
}

class WebSocket extends EventEmitter {
  constructor(address, protocols = [], options = {}) {
    super();

    // Internal constructor for server-side accepted sockets.
    if (address instanceof net.Socket) {
      const { head = Buffer.alloc(0), isClient = false, protocol = "", maxPayload = 0 } = protocols ?? {};
      this._initFromSocket(address, head, { isClient, protocol, maxPayload });
      return;
    }

    /** @type {net.Socket | null} */
    this._socket = null;
    this._buffer = Buffer.alloc(0);
    this._isClient = true;
    this._maxPayload = 0;
    this._protocol = "";

    this._sentClose = false;
    this._closeCode = null;
    this._closeReason = Buffer.alloc(0);
    this._closeEmitted = false;

    this.binaryType = "nodebuffer";

    let protos = protocols;
    let opts = options;
    if (protos && typeof protos === "object" && !Array.isArray(protos) && typeof protos !== "string") {
      // new WebSocket(url, options)
      opts = protos;
      protos = [];
    }
    const protoList = Array.isArray(protos) ? protos : protos ? [protos] : [];

    this._connect(address, protoList, opts ?? {});
  }

  get protocol() {
    return this._protocol;
  }

  send(data, ...args) {
    if (!this._socket) throw new Error("WebSocket is not connected");
    const payload = normalizeSendData(data);
    const frame = encodeFrame(Buffer.isBuffer(payload) ? 0x2 : 0x1, payload, {
      mask: this._isClient,
    });
    const cb = typeof args[args.length - 1] === "function" ? args[args.length - 1] : null;
    const ok = trySocketWrite(this._socket, frame, cb ?? undefined);
    if (!ok && !cb) {
      queueMicrotask(() => this.emit("error", new Error("WebSocket send failed")));
      trySocketDestroy(this._socket);
    }
  }

  close(code = 1000, reason = "") {
    if (this._sentClose) return;
    this._sentClose = true;
    const payload = encodeClosePayload(code, reason);
    if (this._socket) {
      trySocketWrite(this._socket, encodeFrame(0x8, payload, { mask: this._isClient }));
      endThenDestroyQuietly(this._socket);
    }
  }

  terminate() {
    if (this._socket) trySocketDestroy(this._socket);
  }

  _connect(url, protocols, options) {
    let urlStr;
    let u;
    if (typeof url === "string") {
      urlStr = url;
      if (urlStr.length === 0 || urlStr.length > MAX_WS_URL_LEN) {
        throw new RangeError("WebSocket URL is invalid or too long");
      }
      try {
        u = new URL(urlStr);
      } catch {
        throw new TypeError("Invalid WebSocket URL");
      }
    } else if (url instanceof URL) {
      u = url;
      urlStr = u.href;
      if (urlStr.length === 0 || urlStr.length > MAX_WS_URL_LEN) {
        throw new RangeError("WebSocket URL is invalid or too long");
      }
    } else {
      throw new TypeError("WebSocket URL must be a string or URL");
    }
    if (u.protocol !== "ws:" && u.protocol !== "wss:") {
      queueMicrotask(() => this.emit("error", new Error(`Unsupported WebSocket URL scheme: ${u.protocol}`)));
      return;
    }

    const secure = u.protocol === "wss:";
    const mod = secure ? https : http;
    const port = u.port ? Number.parseInt(u.port, 10) : secure ? 443 : 80;

    if (!Array.isArray(protocols)) {
      throw new TypeError("WebSocket protocols must be an array");
    }
    if (protocols.length > MAX_SUBPROTOCOL_TOKENS) {
      throw new RangeError("Too many WebSocket subprotocols");
    }
    let protocolHeader = null;
    if (protocols.length > 0) {
      let totalLen = 0;
      for (const proto of protocols) {
        if (!isValidHttpToken(proto)) {
          throw new TypeError("WebSocket subprotocol must be a valid token");
        }
        totalLen += proto.length;
        if (totalLen > MAX_SUBPROTOCOL_HEADER_LEN) {
          throw new RangeError("WebSocket subprotocol header is too long");
        }
      }
      const joinedLen = totalLen + (protocols.length - 1) * 2; // ", "
      if (joinedLen > MAX_SUBPROTOCOL_HEADER_LEN) {
        throw new RangeError("WebSocket subprotocol header is too long");
      }
      protocolHeader = protocols.join(", ");
    }

    const key = randomBytes(16).toString("base64");
    const expectedAccept = computeWebSocketAccept(key);

    const headers = {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": key,
      ...(protocolHeader ? { "Sec-WebSocket-Protocol": protocolHeader } : null),
      ...(options.headers ?? {}),
    };

    const req = mod.request({
      protocol: secure ? "https:" : "http:",
      hostname: u.hostname,
      port,
      method: "GET",
      path: `${u.pathname}${u.search}`,
      headers,
    });

    let settled = false;

    req.once("upgrade", (res, socket, head) => {
      settled = true;

      const accept = res.headers["sec-websocket-accept"];
      if (typeof accept !== "string" || accept !== expectedAccept) {
        trySocketDestroy(socket);
        this.emit("error", new Error("Invalid Sec-WebSocket-Accept in handshake response"));
        return;
      }

      const protocol = typeof res.headers["sec-websocket-protocol"] === "string" ? res.headers["sec-websocket-protocol"] : "";

      this._initFromSocket(socket, head, { isClient: true, protocol, maxPayload: options.maxPayload ?? 0 });
      this.emit("open");
    });

    req.once("response", (res) => {
      if (settled) return;
      settled = true;
      this.emit("unexpected-response", req, res);
      // Ensure the response body is drained even if the consumer ignores it.
      try {
        res.resume();
      } catch {
        // ignore
      }
    });

    req.once("error", (err) => {
      if (settled) return;
      settled = true;
      this.emit("error", err);
    });

    try {
      req.end();
    } catch (err) {
      if (settled) return;
      settled = true;
      queueMicrotask(() => this.emit("error", err));
    }
  }

  _initFromSocket(socket, head, { isClient, protocol, maxPayload }) {
    this._socket = socket;
    this._buffer = head && head.length > 0 ? Buffer.from(head) : Buffer.alloc(0);
    this._isClient = Boolean(isClient);
    this._protocol = protocol ?? "";
    this._maxPayload = Number.isFinite(maxPayload) ? maxPayload : 0;

    try {
      socket.setNoDelay(true);
    } catch {
      // ignore
    }
    socket.on("data", (chunk) => this._onData(chunk));
    socket.on("error", (err) => this.emit("error", err));
    socket.on("close", () => this._onSocketClose());
    try {
      socket.resume();
    } catch {
      // ignore
    }

    if (this._buffer.length > 0) {
      this._drainFrames();
    }
  }

  _onData(chunk) {
    if (chunk.length === 0) return;
    this._buffer = this._buffer.length === 0 ? chunk : Buffer.concat([this._buffer, chunk]);
    this._drainFrames();
  }

  _fail(code, reason) {
    try {
      this.close(code, reason);
    } catch {
      // ignore
    }
  }

  _drainFrames() {
    while (true) {
      if (this._buffer.length < 2) return;

      const b0 = this._buffer[0];
      const b1 = this._buffer[1];

      const fin = (b0 & 0x80) !== 0;
      const opcode = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f;

      if (!fin) {
        this._fail(1002, "Fragmented frames not supported");
        return;
      }

      const expectMasked = !this._isClient;
      if (masked !== expectMasked) {
        this._fail(1002, "Invalid masking");
        return;
      }

      let offset = 2;
      if (len === 126) {
        if (this._buffer.length < offset + 2) return;
        len = this._buffer.readUInt16BE(offset);
        offset += 2;
      } else if (len === 127) {
        if (this._buffer.length < offset + 8) return;
        const big = this._buffer.readBigUInt64BE(offset);
        if (big > BigInt(Number.MAX_SAFE_INTEGER)) {
          this._fail(1009, "Message too large");
          return;
        }
        len = Number(big);
        offset += 8;
      }

      if (this._maxPayload > 0 && len > this._maxPayload) {
        this._fail(1009, "Message too large");
        return;
      }

      let maskKey = null;
      if (masked) {
        if (this._buffer.length < offset + 4) return;
        maskKey = this._buffer.subarray(offset, offset + 4);
        offset += 4;
      }

      if (this._buffer.length < offset + len) return;

      let payload = this._buffer.subarray(offset, offset + len);
      offset += len;

      if (maskKey) {
        const unmasked = Buffer.allocUnsafe(payload.length);
        for (let i = 0; i < payload.length; i++) {
          unmasked[i] = payload[i] ^ maskKey[i & 3];
        }
        payload = unmasked;
      } else {
        payload = Buffer.from(payload);
      }

      this._buffer = this._buffer.subarray(offset);

      if (opcode === 0x1) {
        let text;
        try {
          text = utf8DecoderFatal.decode(payload);
        } catch {
          this._fail(1007, "Invalid UTF-8");
          return;
        }
        this.emit("message", text);
        continue;
      }
      if (opcode === 0x2) {
        this.emit("message", payload);
        continue;
      }
      if (opcode === 0x9) {
        // Ping → Pong.
        if (this._socket) {
          const ok = trySocketWrite(this._socket, encodeFrame(0x0a, payload, { mask: this._isClient }));
          if (!ok) trySocketDestroy(this._socket);
        }
        continue;
      }
      if (opcode === 0x0a) {
        // Pong: ignore.
        continue;
      }
      if (opcode === 0x8) {
        const { code, reason } = decodeClosePayload(payload);
        this._closeCode = code;
        this._closeReason = reason;

        if (!this._sentClose) {
          this._sentClose = true;
          if (this._socket) {
            trySocketWrite(this._socket, encodeFrame(0x8, encodeClosePayload(code, reason), { mask: this._isClient }));
          }
        }
        if (this._socket) endThenDestroyQuietly(this._socket);
        continue;
      }

      this._fail(1002, "Unsupported opcode");
      return;
    }
  }

  _onSocketClose() {
    if (this._closeEmitted) return;
    this._closeEmitted = true;
    const code = this._closeCode ?? (this._sentClose ? 1000 : 1006);
    const reason = this._closeReason ?? Buffer.alloc(0);
    this.emit("close", code, reason);
  }
}

function normalizeSendData(data) {
  if (typeof data === "string") return data;
  if (Buffer.isBuffer(data)) return data;
  if (data instanceof ArrayBuffer) return Buffer.from(data);
  if (ArrayBuffer.isView(data)) return Buffer.from(data.buffer, data.byteOffset, data.byteLength);

  // Match WebSocket's "send any scalar" ergonomics while avoiding `toString()` on
  // arbitrary objects/functions (can throw, be expensive, or be attacker-controlled).
  if (data === null) return "null";
  switch (typeof data) {
    case "number":
    case "boolean":
    case "bigint":
    case "symbol":
    case "undefined":
      return String(data);
    case "object":
    case "function":
    default:
      throw new TypeError("Unsupported WebSocket send() payload type");
  }
}

function encodeClosePayload(code, reason) {
  const reasonBuf = Buffer.isBuffer(reason)
    ? reason.subarray(0, MAX_WS_CLOSE_REASON_BYTES)
    : Buffer.from(formatOneLineUtf8(reason, MAX_WS_CLOSE_REASON_BYTES), "utf8");
  const buf = Buffer.allocUnsafe(2 + reasonBuf.length);
  buf.writeUInt16BE(code, 0);
  reasonBuf.copy(buf, 2);
  return buf;
}

function decodeClosePayload(payload) {
  if (payload.length < 2) return { code: 1005, reason: Buffer.alloc(0) };
  const code = payload.readUInt16BE(0);
  return { code, reason: payload.subarray(2) };
}

class WebSocketServer extends EventEmitter {
  constructor(options = {}) {
    super();
    this.clients = new Set();
    this._noServer = Boolean(options.noServer);
    this._maxPayload = Number.isFinite(options.maxPayload) ? options.maxPayload : 0;
    this._handleProtocols = typeof options.handleProtocols === "function" ? options.handleProtocols : null;

    this._server = null;
    if (!this._noServer) {
      const host = options.host ?? "127.0.0.1";
      const port = options.port ?? 0;

      const server = http.createServer();
      this._server = server;

      server.on("upgrade", (req, socket, head) => {
        try {
          this.handleUpgrade(req, socket, head, (ws) => this.emit("connection", ws, req));
        } catch {
          try {
            rejectHttpUpgrade(socket, 500, "WebSocket upgrade failed");
          } catch {
            trySocketDestroy(socket);
          }
        }
      });
      server.on("listening", () => this.emit("listening"));
      server.on("error", (err) => this.emit("error", err));
      server.listen(port, host);
    }
  }

  address() {
    return this._server ? this._server.address() : null;
  }

  close(cb) {
    for (const client of this.clients) client.terminate();
    this.clients.clear();

    if (this._server) {
      this._server.close(() => cb?.());
      return;
    }
    cb?.();
  }

  handleUpgrade(req, socket, head, cb) {
    try {
      const key = req.headers["sec-websocket-key"];
      if (typeof key !== "string" || key.length === 0 || key.length > MAX_WS_KEY_LEN) {
        trySocketDestroy(socket);
        return;
      }

      const offered = parseProtocolsHeader(req.headers["sec-websocket-protocol"]);
      if (offered === null) {
        rejectHttpUpgrade(socket, 400, "Invalid Sec-WebSocket-Protocol header");
        return;
      }
      const offeredSet = new Set(offered);
      let selected = "";
      if (this._handleProtocols) {
        const res = this._handleProtocols(offeredSet, req);
        if (res === false || typeof res !== "string") {
          rejectHttpUpgrade(socket, 400, "Bad Request");
          return;
        }
        const trimmed = res.trim();
        if (trimmed === "" || !offeredSet.has(trimmed)) {
          rejectHttpUpgrade(socket, 400, "Bad Request");
          return;
        }
        selected = trimmed;
      }

      const ok = trySocketWrite(socket, encodeWebSocketHandshakeResponse({ key, protocol: selected }));
      if (!ok) {
        trySocketDestroy(socket);
        return;
      }

      const ws = new WebSocket(socket, {
        head,
        isClient: false,
        protocol: selected,
        maxPayload: this._maxPayload,
      });
      this.clients.add(ws);
      const forget = () => this.clients.delete(ws);
      ws.once("close", forget);
      ws.once("error", forget);

      cb(ws);
    } catch {
      trySocketDestroy(socket);
    }
  }
}

export { WebSocket, WebSocketServer };
export default WebSocket;
