import test from "node:test";
import assert from "node:assert/strict";
import { base64UrlPrefixForHeader, maxBase64UrlLenForBytes } from "../base64url";
import { headerHasMimeType } from "../contentType";
import { startProxyServer } from "../server";
import type { ProxyConfig } from "../config";

async function withProxyServer<T>(
  overrides: Partial<ProxyConfig>,
  fn: (baseUrl: string) => Promise<T>
): Promise<T> {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, ...overrides });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");
  const baseUrl = `http://127.0.0.1:${proxyAddr.port}`;

  try {
    return await fn(baseUrl);
  } finally {
    await proxy.close();
  }
}

function encodeDnsQuery(name: string, type: number, id: number, cls = 1, flags = 0x0100): Buffer {
  const labels = name.split(".").filter(Boolean);
  const nameParts: Buffer[] = [];
  let nameBytes = 1; // trailing 0
  for (const label of labels) {
    const len = Buffer.byteLength(label, "utf8");
    if (len < 1 || len > 63) throw new Error("invalid dns label length");
    nameParts.push(Buffer.from([len]), Buffer.from(label, "utf8"));
    nameBytes += 1 + len;
  }
  nameParts.push(Buffer.from([0]));

  const out = Buffer.allocUnsafe(12 + nameBytes + 4);
  out.writeUInt16BE(id & 0xffff, 0);
  out.writeUInt16BE(flags & 0xffff, 2);
  out.writeUInt16BE(1, 4); // QDCOUNT
  out.writeUInt16BE(0, 6);
  out.writeUInt16BE(0, 8);
  out.writeUInt16BE(0, 10);

  let offset = 12;
  for (const part of nameParts) {
    part.copy(out, offset);
    offset += part.length;
  }
  out.writeUInt16BE(type & 0xffff, offset);
  out.writeUInt16BE(cls & 0xffff, offset + 2);
  return out;
}

function readDnsName(message: Buffer, offset: number): { name: string; nextOffset: number } {
  const labels: string[] = [];
  let jumped = false;
  let nextOffset = offset;
  let guard = 0;

  while (true) {
    if (offset >= message.length) throw new Error("dns name out of bounds");
    if (guard++ > message.length) throw new Error("dns name pointer loop");
    const len = message[offset] ?? 0;

    // Compression pointer.
    if ((len & 0xc0) === 0xc0) {
      if (offset + 1 >= message.length) throw new Error("dns name pointer out of bounds");
      const ptr = ((len & 0x3f) << 8) | (message[offset + 1] ?? 0);
      if (!jumped) {
        nextOffset = offset + 2;
        jumped = true;
      }
      offset = ptr;
      continue;
    }

    if (len === 0) {
      offset += 1;
      if (!jumped) nextOffset = offset;
      break;
    }

    if ((len & 0xc0) !== 0) throw new Error("dns name label has invalid prefix bits");
    offset += 1;
    if (offset + len > message.length) throw new Error("dns name label out of bounds");
    labels.push(message.toString("utf8", offset, offset + len));
    offset += len;
    if (!jumped) nextOffset = offset;
  }

  return { name: labels.join("."), nextOffset };
}

function findAAnswers(response: Buffer): string[] {
  assert.ok(response.length >= 12);
  const qdcount = response.readUInt16BE(4);
  const ancount = response.readUInt16BE(6);

  let offset = 12;
  for (let i = 0; i < qdcount; i += 1) {
    const qname = readDnsName(response, offset);
    offset = qname.nextOffset;
    assert.ok(offset + 4 <= response.length);
    offset += 4;
  }

  const addrs: string[] = [];
  for (let i = 0; i < ancount; i += 1) {
    const nameResult = readDnsName(response, offset);
    offset = nameResult.nextOffset;
    assert.ok(offset + 10 <= response.length);
    const type = response.readUInt16BE(offset);
    const cls = response.readUInt16BE(offset + 2);
    const rdlength = response.readUInt16BE(offset + 8);
    offset += 10;
    assert.ok(offset + rdlength <= response.length);

    if (type === 1 && cls === 1 && rdlength === 4) {
      const addr = response.subarray(offset, offset + 4);
      addrs.push(`${addr[0]}.${addr[1]}.${addr[2]}.${addr[3]}`);
    }

    offset += rdlength;
  }

  return addrs;
}

test("GET /dns-query returns a DNS response with at least one A record", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const id = 0x1234;
    const query = encodeDnsQuery("localhost", 1, id);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`${baseUrl}/dns-query?dns=${dnsParam}`, {
      headers: { accept: "application/dns-message" }
    });
    assert.equal(resp.status, 200);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-message", 256), true);

    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // QR bit set, RCODE=0
    assert.equal((responseBuf.readUInt16BE(2) & 0x8000) !== 0, true);
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 0);

    // Ensure the question section bytes are echoed verbatim.
    assert.deepEqual(responseBuf.subarray(12, query.length), query.subarray(12));

    const addrs = findAAnswers(responseBuf);
    assert.ok(addrs.length >= 1);
  });
});

test("base64url helper bounds match Node's base64url encoding", () => {
  for (const n of [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 31, 32, 33, 255, 256, 257]) {
    const buf = Buffer.alloc(n, 0xab);
    const encoded = buf.toString("base64url");
    assert.equal(encoded.length, maxBase64UrlLenForBytes(n));
  }
});

test("base64UrlPrefixForHeader never returns len%4==1", () => {
  const raw = "a".repeat(128);
  for (let maxChars = 0; maxChars <= 32; maxChars += 1) {
    const prefix = base64UrlPrefixForHeader(raw, maxChars);
    assert.ok(prefix.length <= maxChars);
    assert.ok(prefix.length <= raw.length);
    assert.notEqual(prefix.length % 4, 1);
  }

  // Edge case: tiny max where len%4 would be 1; ensure it trims down to empty.
  assert.equal(base64UrlPrefixForHeader("abcd", 1), "");
});

test("GET /dns-query rejects malformed base64url input", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const resp = await fetch(`${baseUrl}/dns-query?dns=not!base64`);
    assert.equal(resp.status, 400);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-message", 256), true);
    const body = Buffer.from(await resp.arrayBuffer());
    assert.ok(body.length >= 12);
    // FORMERR (1)
    assert.equal(body.readUInt16BE(2) & 0x000f, 1);
  });
});

test("POST /dns-query returns a DNS response with echoed question", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const id = 0x2222;
    const query = encodeDnsQuery("localhost", 1, id);
    const resp = await fetch(`${baseUrl}/dns-query`, {
      method: "POST",
      headers: {
        "content-type": "application/dns-message",
        accept: "application/dns-message"
      },
      body: query
    });
    assert.equal(resp.status, 200);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-message", 256), true);

    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // Ensure the question section bytes are echoed verbatim.
    assert.deepEqual(responseBuf.subarray(12, query.length), query.subarray(12));

    const addrs = findAAnswers(responseBuf);
    assert.ok(addrs.length >= 1);
  });
});

test("POST /dns-query rejects non-application/dns-message content-type", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const query = encodeDnsQuery("localhost", 1, 0x3333);
    const resp = await fetch(`${baseUrl}/dns-query`, {
      method: "POST",
      headers: {
        "content-type": "text/plain",
        accept: "application/dns-message"
      },
      body: query
    });
    assert.equal(resp.status, 415);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-message", 256), true);
    const body = Buffer.from(await resp.arrayBuffer());
    assert.ok(body.length >= 12);
    // FORMERR (1)
    assert.equal(body.readUInt16BE(2) & 0x000f, 1);
  });
});

test("GET /dns-query returns NOERROR with empty answers for unsupported QTYPE", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const id = 0x4444;
    const query = encodeDnsQuery("localhost", 15 /* MX */, id);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`${baseUrl}/dns-query?dns=${dnsParam}`, {
      headers: { accept: "application/dns-message" }
    });
    assert.equal(resp.status, 200);
    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // RCODE=0
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 0);
    assert.equal(responseBuf.readUInt16BE(6), 0); // ANCOUNT
    // Ensure the question section bytes are echoed verbatim.
    assert.deepEqual(responseBuf.subarray(12, query.length), query.subarray(12));
  });
});

test("GET /dns-query enforces dohMaxQueryBytes (413)", async () => {
  await withProxyServer({ open: true, dohMaxQueryBytes: 20 }, async (baseUrl) => {
    const id = 0x5555;
    const queryFlags = 0x1110; // opcode=2 + RD + CD
    const query = encodeDnsQuery("localhost", 1, id, 1, queryFlags);
    assert.ok(query.length > 20);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`${baseUrl}/dns-query?dns=${dnsParam}`);
    assert.equal(resp.status, 413);
    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // FORMERR (1)
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 1);
    const expectedFlags =
      0x8000 | // QR
      (queryFlags & 0x7800) | // opcode
      (queryFlags & 0x0100) | // RD
      0x0080 | // RA
      (queryFlags & 0x0010) | // CD
      0x0001; // FORMERR (rcode=1)
    assert.equal(responseBuf.readUInt16BE(2), expectedFlags);
  });
});

test("POST /dns-query enforces dohMaxQueryBytes (413)", async () => {
  await withProxyServer({ open: true, dohMaxQueryBytes: 20 }, async (baseUrl) => {
    const id = 0x6666;
    const queryFlags = 0x1110; // opcode=2 + RD + CD
    const query = encodeDnsQuery("localhost", 1, id, 1, queryFlags);
    assert.ok(query.length > 20);
    const resp = await fetch(`${baseUrl}/dns-query`, {
      method: "POST",
      headers: { "content-type": "application/dns-message" },
      body: query
    });
    assert.equal(resp.status, 413);
    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // FORMERR (1)
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 1);
    const expectedFlags =
      0x8000 | // QR
      (queryFlags & 0x7800) | // opcode
      (queryFlags & 0x0100) | // RD
      0x0080 | // RA
      (queryFlags & 0x0010) | // CD
      0x0001; // FORMERR (rcode=1)
    assert.equal(responseBuf.readUInt16BE(2), expectedFlags);
  });
});

test("GET /dns-query enforces dohMaxQnameLength (400)", async () => {
  await withProxyServer({ open: true, dohMaxQnameLength: 5 }, async (baseUrl) => {
    const id = 0x7777;
    const query = encodeDnsQuery("localhost", 1, id);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`${baseUrl}/dns-query?dns=${dnsParam}`);
    assert.equal(resp.status, 400);
    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // FORMERR (1)
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 1);
  });
});

test("GET /dns-query returns NOERROR with empty answers for non-IN QCLASS", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const id = 0x8888;
    const query = encodeDnsQuery("localhost", 1, id, 2 /* CS */);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`${baseUrl}/dns-query?dns=${dnsParam}`);
    assert.equal(resp.status, 200);
    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 0);
    assert.equal(responseBuf.readUInt16BE(6), 0); // ANCOUNT
  });
});

test("GET /dns-json returns application/dns-json and at least one A answer for localhost", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const resp = await fetch(`${baseUrl}/dns-json?name=localhost&type=A`, {
      headers: { accept: "application/dns-json" }
    });
    assert.equal(resp.status, 200);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-json", 256), true);
    const body: any = await resp.json();
    assert.equal(body.Status, 0);
    assert.deepEqual(body.Question, [{ name: "localhost", type: 1 }]);
    assert.ok(Array.isArray(body.Answer));
    assert.ok(body.Answer.length >= 1);
    assert.ok(body.Answer.some((a: any) => a.type === 1 && typeof a.data === "string" && /^\d+\.\d+\.\d+\.\d+$/.test(a.data)));
  });
});

test("GET /dns-json supports CNAME queries (Status may be SERVFAIL if no CNAME exists)", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const resp = await fetch(`${baseUrl}/dns-json?name=localhost&type=CNAME`);
    assert.equal(resp.status, 200);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/dns-json", 256), true);
    const body: any = await resp.json();
    assert.deepEqual(body.Question, [{ name: "localhost", type: 5 }]);
    assert.ok(body.Status === 0 || body.Status === 2);
  });
});

test("GET /dns-json rejects unsupported types", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const resp = await fetch(`${baseUrl}/dns-json?name=localhost&type=TXT`);
    assert.equal(resp.status, 400);
    assert.equal(headerHasMimeType(resp.headers.get("content-type"), "application/json", 256), true);
    const body: any = await resp.json();
    assert.equal(body.error, "unsupported type");
  });
});

test("GET /dns-json rejects missing/too-long names", async () => {
  await withProxyServer({ open: true }, async (baseUrl) => {
    const missing = await fetch(`${baseUrl}/dns-json?type=A`);
    assert.equal(missing.status, 400);
    const missingBody: any = await missing.json();
    assert.equal(missingBody.error, "missing name");
  });

  await withProxyServer({ open: true, dohMaxQnameLength: 5 }, async (baseUrl) => {
    const tooLong = await fetch(`${baseUrl}/dns-json?name=localhost&type=A`);
    assert.equal(tooLong.status, 400);
    const tooLongBody: any = await tooLong.json();
    assert.equal(tooLongBody.error, "name too long");
  });
});

test("DoH endpoints support optional CORS allowlist (preflight + response headers)", async () => {
  await withProxyServer({ open: true, dohCorsAllowOrigins: ["http://localhost:5173"] }, async (baseUrl) => {
    const preflight = await fetch(`${baseUrl}/dns-query`, {
      method: "OPTIONS",
      headers: {
        Origin: "http://localhost:5173",
        "Access-Control-Request-Method": "POST",
        "Access-Control-Request-Headers": "content-type",
        "Access-Control-Request-Private-Network": "true"
      }
    });
    assert.equal(preflight.status, 204);
    assert.equal(preflight.headers.get("access-control-allow-origin"), "http://localhost:5173");
    assert.ok((preflight.headers.get("access-control-allow-methods") ?? "").includes("POST"));
    assert.ok((preflight.headers.get("access-control-allow-headers") ?? "").toLowerCase().includes("content-type"));
    assert.equal(preflight.headers.get("access-control-allow-private-network"), "true");
    assert.ok((preflight.headers.get("access-control-expose-headers") ?? "").toLowerCase().includes("content-length"));
    assert.equal(preflight.headers.get("access-control-max-age"), "600");

    const oversizedPreflight = await fetch(`${baseUrl}/dns-query`, {
      method: "OPTIONS",
      headers: {
        Origin: "http://localhost:5173",
        "Access-Control-Request-Method": "POST",
        "Access-Control-Request-Headers": `content-type, ${"x".repeat(10_000)}`
      }
    });
    assert.equal(oversizedPreflight.status, 204);
    assert.equal(oversizedPreflight.headers.get("access-control-allow-origin"), "http://localhost:5173");
    assert.equal(oversizedPreflight.headers.get("access-control-allow-headers"), "Content-Type");

    const jsonPreflight = await fetch(`${baseUrl}/dns-json`, {
      method: "OPTIONS",
      headers: {
        Origin: "http://localhost:5173",
        "Access-Control-Request-Method": "GET"
      }
    });
    assert.equal(jsonPreflight.status, 204);
    assert.equal(jsonPreflight.headers.get("access-control-allow-origin"), "http://localhost:5173");
    assert.ok((jsonPreflight.headers.get("access-control-allow-methods") ?? "").includes("GET"));

    const id = 0x9999;
    const query = encodeDnsQuery("localhost", 1, id);
    const resp = await fetch(`${baseUrl}/dns-query`, {
      method: "POST",
      headers: {
        Origin: "http://localhost:5173",
        "content-type": "application/dns-message",
        accept: "application/dns-message"
      },
      body: query
    });
    assert.equal(resp.status, 200);
    assert.equal(resp.headers.get("access-control-allow-origin"), "http://localhost:5173");
    assert.ok((resp.headers.get("access-control-expose-headers") ?? "").toLowerCase().includes("content-length"));

    const jsonResp = await fetch(`${baseUrl}/dns-json?name=localhost&type=A`, {
      headers: { Origin: "http://localhost:5173", accept: "application/dns-json" }
    });
    assert.equal(jsonResp.status, 200);
    assert.equal(jsonResp.headers.get("access-control-allow-origin"), "http://localhost:5173");
    assert.ok((jsonResp.headers.get("access-control-expose-headers") ?? "").toLowerCase().includes("content-length"));
  });

  await withProxyServer({ open: true, dohCorsAllowOrigins: ["null"] }, async (baseUrl) => {
    const preflight = await fetch(`${baseUrl}/dns-query`, {
      method: "OPTIONS",
      headers: {
        Origin: "null",
        "Access-Control-Request-Method": "POST",
        "Access-Control-Request-Headers": "content-type"
      }
    });
    assert.equal(preflight.status, 204);
    assert.equal(preflight.headers.get("access-control-allow-origin"), "null");
  });

  await withProxyServer({ open: true, dohCorsAllowOrigins: ["http://localhost:5173"] }, async (baseUrl) => {
    const resp = await fetch(`${baseUrl}/dns-query`, {
      method: "POST",
      headers: {
        Origin: "https://evil.example",
        "content-type": "application/dns-message"
      },
      body: encodeDnsQuery("localhost", 1, 0xaaaa)
    });
    assert.equal(resp.status, 200);
    assert.equal(resp.headers.get("access-control-allow-origin"), null);
  });
});
