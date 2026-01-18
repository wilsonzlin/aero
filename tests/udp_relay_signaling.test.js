import test from "node:test";
import assert from "node:assert/strict";

import {
  UdpRelaySignalingDecodeError,
  parseAnswerResponseJSON,
  parseOfferRequestJSON,
  parseSignalMessageJSON,
} from "../web/src/shared/udpRelaySignaling.ts";

test("udp relay signaling v1: parses offer request", () => {
  const offer = parseOfferRequestJSON(JSON.stringify({ version: 1, offer: { type: "offer", sdp: "v=0..." } }));
  assert.equal(offer.version, 1);
  assert.deepEqual(offer.offer, { type: "offer", sdp: "v=0..." });
});

test("udp relay signaling v1: parses answer response", () => {
  const answer = parseAnswerResponseJSON(
    JSON.stringify({ version: 1, answer: { type: "answer", sdp: "v=0..." } }),
  );
  assert.equal(answer.version, 1);
  assert.deepEqual(answer.answer, { type: "answer", sdp: "v=0..." });
});

test("udp relay signaling v1: rejects unsupported versions", () => {
  assert.throws(
    () => parseOfferRequestJSON(JSON.stringify({ version: 2, offer: { type: "offer", sdp: "v=0..." } })),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "unsupported_version",
  );
});

test("udp relay signaling v1: rejects invalid SDP types", () => {
  assert.throws(
    () => parseOfferRequestJSON(JSON.stringify({ version: 1, offer: { type: "answer", sdp: "v=0..." } })),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "invalid_sdp_type",
  );
});

test("udp relay signaling v1: rejects missing SDP", () => {
  assert.throws(
    () => parseOfferRequestJSON(JSON.stringify({ version: 1, offer: { type: "offer", sdp: "" } })),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "missing_sdp",
  );
});

test("udp relay signaling v1: rejects invalid JSON", () => {
  assert.throws(
    () => parseOfferRequestJSON("{"),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "invalid_json",
  );
});

test("udp relay signaling typed: parses offer message", () => {
  const msg = parseSignalMessageJSON(JSON.stringify({ type: "offer", sdp: { type: "offer", sdp: "v=0..." } }));
  assert.deepEqual(msg, { type: "offer", sdp: { type: "offer", sdp: "v=0..." } });
});

test("udp relay signaling typed: parses candidate message", () => {
  const msg = parseSignalMessageJSON(
    JSON.stringify({
      type: "candidate",
      candidate: { candidate: "candidate:1 1 UDP 1234 127.0.0.1 9999 typ host", sdpMid: "0", sdpMLineIndex: 0 },
    }),
  );
  assert.equal(msg.type, "candidate");
  assert.equal(msg.candidate.candidate, "candidate:1 1 UDP 1234 127.0.0.1 9999 typ host");
});

test("udp relay signaling typed: parses error message", () => {
  const msg = parseSignalMessageJSON(JSON.stringify({ type: "error", code: "unauthorized", message: "nope" }));
  assert.deepEqual(msg, { type: "error", code: "unauthorized", message: "nope" });
});

test("udp relay signaling typed: parses auth message (token/apiKey)", () => {
  assert.deepEqual(parseSignalMessageJSON(JSON.stringify({ type: "auth", token: "t" })), { type: "auth", token: "t" });
  assert.deepEqual(parseSignalMessageJSON(JSON.stringify({ type: "auth", apiKey: "k" })), { type: "auth", token: "k" });
});

test("udp relay signaling typed: rejects auth message with mismatched token/apiKey", () => {
  assert.throws(
    () => parseSignalMessageJSON(JSON.stringify({ type: "auth", token: "t", apiKey: "k" })),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "mismatched_token",
  );
});

test("udp relay signaling typed: rejects unknown message types", () => {
  assert.throws(
    () => parseSignalMessageJSON(JSON.stringify({ type: "wat" })),
    (err) => err instanceof UdpRelaySignalingDecodeError && err.code === "unsupported_message_type",
  );
});
