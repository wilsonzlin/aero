import assert from "node:assert/strict";
import test from "node:test";

import { decodeDnsHeader, decodeFirstQuestion, encodeDnsErrorResponse, encodeDnsQuery, encodeDnsResponseA, readDnsRecordHeader, skipQuestions } from "../src/dns/codec.js";

test("encodeDnsQuery roundtrips via decodeFirstQuestion", () => {
  const query = encodeDnsQuery({ id: 0x1234, name: "Example.COM", type: 1 });
  const header = decodeDnsHeader(query);
  assert.equal(header.id, 0x1234);
  assert.equal(header.qdcount, 1);

  const q = decodeFirstQuestion(query);
  assert.equal(q.name, "example.com");
  assert.equal(q.type, 1);
  assert.equal(q.class, 1);
});

test("encodeDnsErrorResponse includes question when provided", () => {
  const question = { name: "example.com", type: 1, class: 1 };
  const resp = encodeDnsErrorResponse({ id: 1, queryFlags: 0x0100, question, rcode: 2 });
  const header = decodeDnsHeader(resp);
  assert.equal(header.id, 1);
  assert.equal(header.qdcount, 1);

  const q = decodeFirstQuestion(resp);
  assert.equal(q.name, "example.com");
  assert.equal(q.type, 1);
  assert.equal(q.class, 1);
});

test("encodeDnsResponseA emits a single A answer record", () => {
  const question = { name: "example.com", type: 1, class: 1 };
  const resp = encodeDnsResponseA({
    id: 0xabcd,
    question,
    answers: [{ name: "example.com", ttl: 60, address: "1.2.3.4" }],
  });

  const header = decodeDnsHeader(resp);
  assert.equal(header.id, 0xabcd);
  assert.equal(header.qdcount, 1);
  assert.equal(header.ancount, 1);

  const q = decodeFirstQuestion(resp);
  assert.equal(q.name, "example.com");

  const answerOffset = skipQuestions(resp, header.qdcount);
  const rr = readDnsRecordHeader(resp, answerOffset);
  assert.equal(rr.type, 1);
  assert.equal(rr.class, 1);
  assert.equal(rr.ttl, 60);
  assert.equal(rr.rdataLength, 4);
  assert.deepEqual(resp.subarray(rr.rdataOffset, rr.rdataOffset + rr.rdataLength), Buffer.from([1, 2, 3, 4]));
  assert.equal(rr.offsetAfter, resp.length);
});

