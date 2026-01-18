const TCP_MUX_SUBPROTOCOL = "aero-tcp-mux-v1";
const HEADER_BYTES = 9;

import WebSocket from "ws";
import { wsCloseSafe, wsSendSafe } from "../../../scripts/_shared/ws_safe.js";

const MsgType = {
  OPEN: 1,
  DATA: 2,
  CLOSE: 3,
  ERROR: 4,
  PING: 5,
  PONG: 6,
};

const CloseFlags = {
  FIN: 0x01,
  RST: 0x02,
};

function decodeErrorPayload(payload) {
  if (payload.length < 4) return { code: 0, message: "ERROR payload too short" };
  const code = payload.readUInt16BE(0);
  const messageLen = payload.readUInt16BE(2);
  if (payload.length !== 4 + messageLen) return { code, message: "ERROR payload length mismatch" };
  const message = payload.subarray(4).toString("utf8");
  return { code, message };
}

function decodeClosePayload(payload) {
  if (payload.length !== 1) return { flags: 0 };
  return { flags: payload.readUInt8(0) };
}

function replaceProtocol(input, map) {
  const url = new URL(input);
  if (map[url.protocol]) url.protocol = map[url.protocol];
  return url;
}

async function bootstrapSessionCookie(gatewayBase) {
  const httpUrl = replaceProtocol(gatewayBase, { "ws:": "http:", "wss:": "https:" });
  httpUrl.pathname = `${httpUrl.pathname.replace(/\/$/, "")}/session`;

  const res = await fetch(httpUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: "{}",
  });

  if (!res.ok) {
    throw new Error(`POST /session failed: HTTP ${res.status}`);
  }

  const setCookie = res.headers.get("set-cookie");
  if (!setCookie) {
    throw new Error("POST /session did not return a Set-Cookie header");
  }

  // Format: "aero_session=...; Path=/; HttpOnly; ..."
  return setCookie.split(";")[0];
}

function encodeFrame(msgType, streamId, payload) {
  const payloadBuf = payload ?? Buffer.alloc(0);
  const buf = Buffer.allocUnsafe(HEADER_BYTES + payloadBuf.length);
  buf.writeUInt8(msgType, 0);
  buf.writeUInt32BE(streamId >>> 0, 1);
  buf.writeUInt32BE(payloadBuf.length >>> 0, 5);
  payloadBuf.copy(buf, HEADER_BYTES);
  return buf;
}

function encodeOpenPayload({ host, port, metadata }) {
  const hostBytes = Buffer.from(host, "utf8");
  const metadataBytes = metadata ? Buffer.from(metadata, "utf8") : Buffer.alloc(0);

  const buf = Buffer.allocUnsafe(2 + hostBytes.length + 2 + 2 + metadataBytes.length);
  let offset = 0;
  buf.writeUInt16BE(hostBytes.length, offset);
  offset += 2;
  hostBytes.copy(buf, offset);
  offset += hostBytes.length;
  buf.writeUInt16BE(port, offset);
  offset += 2;
  buf.writeUInt16BE(metadataBytes.length, offset);
  offset += 2;
  metadataBytes.copy(buf, offset);
  return buf;
}

// Usage:
//   node backend/aero-gateway/examples/tcp-mux-client.js http://127.0.0.1:8080 127.0.0.1 1234
//
// This script bootstraps an `aero_session` cookie via `POST /session`, then
// opens a `/tcp-mux` WebSocket using the canonical `aero-tcp-mux-v1` framing.
const [, , gatewayBase = "http://127.0.0.1:8080", targetHost = "example.com", targetPortStr = "80"] = process.argv;
const targetPort = Number.parseInt(targetPortStr, 10);

const cookie = await bootstrapSessionCookie(gatewayBase);
const wsUrl = replaceProtocol(gatewayBase, { "http:": "ws:", "https:": "wss:" });
wsUrl.pathname = `${wsUrl.pathname.replace(/\/$/, "")}/tcp-mux`;

const ws = new WebSocket(wsUrl.toString(), TCP_MUX_SUBPROTOCOL, {
  headers: { cookie },
});
ws.binaryType = "arraybuffer";

ws.on("open", () => {
  const streamId = 1;
  if (!wsSendSafe(ws, encodeFrame(MsgType.OPEN, streamId, encodeOpenPayload({ host: targetHost, port: targetPort })))) {
    wsCloseSafe(ws);
    return;
  }
  const request = `GET / HTTP/1.1\r\nHost: ${targetHost}\r\nConnection: close\r\n\r\n`;
  if (!wsSendSafe(ws, encodeFrame(MsgType.DATA, streamId, Buffer.from(request, "utf8")))) {
    wsCloseSafe(ws);
    return;
  }

  // Closing is optional here because we set "Connection: close". We still send
  // a FIN after a short delay to demonstrate half-close propagation.
  setTimeout(() => {
    if (!wsSendSafe(ws, encodeFrame(MsgType.CLOSE, streamId, Buffer.from([CloseFlags.FIN])))) {
      wsCloseSafe(ws);
    }
  }, 1000);
});

let pending = Buffer.alloc(0);

ws.on("message", (data) => {
  const chunk =
    data instanceof ArrayBuffer ? Buffer.from(data) : Array.isArray(data) ? Buffer.concat(data) : Buffer.from(data);
  pending = pending.length === 0 ? chunk : Buffer.concat([pending, chunk]);

  // Frames are carried in a byte stream: multiple frames may be concatenated in
  // a WebSocket message, or split across messages.
  while (pending.length >= HEADER_BYTES) {
    const msgType = pending.readUInt8(0);
    const streamId = pending.readUInt32BE(1);
    const length = pending.readUInt32BE(5);
    const total = HEADER_BYTES + length;
    if (pending.length < total) break;

    const payload = pending.subarray(HEADER_BYTES, total);
    pending = pending.subarray(total);

    if (msgType === MsgType.DATA) {
      process.stdout.write(`[stream ${streamId}] ${payload.toString("utf8")}`);
    } else if (msgType === MsgType.ERROR) {
      const { code, message } = decodeErrorPayload(payload);
      console.error(`[stream ${streamId}] ERROR code=${code} message=${message}`);
    } else if (msgType === MsgType.CLOSE) {
      const { flags } = decodeClosePayload(payload);
      const parts = [];
      if (flags & CloseFlags.FIN) parts.push("FIN");
      if (flags & CloseFlags.RST) parts.push("RST");
      console.log(`[stream ${streamId}] CLOSE ${parts.length ? parts.join("|") : `flags=0x${flags.toString(16)}`}`);
    } else if (msgType === MsgType.PING) {
      // Reply with PONG (same payload) per protocol.
      if (!wsSendSafe(ws, encodeFrame(MsgType.PONG, streamId, payload))) {
        wsCloseSafe(ws);
      }
    } else if (msgType === MsgType.PONG) {
      console.log(`[stream ${streamId}] PONG len=${length}`);
    } else {
      console.log(`[stream ${streamId}] msg_type=${msgType} len=${length}`);
    }
  }
});

ws.on("close", () => {
  console.log("ws closed");
});

ws.on("error", (err) => {
  console.error("ws error", err);
});
