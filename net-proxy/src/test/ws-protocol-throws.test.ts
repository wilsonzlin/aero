import test from "node:test";
import assert from "node:assert/strict";

import type { ProxyConfig } from "../config";
import type { ProxyServerMetrics } from "../metrics";
import { handleTcpMuxRelay } from "../tcpMuxRelay";

test("handleTcpMuxRelay does not throw if ws.protocol getter throws", () => {
  let denied = 0;
  const metrics = {
    incConnectionError(reason: string) {
      if (reason === "denied") denied += 1;
    },
  } as unknown as ProxyServerMetrics;

  const cfg = {} as unknown as ProxyConfig;

  const ws = {
    get protocol() {
      throw new Error("boom");
    },
    close() {
      throw new Error("boom");
    },
    terminate() {
      // ignore
    },
  } as any;

  assert.doesNotThrow(() => handleTcpMuxRelay(ws, 1, null, cfg, metrics));
  assert.equal(denied, 1);
});

