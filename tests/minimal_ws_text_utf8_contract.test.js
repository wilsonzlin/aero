import assert from "node:assert/strict";
import test from "node:test";
import net from "node:net";
import { randomBytes } from "node:crypto";

import { unrefBestEffort } from "../src/unref_safe.js";
import { WebSocketServer } from "../tools/minimal_ws.js";

function withTimeout(promise, ms, label) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`timeout: ${label}`)), ms);
    unrefBestEffort(timeout);
    promise.then(
      (value) => {
        clearTimeout(timeout);
        resolve(value);
      },
      (err) => {
        clearTimeout(timeout);
        reject(err);
      },
    );
  });
}

function readUntil(sock, marker, maxBytes) {
  return new Promise((resolve, reject) => {
    let buf = Buffer.alloc(0);
    const markerBuf = Buffer.isBuffer(marker) ? marker : Buffer.from(marker, "utf8");

    const onData = (chunk) => {
      buf = buf.length === 0 ? chunk : Buffer.concat([buf, chunk]);
      if (buf.length > maxBytes) {
        cleanup();
        reject(new Error("readUntil exceeded maxBytes"));
        return;
      }
      const idx = buf.indexOf(markerBuf);
      if (idx !== -1) {
        const head = buf.subarray(0, idx + markerBuf.length);
        const rest = buf.subarray(idx + markerBuf.length);
        cleanup();
        resolve({ head, rest });
      }
    };
    const onError = (err) => {
      cleanup();
      reject(err);
    };
    const onClose = () => {
      cleanup();
      reject(new Error("socket closed before marker"));
    };
    const cleanup = () => {
      sock.off("data", onData);
      sock.off("error", onError);
      sock.off("close", onClose);
    };

    sock.on("data", onData);
    sock.on("error", onError);
    sock.on("close", onClose);
  });
}

function parseServerFrame(buf) {
  if (buf.length < 2) return null;
  const b0 = buf[0];
  const b1 = buf[1];
  const fin = (b0 & 0x80) !== 0;
  const opcode = b0 & 0x0f;
  const masked = (b1 & 0x80) !== 0;
  let len = b1 & 0x7f;
  let offset = 2;

  if (!fin) throw new Error("unexpected fragmented frame");
  if (masked) throw new Error("server frame must not be masked");

  if (len === 126) {
    if (buf.length < offset + 2) return null;
    len = buf.readUInt16BE(offset);
    offset += 2;
  } else if (len === 127) {
    if (buf.length < offset + 8) return null;
    const big = buf.readBigUInt64BE(offset);
    if (big > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("oversized frame length");
    len = Number(big);
    offset += 8;
  }

  if (buf.length < offset + len) return null;
  const payload = buf.subarray(offset, offset + len);
  const rest = buf.subarray(offset + len);
  return { opcode, payload, rest };
}

function encodeMaskedClientTextFrame(payloadBytes) {
  const payload = Buffer.from(payloadBytes);
  const maskKey = randomBytes(4);
  const out = Buffer.allocUnsafe(2 + 4 + payload.length);
  out[0] = 0x81; // FIN + text
  out[1] = 0x80 | (payload.length & 0x7f);
  maskKey.copy(out, 2);
  for (let i = 0; i < payload.length; i++) {
    out[6 + i] = payload[i] ^ maskKey[i & 3];
  }
  return out;
}

test("minimal_ws: invalid UTF-8 text frames close with 1007", async () => {
  const wss = new WebSocketServer({ port: 0, host: "127.0.0.1" });
  await withTimeout(
    new Promise((resolve, reject) => {
      wss.once("listening", resolve);
      wss.once("error", reject);
    }),
    2000,
    "wss listening",
  );

  const addr = wss.address();
  assert(addr && typeof addr === "object");

  const sock = net.connect({ host: "127.0.0.1", port: addr.port });
  await withTimeout(
    new Promise((resolve, reject) => {
      sock.once("connect", resolve);
      sock.once("error", reject);
    }),
    2000,
    "socket connect",
  );

  const key = randomBytes(16).toString("base64");
  const req =
    [
      "GET / HTTP/1.1",
      `Host: 127.0.0.1:${addr.port}`,
      "Upgrade: websocket",
      "Connection: Upgrade",
      "Sec-WebSocket-Version: 13",
      `Sec-WebSocket-Key: ${key}`,
      "",
      "",
    ].join("\r\n");
  sock.write(req);

  const { rest: afterHeaders } = await withTimeout(readUntil(sock, "\r\n\r\n", 16 * 1024), 2000, "handshake");
  let buf = afterHeaders;

  // Overlong UTF-8 sequence: invalid.
  sock.write(encodeMaskedClientTextFrame([0xc0, 0xaf]));

  // Expect the server to reply with a close frame containing code 1007.
  while (true) {
    const parsed = parseServerFrame(buf);
    if (parsed) {
      assert.equal(parsed.opcode, 0x8);
      assert.ok(parsed.payload.length >= 2);
      const code = parsed.payload.readUInt16BE(0);
      assert.equal(code, 1007);
      break;
    }
    const chunk = await withTimeout(
      new Promise((resolve, reject) => {
        sock.once("data", resolve);
        sock.once("error", reject);
      }),
      2000,
      "close frame",
    );
    buf = buf.length === 0 ? chunk : Buffer.concat([buf, chunk]);
    if (buf.length > 64 * 1024) throw new Error("buffer exceeded while waiting for close");
  }

  sock.end();
  await withTimeout(
    new Promise((resolve) => sock.once("close", resolve)),
    2000,
    "socket close",
  );
  await new Promise((resolve) => wss.close(resolve));
});

