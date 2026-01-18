import assert from "node:assert/strict";
import { PassThrough, Duplex } from "node:stream";
import { test } from "node:test";

import { WebSocketTcpBridge } from "../src/routes/tcpBridge.js";

class SpyTcpSocket extends PassThrough {
  pauses = 0;
  resumes = 0;

  override pause(): this {
    this.pauses += 1;
    return super.pause();
  }

  override resume(): this {
    this.resumes += 1;
    return super.resume();
  }
}

test("WebSocketTcpBridge pauses TCP reads when wsSocket is backpressured (and resumes on drain)", async () => {
  /** @type {Buffer[]} */
  const wsWrites = [];
  /** @type {Array<() => void>} */
  const writeCbs = [];

  const wsSocket = new Duplex({
    readableHighWaterMark: 16,
    writableHighWaterMark: 1,
    read() {},
    write(chunk, _enc, cb) {
      wsWrites.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
      writeCbs.push(cb);
      // Don't call cb yet: force backpressure so .write(...) returns false.
    },
  });

  const tcpSocket = new SpyTcpSocket();
  const bridge = new WebSocketTcpBridge(wsSocket as any, tcpSocket as any, {
    maxMessageBytes: 1024 * 1024,
    maxTcpBufferedBytes: 10 * 1024 * 1024,
  });
  bridge.start(Buffer.alloc(0));

  const basePauses = tcpSocket.pauses;
  const baseResumes = tcpSocket.resumes;

  tcpSocket.write(Buffer.alloc(64, 0x61));
  await new Promise((r) => setImmediate(r));

  assert.ok(wsWrites.length >= 1, "expected wsSocket.write to be called");
  assert.equal(
    tcpSocket.pauses,
    basePauses + 1,
    "expected tcpSocket.pause to be called after backpressure",
  );

  const cb = writeCbs.shift();
  assert.equal(typeof cb, "function");
  cb();
  await new Promise((r) => setImmediate(r));

  assert.equal(
    tcpSocket.resumes,
    baseResumes + 1,
    "expected tcpSocket.resume to be called on wsSocket drain",
  );
});

