import test from "node:test";
import assert from "node:assert/strict";

import { resolveConfig } from "../src/config.js";

test("resolveConfig: ignores invalid numeric env values (strict int parsing + bounds)", () => {
  const baseEnv = {
    AERO_PROXY_TOKEN: "t",
  };

  {
    const cfg = resolveConfig({}, { ...baseEnv, AERO_PROXY_PORT: "-1" });
    assert.equal(cfg.port, 8080);
  }
  {
    const cfg = resolveConfig({}, { ...baseEnv, AERO_PROXY_PORT: "70000" });
    assert.equal(cfg.port, 8080);
  }
  {
    // `parseInt("123abc")` would previously accept 123; require full integer strings now.
    const cfg = resolveConfig({}, { ...baseEnv, AERO_PROXY_PORT: "123abc" });
    assert.equal(cfg.port, 8080);
  }
  {
    const cfg = resolveConfig({}, { ...baseEnv, AERO_PROXY_MAX_WS_MESSAGE_BYTES: "0" });
    assert.equal(cfg.maxWsMessageBytes, 1_048_576);
  }
  {
    const cfg = resolveConfig({}, { ...baseEnv, AERO_PROXY_MAX_WS_MESSAGE_BYTES: String(128 * 1024 * 1024) });
    assert.equal(cfg.maxWsMessageBytes, 1_048_576);
  }
});

