import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  TCP_MUX_SUBPROTOCOL,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
} from "../src/net/tcpMuxProxy.ts";

type NetworkingVectors = {
  schemaVersion: number;
  tcpMux: {
    v1: {
      subprotocol: string;
      frames: {
        open: {
          msgType: number;
          streamId: number;
          host: string;
          port: number;
          metadata: string;
          payloadHex: string;
          frameHex: string;
        };
        data: { msgType: number; streamId: number; payloadHex: string; frameHex: string };
        close: { msgType: number; streamId: number; flags: number; payloadHex: string; frameHex: string };
        error: { msgType: number; streamId: number; code: number; message: string; payloadHex: string; frameHex: string };
      };
    };
  };
};

function loadVectors(): NetworkingVectors {
  const path = new URL("../../tests/protocol-vectors/networking.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8")) as NetworkingVectors;
}

function hexToBytes(hex: string): Uint8Array {
  return new Uint8Array(Buffer.from(hex, "hex"));
}

const vectors = loadVectors();

test("web tcp-mux codec matches shared protocol vectors", () => {
  const v = vectors.tcpMux.v1;
  assert.equal(v.subprotocol, TCP_MUX_SUBPROTOCOL);

  const open = v.frames.open;
  const openPayload = encodeTcpMuxOpenPayload({ host: open.host, port: open.port, metadata: open.metadata });
  assert.deepEqual(openPayload, hexToBytes(open.payloadHex));

  const openFrame = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, open.streamId, openPayload);
  assert.deepEqual(openFrame, hexToBytes(open.frameHex));

  const data = v.frames.data;
  const dataPayload = hexToBytes(data.payloadHex);
  const dataFrame = encodeTcpMuxFrame(TcpMuxMsgType.DATA, data.streamId, dataPayload);
  assert.deepEqual(dataFrame, hexToBytes(data.frameHex));

  const close = v.frames.close;
  const closePayload = encodeTcpMuxClosePayload(close.flags);
  assert.deepEqual(closePayload, hexToBytes(close.payloadHex));
  assert.deepEqual(decodeTcpMuxClosePayload(closePayload), { flags: close.flags });
  const closeFrame = encodeTcpMuxFrame(TcpMuxMsgType.CLOSE, close.streamId, closePayload);
  assert.deepEqual(closeFrame, hexToBytes(close.frameHex));

  const err = v.frames.error;
  const errPayload = hexToBytes(err.payloadHex);
  assert.deepEqual(decodeTcpMuxErrorPayload(errPayload), { code: err.code, message: err.message });
  const errFrame = encodeTcpMuxFrame(TcpMuxMsgType.ERROR, err.streamId, errPayload);
  assert.deepEqual(errFrame, hexToBytes(err.frameHex));
});

test("web TcpMuxFrameParser handles shared vectors across chunk boundaries", () => {
  const v = vectors.tcpMux.v1.frames;
  const stream = new Uint8Array([
    ...hexToBytes(v.open.frameHex),
    ...hexToBytes(v.data.frameHex),
    ...hexToBytes(v.close.frameHex),
    ...hexToBytes(v.error.frameHex),
  ]);

  const parser = new TcpMuxFrameParser();
  const frames: Array<{ msgType: number; streamId: number; payloadHex: string }> = [];

  for (let i = 0; i < stream.byteLength; i++) {
    const chunk = stream.subarray(i, i + 1);
    for (const frame of parser.push(chunk)) {
      frames.push({
        msgType: frame.msgType,
        streamId: frame.streamId,
        payloadHex: Buffer.from(frame.payload).toString("hex"),
      });
    }
  }

  assert.equal(parser.pendingBytes(), 0);
  parser.finish();

  assert.deepEqual(frames, [
    { msgType: TcpMuxMsgType.OPEN, streamId: v.open.streamId, payloadHex: v.open.payloadHex },
    { msgType: TcpMuxMsgType.DATA, streamId: v.data.streamId, payloadHex: v.data.payloadHex },
    { msgType: TcpMuxMsgType.CLOSE, streamId: v.close.streamId, payloadHex: v.close.payloadHex },
    { msgType: TcpMuxMsgType.ERROR, streamId: v.error.streamId, payloadHex: v.error.payloadHex },
  ]);
});

