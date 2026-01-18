import assert from "node:assert/strict";
import test from "node:test";
import { EventEmitter } from "node:events";
import { WebSocketServer } from "ws";

import { createProxyServer } from "../src/server.js";

test("connection handler does not throw on hostile ws.protocol / req.socket.remoteAddress getters", async (t) => {
  /** @type {((ws: any, req: any) => void) | null} */
  let onConnection = null;

  const origOn = WebSocketServer.prototype.on;
  WebSocketServer.prototype.on = function patchedOn(event, listener) {
    if (event === "connection" && typeof listener === "function") {
      onConnection = listener;
    }
    // @ts-ignore - ws typings aren't in scope for this repo-root test
    return origOn.call(this, event, listener);
  };
  t.after(() => {
    WebSocketServer.prototype.on = origOn;
  });

  const proxy = await createProxyServer({
    host: "127.0.0.1",
    port: 0,
    authToken: "test-token",
    allowPrivateIps: true,
    metricsIntervalMs: 0,
    logger() {},
  });
  t.after(async () => {
    await proxy.close();
  });

  assert.equal(typeof onConnection, "function");

  class FakeWs extends EventEmitter {
    OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    send(_data, cb) {
      if (typeof cb === "function") cb(null);
    }
    close() {}
    terminate() {}
    get protocol() {
      throw new Error("boom");
    }
  }

  const fakeReq = {
    socket: {},
  };
  Object.defineProperty(fakeReq.socket, "remoteAddress", {
    get() {
      throw new Error("boom");
    },
  });

  assert.doesNotThrow(() => onConnection(new FakeWs(), fakeReq));
});

