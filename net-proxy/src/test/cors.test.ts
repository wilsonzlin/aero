import test from "node:test";
import assert from "node:assert/strict";
import type http from "node:http";

import type { ProxyConfig } from "../config";
import { setDohCorsHeaders } from "../cors";

test("setDohCorsHeaders does not throw if res.setHeader throws", () => {
  const cfg = {
    dohCorsAllowOrigins: ["*"],
  } as unknown as ProxyConfig;

  const req = { headers: { origin: "https://example.test" } } as unknown as http.IncomingMessage;
  const res = {
    setHeader() {
      throw new Error("boom");
    },
  } as unknown as http.ServerResponse;

  assert.doesNotThrow(() => setDohCorsHeaders(req, res, cfg));
});

