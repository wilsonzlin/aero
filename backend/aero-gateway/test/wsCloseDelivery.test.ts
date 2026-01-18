import assert from "node:assert/strict";
import net from "node:net";
import { Duplex } from "node:stream";
import test from "node:test";

import { WebSocketTcpBridge } from "../src/routes/tcpBridge.js";
import { WebSocketTcpMuxBridge } from "../src/routes/tcpMuxBridge.js";

function encodeMaskedCloseFrame(code: number): Buffer {
  const payload = Buffer.alloc(2);
  payload.writeUInt16BE(code, 0);
  const maskKey = Buffer.from([0x01, 0x02, 0x03, 0x04]);
  const masked = Buffer.alloc(payload.length);
  for (let i = 0; i < payload.length; i++) masked[i] = payload[i]! ^ maskKey[i % 4]!;
  return Buffer.concat([Buffer.from([0x88, 0x80 | payload.length]), maskKey, masked]);
}

class TestDuplex extends Duplex {
  calls: string[] = [];
  written: Buffer[] = [];

  _read(): void {
    // no-op
  }

  _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    this.calls.push("write");
    this.written.push(Buffer.from(chunk));
    callback();
  }

  override end(...args: Parameters<Duplex["end"]>): this {
    this.calls.push("end");
    // Simulate the underlying socket closing shortly after a graceful end.
    queueMicrotask(() => this.emit("close"));
    return super.end(...args);
  }

  override destroy(error?: Error): this {
    this.calls.push("destroy");
    return super.destroy(error);
  }
}

function assertEchoedCloseFrame(buf: Buffer, expectedCode: number): void {
  assert.ok(buf.length >= 4, "expected a websocket close frame");
  assert.equal(buf[0], 0x88); // FIN + CLOSE
  assert.equal(buf[1], 0x02); // length 2, unmasked
  assert.equal(buf.readUInt16BE(2), expectedCode);
}

test("WebSocketTcpBridge: echoes close frame before ending the upgrade socket", async () => {
  const wsSocket = new TestDuplex();
  const tcpSocket = new net.Socket();
  tcpSocket.on("error", () => {});

  const bridge = new WebSocketTcpBridge(wsSocket, tcpSocket, 1024);
  bridge.start(Buffer.alloc(0));
  wsSocket.resume();

  wsSocket.push(encodeMaskedCloseFrame(1000));

  // `stream.Writable` may invoke `_write` on a later tick; wait for it.
  await new Promise((resolve) => setImmediate(resolve));

  assert.ok(wsSocket.calls.includes("write"), "expected server to write close echo frame");
  assert.ok(wsSocket.calls.includes("end"), "expected server to end upgrade socket after close");
  assert.ok(!wsSocket.calls.includes("destroy"), "did not expect immediate destroy on graceful close");
  assert.ok(wsSocket.calls.indexOf("write") < wsSocket.calls.indexOf("end"), "expected write before end");
  assert.equal(wsSocket.written.length, 1);
  assertEchoedCloseFrame(wsSocket.written[0]!, 1000);
});

test("WebSocketTcpMuxBridge: echoes close frame before ending the upgrade socket", async () => {
  const wsSocket = new TestDuplex();
  const bridge = new WebSocketTcpMuxBridge(wsSocket, { maxMessageBytes: 1024 });
  bridge.start(Buffer.alloc(0));
  wsSocket.resume();

  wsSocket.push(encodeMaskedCloseFrame(1000));

  // `stream.Writable` may invoke `_write` on a later tick; wait for it.
  await new Promise((resolve) => setImmediate(resolve));

  assert.ok(wsSocket.calls.includes("write"), "expected server to write close echo frame");
  assert.ok(wsSocket.calls.includes("end"), "expected server to end upgrade socket after close");
  assert.ok(!wsSocket.calls.includes("destroy"), "did not expect immediate destroy on graceful close");
  assert.ok(wsSocket.calls.indexOf("write") < wsSocket.calls.indexOf("end"), "expected write before end");
  assert.equal(wsSocket.written.length, 1);
  assertEchoedCloseFrame(wsSocket.written[0]!, 1000);
});
