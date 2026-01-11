import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  TCP_MUX_SUBPROTOCOL,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../tools/net-proxy-server/src/protocol.js";

function loadVectors() {
  const path = new URL("./protocol-vectors/networking.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8"));
}

const vectors = loadVectors();

test("tools/net-proxy-server tcp-mux framing matches shared protocol vectors", () => {
  const v = vectors.tcpMux.v1;
  assert.equal(v.subprotocol, TCP_MUX_SUBPROTOCOL);

  const open = v.frames.open;
  const openPayload = encodeTcpMuxOpenPayload({ host: open.host, port: open.port, metadata: open.metadata });
  assert.equal(openPayload.toString("hex"), open.payloadHex);
  assert.deepEqual(decodeTcpMuxOpenPayload(openPayload), { host: open.host, port: open.port, metadata: open.metadata });
  const openFrame = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, open.streamId, openPayload);
  assert.equal(openFrame.toString("hex"), open.frameHex);

  const data = v.frames.data;
  const dataPayload = Buffer.from(data.payloadHex, "hex");
  const dataFrame = encodeTcpMuxFrame(TcpMuxMsgType.DATA, data.streamId, dataPayload);
  assert.equal(dataFrame.toString("hex"), data.frameHex);

  const close = v.frames.close;
  const closePayload = encodeTcpMuxClosePayload(close.flags);
  assert.equal(closePayload.toString("hex"), close.payloadHex);
  assert.deepEqual(decodeTcpMuxClosePayload(closePayload), { flags: close.flags });
  const closeFrame = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, close.streamId, closePayload);
  assert.equal(closeFrame.toString("hex"), close.frameHex);

  const error = v.frames.error;
  const errorPayload = encodeTcpMuxErrorPayload(error.code, error.message);
  assert.equal(errorPayload.toString("hex"), error.payloadHex);
  assert.deepEqual(decodeTcpMuxErrorPayload(errorPayload), { code: error.code, message: error.message });
  const errorFrame = encodeTcpMuxFrame(TcpMuxMsgType.ERROR, error.streamId, errorPayload);
  assert.equal(errorFrame.toString("hex"), error.frameHex);
});

test("tools/net-proxy-server tcp-mux parser handles shared vectors across chunk boundaries", () => {
  const v = vectors.tcpMux.v1.frames;
  const stream = Buffer.concat([
    Buffer.from(v.open.frameHex, "hex"),
    Buffer.from(v.data.frameHex, "hex"),
    Buffer.from(v.close.frameHex, "hex"),
    Buffer.from(v.error.frameHex, "hex"),
  ]);

  const parser = new TcpMuxFrameParser();
  const frames = [];
  for (let i = 0; i < stream.length; i++) {
    for (const frame of parser.push(stream.subarray(i, i + 1))) {
      frames.push({
        msgType: frame.msgType,
        streamId: frame.streamId,
        payloadHex: frame.payload.toString("hex"),
      });
    }
  }
  assert.equal(parser.pendingBytes(), 0);

  assert.deepEqual(frames, [
    { msgType: TcpMuxMsgType.OPEN, streamId: v.open.streamId, payloadHex: v.open.payloadHex },
    { msgType: TcpMuxMsgType.DATA, streamId: v.data.streamId, payloadHex: v.data.payloadHex },
    { msgType: TcpMuxMsgType.CLOSE, streamId: v.close.streamId, payloadHex: v.close.payloadHex },
    { msgType: TcpMuxMsgType.ERROR, streamId: v.error.streamId, payloadHex: v.error.payloadHex },
  ]);
});

