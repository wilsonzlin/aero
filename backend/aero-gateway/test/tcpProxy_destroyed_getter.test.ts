import assert from "node:assert/strict";
import test from "node:test";
import { EventEmitter } from "node:events";

import type http from "node:http";

import { handleTcpProxyUpgrade } from "../src/routes/tcpProxy.js";

test("handleTcpProxyUpgrade does not throw if socket.destroyed getter throws after handshake write fails", async () => {
  let createConnectionCalls = 0;

  class FakeSocket extends EventEmitter {
    write() {
      throw new Error("boom");
    }
    destroy() {
      queueMicrotask(() => this.emit("close"));
    }
    get destroyed() {
      throw new Error("boom");
    }
  }

  const req = {
    url: "/tcp?v=1&host=127.0.0.1&port=80",
    headers: {},
    socket: {},
  } as unknown as http.IncomingMessage;

  const socket = new FakeSocket() as unknown as import("node:stream").Duplex;

  assert.doesNotThrow(() =>
    handleTcpProxyUpgrade(req, socket, Buffer.alloc(0), {
      allowPrivateIps: true,
      handshakeKey: "dGhlIHNhbXBsZSBub25jZQ==",
      createConnection: () => {
        createConnectionCalls += 1;
        throw new Error("should not connect");
      },
    }),
  );

  await new Promise((r) => setImmediate(r));
  assert.equal(createConnectionCalls, 0);
});

