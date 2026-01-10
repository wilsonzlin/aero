import test from "node:test";
import assert from "node:assert/strict";

import {
  UdpRelaySignalingDecodeError,
  parseAnswerResponseJSON,
  parseOfferRequestJSON,
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

