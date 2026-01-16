import assert from "node:assert/strict";
import test from "node:test";

const gateway = await import(new URL("../backend/aero-gateway/src/routes/wsSubprotocol.ts", import.meta.url));
const tools = await import(new URL("../tools/net-proxy-server/src/wsSubprotocol.js", import.meta.url));

test("wsSubprotocol: gateway and tools implementations match", () => {
  const required = "aero-tcp-mux-v1";

  const cases = [
    { header: undefined },
    { header: "" },
    { header: required },
    { header: ` ${required} ` },
    { header: `chat, ${required}, superchat` },
    { header: ["chat", required] },
    { header: ["chat", ` ${required} `] },
    { header: "a b" }, // invalid token
    { header: ["x".repeat(4096)] }, // totalLen too large
    { header: Array.from({ length: 33 }, (_v, i) => `p${i}`).join(",") }, // too many tokens
    { header: ["ok", 123] }, // non-string in array -> invalid
    { header: `${required}x` }, // partial match not allowed
  ];

  for (const { header } of cases) {
    // tools impl accepts unknown; gateway is typed but runtime works.
    const g = gateway.hasWebSocketSubprotocol(header, required);
    const t = tools.hasWebSocketSubprotocol(header, required);
    assert.deepEqual(t, g, `mismatch for header=${JSON.stringify(header)}`);
  }
});

