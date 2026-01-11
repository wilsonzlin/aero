const TCP_MUX_SUBPROTOCOL = "aero-tcp-mux-v1";
const HEADER_BYTES = 9;

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
//   node backend/aero-gateway/examples/tcp-mux-client.js ws://127.0.0.1:8080 127.0.0.1 1234
const [, , gatewayUrl = "ws://127.0.0.1:8080", targetHost = "127.0.0.1", targetPortStr = "7"] = process.argv;
const targetPort = Number.parseInt(targetPortStr, 10);

const ws = new WebSocket(`${gatewayUrl.replace(/^http/, "ws")}/tcp-mux`, TCP_MUX_SUBPROTOCOL);
ws.binaryType = "arraybuffer";

ws.addEventListener("open", () => {
  const streamId = 1;
  ws.send(encodeFrame(MsgType.OPEN, streamId, encodeOpenPayload({ host: targetHost, port: targetPort })));
  ws.send(encodeFrame(MsgType.DATA, streamId, Buffer.from("hello from tcp-mux\n", "utf8")));

  setTimeout(() => {
    ws.send(encodeFrame(MsgType.CLOSE, streamId, Buffer.from([CloseFlags.FIN])));
  }, 250);
});

let pending = Buffer.alloc(0);

ws.addEventListener("message", (event) => {
  if (!(event.data instanceof ArrayBuffer)) return;
  const chunk = Buffer.from(event.data);
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
      ws.send(encodeFrame(MsgType.PONG, streamId, payload));
    } else if (msgType === MsgType.PONG) {
      console.log(`[stream ${streamId}] PONG len=${length}`);
    } else {
      console.log(`[stream ${streamId}] msg_type=${msgType} len=${length}`);
    }
  }
});

ws.addEventListener("close", () => {
  console.log("ws closed");
});

ws.addEventListener("error", (err) => {
  console.error("ws error", err);
});
