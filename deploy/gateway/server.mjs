import crypto from "node:crypto";
import http from "node:http";

const PORT = Number.parseInt(process.env.PORT ?? "8080", 10);

function writeJson(res, status, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Content-Length": Buffer.byteLength(body),
  });
  res.end(body);
}

function writeText(res, status, text) {
  res.writeHead(status, {
    "Content-Type": "text/plain; charset=utf-8",
    "Content-Length": Buffer.byteLength(text),
  });
  res.end(text);
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);

  // Minimal health endpoint for "is the proxy wiring correct?" checks.
  if (url.pathname === "/healthz") {
    writeText(res, 200, "ok\n");
    return;
  }

  // Minimal API endpoint for basic curl testing.
  if (url.pathname === "/api/echo") {
    writeJson(res, 200, {
      msg: url.searchParams.get("msg") ?? "",
      ts: Date.now(),
    });
    return;
  }

  // Placeholder for a DNS-over-HTTPS endpoint. Real implementations will likely
  // handle RFC 8484 POST/GET with "application/dns-message".
  if (url.pathname === "/dns-query") {
    writeText(res, 501, "dns-query not implemented in deploy stub\n");
    return;
  }

  writeText(res, 404, "not found\n");
});

// Very small WebSocket implementation (no external deps) to validate that the
// edge proxy forwards upgrade requests correctly. This is NOT a full-featured
// WebSocket server; it's only meant for deployment smoke tests.
server.on("upgrade", (req, socket, head) => {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (url.pathname !== "/tcp") {
    socket.destroy();
    return;
  }

  const key = req.headers["sec-websocket-key"];
  if (!key || typeof key !== "string") {
    socket.destroy();
    return;
  }

  const accept = crypto
    .createHash("sha1")
    .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`, "binary")
    .digest("base64");

  socket.write(
    [
      "HTTP/1.1 101 Switching Protocols",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Accept: ${accept}`,
      "",
      "",
    ].join("\r\n"),
  );

  let buffer = head?.length ? Buffer.from(head) : Buffer.alloc(0);

  socket.on("data", (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);

    // Process as many complete frames as we can.
    while (true) {
      const frame = parseWsFrame(buffer);
      if (!frame) break;
      buffer = buffer.subarray(frame.consumed);
      handleWsFrame(socket, frame);
    }
  });

  socket.on("error", () => {
    // Ignore; the client may disconnect at any time.
  });
});

server.listen(PORT, "0.0.0.0", () => {
  // eslint-disable-next-line no-console
  console.log(`deploy stub gateway listening on http://0.0.0.0:${PORT}`);
});

const WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
void WS_GUID; // documentation-only constant; keep in sync with handshake above.

function parseWsFrame(buf) {
  if (buf.length < 2) return null;

  const b0 = buf[0];
  const b1 = buf[1];

  const fin = (b0 & 0x80) !== 0;
  const opcode = b0 & 0x0f;

  const masked = (b1 & 0x80) !== 0;
  let payloadLen = b1 & 0x7f;

  let offset = 2;
  if (payloadLen === 126) {
    if (buf.length < offset + 2) return null;
    payloadLen = buf.readUInt16BE(offset);
    offset += 2;
  } else if (payloadLen === 127) {
    if (buf.length < offset + 8) return null;
    const hi = buf.readUInt32BE(offset);
    const lo = buf.readUInt32BE(offset + 4);
    offset += 8;
    // Only support lengths up to 2^32-1 for this stub.
    if (hi !== 0) {
      return {
        fin,
        opcode: 0x8,
        payload: Buffer.alloc(0),
        consumed: buf.length,
      };
    }
    payloadLen = lo;
  }

  if (payloadLen > 16 * 1024 * 1024) {
    // Too large for a smoke test; treat as protocol error.
    return {
      fin,
      opcode: 0x8,
      payload: Buffer.alloc(0),
      consumed: buf.length,
    };
  }

  let maskKey = null;
  if (masked) {
    if (buf.length < offset + 4) return null;
    maskKey = buf.subarray(offset, offset + 4);
    offset += 4;
  }

  if (buf.length < offset + payloadLen) return null;

  const payload = Buffer.from(buf.subarray(offset, offset + payloadLen));
  if (masked && maskKey) {
    for (let i = 0; i < payload.length; i++) payload[i] ^= maskKey[i & 3];
  }

  return {
    fin,
    opcode,
    payload,
    consumed: offset + payloadLen,
  };
}

function handleWsFrame(socket, frame) {
  // This stub only supports unfragmented messages.
  if (!frame.fin) {
    sendWsFrame(socket, 0x8, Buffer.alloc(0));
    socket.destroy();
    return;
  }

  switch (frame.opcode) {
    case 0x1: // text
    case 0x2: // binary
      sendWsFrame(socket, frame.opcode, frame.payload);
      return;
    case 0x8: // close
      sendWsFrame(socket, 0x8, frame.payload);
      socket.destroy();
      return;
    case 0x9: // ping
      sendWsFrame(socket, 0xA, frame.payload);
      return;
    case 0xA: // pong
      return;
    default:
      // Unsupported opcode; close.
      sendWsFrame(socket, 0x8, Buffer.alloc(0));
      socket.destroy();
  }
}

function sendWsFrame(socket, opcode, payload) {
  const payloadLen = payload.length;

  let header = null;
  if (payloadLen < 126) {
    header = Buffer.alloc(2);
    header[1] = payloadLen;
  } else if (payloadLen < 65536) {
    header = Buffer.alloc(4);
    header[1] = 126;
    header.writeUInt16BE(payloadLen, 2);
  } else {
    header = Buffer.alloc(10);
    header[1] = 127;
    header.writeUInt32BE(0, 2);
    header.writeUInt32BE(payloadLen >>> 0, 6);
  }

  header[0] = 0x80 | (opcode & 0x0f); // FIN + opcode; server frames are unmasked.

  socket.write(Buffer.concat([header, payload]));
}
