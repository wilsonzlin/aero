import assert from "node:assert/strict";
import test from "node:test";
import { EventEmitter } from "node:events";

import { queryUdpUpstream } from "../src/dns/upstream.js";

class FakeDgramSocket extends EventEmitter {
  closeCalls = 0;

  close() {
    this.closeCalls += 1;
    this.emit("close");
  }

  // Signature matches dgram.Socket#send(message, port, address, cb)
  // We intentionally throw synchronously to simulate close-race / poisoned implementations.
  // eslint-disable-next-line class-methods-use-this
  send() {
    throw new Error("send threw");
  }
}

test("queryUdpUpstream: rejects and closes socket if socket.send throws synchronously", async () => {
  const socket = new FakeDgramSocket();
  const upstream = { kind: "udp" as const, host: "127.0.0.1", port: 53, label: "127.0.0.1:53" };

  await assert.rejects(
    () =>
      queryUdpUpstream(upstream, Buffer.from([0x00, 0x01]), 10_000, {
        // `queryUdpUpstream` should call this once and then handle the send throw.
        createSocket: () => socket as unknown as import("node:dgram").Socket,
      }),
    /send threw/,
  );

  assert.equal(socket.closeCalls, 1);
});

class FakeDgramSocketSendCbError extends EventEmitter {
  closeCalls = 0;

  close() {
    this.closeCalls += 1;
    this.emit("close");
  }

  // eslint-disable-next-line class-methods-use-this
  send(_msg: unknown, _port: unknown, _host: unknown, cb?: (err?: unknown) => void) {
    queueMicrotask(() => cb?.(new Error("send cb error")));
  }
}

test("queryUdpUpstream: rejects and closes socket if send callback receives an error", async () => {
  const socket = new FakeDgramSocketSendCbError();
  const upstream = { kind: "udp" as const, host: "127.0.0.1", port: 53, label: "127.0.0.1:53" };

  await assert.rejects(
    () =>
      queryUdpUpstream(upstream, Buffer.from([0x00, 0x01]), 10_000, {
        createSocket: () => socket as unknown as import("node:dgram").Socket,
      }),
    /send cb error/,
  );

  assert.equal(socket.closeCalls, 1);
});

