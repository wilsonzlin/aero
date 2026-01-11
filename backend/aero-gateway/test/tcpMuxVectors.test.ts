import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { describe, it } from "node:test";

import {
  decodeTcpMuxClosePayload,
  decodeTcpMuxErrorPayload,
  decodeTcpMuxOpenPayload,
  encodeTcpMuxClosePayload,
  encodeTcpMuxErrorPayload,
  encodeTcpMuxFrame,
  encodeTcpMuxOpenPayload,
  TcpMuxFrameParser,
  TcpMuxMsgType,
  TCP_MUX_SUBPROTOCOL,
} from "../src/protocol/tcpMux.js";

type NetworkingVectors = {
  tcpMux: {
    v1: {
      subprotocol: string;
      frames: {
        open: {
          msgType: 1;
          streamId: number;
          host: string;
          port: number;
          metadata: string;
          payloadHex: string;
          frameHex: string;
        };
        data: {
          msgType: 2;
          streamId: number;
          payloadHex: string;
          frameHex: string;
        };
        close: {
          msgType: 3;
          streamId: number;
          flags: number;
          payloadHex: string;
          frameHex: string;
        };
        error: {
          msgType: 4;
          streamId: number;
          code: number;
          message: string;
          payloadHex: string;
          frameHex: string;
        };
      };
    };
  };
};

function loadVectors(): NetworkingVectors {
  const path = new URL("../../../tests/protocol-vectors/networking.json", import.meta.url);
  return JSON.parse(readFileSync(path, "utf8")) as NetworkingVectors;
}

function hexToBuf(hex: string): Buffer {
  return Buffer.from(hex, "hex");
}

const vectors = loadVectors();

describe("tcp mux v1 protocol vectors", () => {
  it("encodes frames exactly matching golden bytes", () => {
    const v = vectors.tcpMux.v1;
    assert.equal(v.subprotocol, TCP_MUX_SUBPROTOCOL);

    const open = v.frames.open;
    const openPayload = encodeTcpMuxOpenPayload({ host: open.host, port: open.port, metadata: open.metadata });
    assert.equal(openPayload.toString("hex"), open.payloadHex);
    assert.deepEqual(decodeTcpMuxOpenPayload(openPayload), { host: open.host, port: open.port, metadata: open.metadata });

    const openFrame = encodeTcpMuxFrame(TcpMuxMsgType.OPEN, open.streamId, openPayload);
    assert.equal(openFrame.toString("hex"), open.frameHex);

    const data = v.frames.data;
    const dataPayload = hexToBuf(data.payloadHex);
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

  it("parses a byte stream correctly across arbitrary chunk boundaries", () => {
    const v = vectors.tcpMux.v1.frames;

    const stream = Buffer.concat([
      hexToBuf(v.open.frameHex),
      hexToBuf(v.data.frameHex),
      hexToBuf(v.close.frameHex),
      hexToBuf(v.error.frameHex),
    ]);

    const parser = new TcpMuxFrameParser();
    const frames: Array<{ msgType: number; streamId: number; payloadHex: string }> = [];

    for (let i = 0; i < stream.length; i++) {
      const chunk = stream.subarray(i, i + 1);
      for (const frame of parser.push(chunk)) {
        frames.push({ msgType: frame.msgType, streamId: frame.streamId, payloadHex: frame.payload.toString("hex") });
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
});

