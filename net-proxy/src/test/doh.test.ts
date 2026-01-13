import test from "node:test";
import assert from "node:assert/strict";
import { startProxyServer } from "../server";

function encodeDnsQuery(name: string, type: number, id: number): Buffer {
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
  out.writeUInt16BE(0x0100, 2); // RD
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
  out.writeUInt16BE(1, offset + 2); // IN
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
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const id = 0x1234;
    const query = encodeDnsQuery("localhost", 1, id);
    const dnsParam = query.toString("base64url");
    const resp = await fetch(`http://127.0.0.1:${proxyAddr.port}/dns-query?dns=${dnsParam}`, {
      headers: { accept: "application/dns-message" }
    });
    assert.equal(resp.status, 200);
    assert.equal(resp.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase(), "application/dns-message");

    const responseBuf = Buffer.from(await resp.arrayBuffer());
    assert.equal(responseBuf.readUInt16BE(0), id);
    // QR bit set, RCODE=0
    assert.equal((responseBuf.readUInt16BE(2) & 0x8000) !== 0, true);
    assert.equal(responseBuf.readUInt16BE(2) & 0x000f, 0);

    // Ensure the question section bytes are echoed verbatim.
    assert.deepEqual(responseBuf.subarray(12, query.length), query.subarray(12));

    const addrs = findAAnswers(responseBuf);
    assert.ok(addrs.length >= 1);
  } finally {
    await proxy.close();
  }
});

test("GET /dns-query rejects malformed base64url input", async () => {
  const proxy = await startProxyServer({ listenHost: "127.0.0.1", listenPort: 0, open: true });
  const proxyAddr = proxy.server.address();
  assert.ok(proxyAddr && typeof proxyAddr !== "string");

  try {
    const resp = await fetch(`http://127.0.0.1:${proxyAddr.port}/dns-query?dns=not!base64`);
    assert.equal(resp.status, 400);
    assert.equal(resp.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase(), "application/dns-message");
    const body = Buffer.from(await resp.arrayBuffer());
    assert.ok(body.length >= 12);
    // FORMERR (1)
    assert.equal(body.readUInt16BE(2) & 0x000f, 1);
  } finally {
    await proxy.close();
  }
});

