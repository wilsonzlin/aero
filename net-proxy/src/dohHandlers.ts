import type http from "node:http";
import dns from "node:dns/promises";
import ipaddr from "ipaddr.js";

import type { ProxyConfig } from "./config";
import { base64UrlPrefixForHeader, decodeBase64UrlToBuffer, maxBase64UrlLenForBytes } from "./base64url";
import { headerHasMimeType } from "./contentType";
import { withTimeout, readRequestBodyWithLimit } from "./httpUtils";
import { sendJsonNoStore, tryWriteResponse } from "./httpResponseSafe";
import { decodeDnsHeader, decodeFirstQuestion, encodeDnsResponse, normalizeDnsName, type DnsAnswer } from "./dnsMessage";
import { stripIpv6ZoneIndex } from "./ipUtils";

const MAX_CONTENT_TYPE_LEN = 256;

function clampInt(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, Math.floor(value)));
}

function sendDnsMessage(res: http.ServerResponse, statusCode: number, message: Buffer): void {
  tryWriteResponse(
    res,
    statusCode,
    {
      "content-type": "application/dns-message",
      "cache-control": "no-store",
      "content-length": message.length
    },
    message
  );
}

export async function handleDnsQuery(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  url: URL,
  config: ProxyConfig
): Promise<void> {
  if (req.method !== "GET" && req.method !== "POST") {
    sendDnsMessage(res, 405, encodeDnsResponse({ id: 0, rcode: 1 }));
    return;
  }

  let query: Buffer;
  let tooLarge = false;
  try {
    if (req.method === "GET") {
      const dnsParam = url.searchParams.get("dns");
      if (!dnsParam) {
        sendDnsMessage(res, 400, encodeDnsResponse({ id: 0, rcode: 1 }));
        return;
      }
      // Avoid decoding arbitrarily large `dns` query params into buffers. For valid base64url,
      // the encoded length is strictly monotonic with decoded byte length, so we can enforce
      // `dohMaxQueryBytes` before decoding the full message.
      const maxEncodedLen = maxBase64UrlLenForBytes(config.dohMaxQueryBytes);
      if (dnsParam.length > maxEncodedLen) {
        tooLarge = true;
        // Best-effort decode of the DNS header (first 12 bytes) so we can preserve query ID/flags
        // in the 413 response without allocating the entire message.
        const prefix = base64UrlPrefixForHeader(dnsParam, 16);
        try {
          query = prefix ? decodeBase64UrlToBuffer(prefix) : Buffer.alloc(0);
        } catch {
          query = Buffer.alloc(0);
        }
      } else {
        query = decodeBase64UrlToBuffer(dnsParam);
      }
    } else {
      if (!headerHasMimeType(req.headers["content-type"], "application/dns-message", MAX_CONTENT_TYPE_LEN)) {
        sendDnsMessage(res, 415, encodeDnsResponse({ id: 0, rcode: 1 }));
        return;
      }
      const bodyResult = await readRequestBodyWithLimit(req, config.dohMaxQueryBytes);
      query = bodyResult.body;
      tooLarge = bodyResult.tooLarge;
    }
  } catch {
    sendDnsMessage(res, 400, encodeDnsResponse({ id: 0, rcode: 1 }));
    return;
  }

  if (tooLarge || query.length > config.dohMaxQueryBytes) {
    let id = 0;
    let queryFlags = 0;
    try {
      const header = decodeDnsHeader(query);
      id = header.id;
      queryFlags = header.flags;
    } catch {
      if (query.length >= 2) id = query.readUInt16BE(0);
      if (query.length >= 4) queryFlags = query.readUInt16BE(2);
    }
    sendDnsMessage(res, 413, encodeDnsResponse({ id, queryFlags, rcode: 1 }));
    return;
  }

  let id = 0;
  let queryFlags = 0;
  try {
    const header = decodeDnsHeader(query);
    id = header.id;
    queryFlags = header.flags;
  } catch {
    if (query.length >= 2) id = query.readUInt16BE(0);
    if (query.length >= 4) queryFlags = query.readUInt16BE(2);
  }

  let question;
  try {
    question = decodeFirstQuestion(query, { maxQnameLength: config.dohMaxQnameLength });
  } catch {
    // FORMERR
    sendDnsMessage(res, 400, encodeDnsResponse({ id, queryFlags, rcode: 1 }));
    return;
  }

  // Only IN is supported.
  if (question.class !== 1) {
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers: [] }));
    return;
  }

  const qtype = question.type;
  // Supported: A (1) and AAAA (28). Other qtypes return NOERROR with no answers.
  if (qtype !== 1 && qtype !== 28) {
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers: [] }));
    return;
  }

  const ttl = clampInt(config.dohAnswerTtlSeconds, 0, config.dohMaxAnswerTtlSeconds);
  const maxAnswers = clampInt(config.dohMaxAnswers, 0, 256);

  let answers: DnsAnswer[] = [];
  try {
    const qname = normalizeDnsName(question.name);
    const family = qtype === 1 ? 4 : 6;
    const resolved = await withTimeout(
      dns.lookup(qname, { family, all: true, verbatim: true }),
      config.dnsTimeoutMs,
      "dns lookup"
    );
    for (const addr of resolved) {
      if (answers.length >= maxAnswers) break;
      try {
        const parsed = ipaddr.parse(stripIpv6ZoneIndex(addr.address));
        const bytes = Buffer.from(parsed.toByteArray());
        if (qtype === 1 && bytes.length !== 4) continue;
        if (qtype === 28 && bytes.length !== 16) continue;
        answers.push({ type: qtype, class: 1, ttl, rdata: bytes });
      } catch {
        // ignore bad addresses
      }
    }

    if (answers.length === 0) {
      sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 2, question: question.wire, answers: [] }));
      return;
    }
  } catch {
    // SERVFAIL (DNS error, not HTTP error)
    sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 2, question: question.wire, answers: [] }));
    return;
  }

  sendDnsMessage(res, 200, encodeDnsResponse({ id, queryFlags, rcode: 0, question: question.wire, answers }));
}

export async function handleDnsJson(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  url: URL,
  config: ProxyConfig
): Promise<void> {
  const rawName = url.searchParams.get("name") ?? "";
  const rawType = url.searchParams.get("type") ?? "A";
  const name = normalizeDnsName(rawName);
  if (!name) {
    sendJsonNoStore(res, 400, { error: "missing name" }, { contentType: "application/json; charset=utf-8" });
    return;
  }
  if (Buffer.byteLength(name, "utf8") > config.dohMaxQnameLength) {
    sendJsonNoStore(res, 400, { error: "name too long" }, { contentType: "application/json; charset=utf-8" });
    return;
  }

  let qtype: number;
  const typeNorm = rawType.trim().toUpperCase();
  if (typeNorm === "A" || typeNorm === "1") {
    qtype = 1;
  } else if (typeNorm === "AAAA" || typeNorm === "28") {
    qtype = 28;
  } else if (typeNorm === "CNAME" || typeNorm === "5") {
    qtype = 5;
  } else {
    sendJsonNoStore(res, 400, { error: "unsupported type" }, { contentType: "application/json; charset=utf-8" });
    return;
  }

  const ttl = clampInt(config.dohAnswerTtlSeconds, 0, config.dohMaxAnswerTtlSeconds);
  const maxAnswers = clampInt(config.dohMaxAnswers, 0, 256);

  let status = 0;
  let answer: Array<{ name: string; type: number; TTL: number; data: string }> = [];
  try {
    if (qtype === 1) {
      const resolved = await withTimeout(
        dns.lookup(name, { family: 4, all: true, verbatim: true }),
        config.dnsTimeoutMs,
        "dns lookup"
      );
      for (const addr of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 1, TTL: ttl, data: addr.address });
      }
    } else if (qtype === 28) {
      const resolved = await withTimeout(
        dns.lookup(name, { family: 6, all: true, verbatim: true }),
        config.dnsTimeoutMs,
        "dns lookup"
      );
      for (const addr of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 28, TTL: ttl, data: addr.address });
      }
    } else {
      const resolved = await withTimeout(dns.resolveCname(name), config.dnsTimeoutMs, "dns cname lookup");
      for (const cname of resolved.slice(0, maxAnswers)) {
        answer.push({ name, type: 5, TTL: ttl, data: cname });
      }
    }
  } catch {
    status = 2; // SERVFAIL
    answer = [];
  }

  sendJsonNoStore(
    res,
    200,
    {
      Status: status,
      TC: false,
      RD: true,
      RA: true,
      AD: false,
      CD: false,
      Question: [{ name, type: qtype }],
      Answer: answer
    },
    { contentType: "application/dns-json; charset=utf-8" }
  );
}

