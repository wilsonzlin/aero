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

ws.addEventListener("message", (event) => {
  if (!(event.data instanceof ArrayBuffer)) return;
  const buf = Buffer.from(event.data);

  // Minimal parsing: assumes one protocol frame per WebSocket message.
  if (buf.length < HEADER_BYTES) {
    console.error("short frame");
    return;
  }
  const msgType = buf.readUInt8(0);
  const streamId = buf.readUInt32BE(1);
  const length = buf.readUInt32BE(5);
  const payload = buf.subarray(HEADER_BYTES, HEADER_BYTES + length);

  if (msgType === MsgType.DATA) {
    process.stdout.write(`[stream ${streamId}] ${payload.toString("utf8")}`);
  } else if (msgType === MsgType.ERROR) {
    console.error(`[stream ${streamId}] ERROR`, payload);
  } else {
    console.log(`[stream ${streamId}] msg_type=${msgType} len=${length}`);
  }
});

ws.addEventListener("close", () => {
  console.log("ws closed");
});

ws.addEventListener("error", (err) => {
  console.error("ws error", err);
});
