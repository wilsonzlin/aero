# Aero cross-language test vectors

This directory contains **canonical, versioned test vectors** that are consumed by multiple
implementations (Rust/TypeScript/Go/Node) to prevent protocol/auth drift.

## `aero-vectors-v1.json`

Top-level fields:

- `version`: the schema version of the vector file (currently `1`).
  - **Do not edit v1 vectors in-place** in a way that would invalidate existing consumers.
  - If you need an incompatible change, add a new file (e.g. `aero-vectors-v2.json`) and bump the
    top-level `version`.

Vector sections:

- `aero-l2-tunnel-v1`: L2 tunnel framing (`magic=0xA2`, `version=0x03`).
  - `valid`: byte-for-byte golden messages (FRAME/PING/PONG/ERROR).
  - `invalid`: malformed messages with expected decode error codes.
  - Bytes are represented as lowercase, unprefixed hex strings (`wireHex`, `payloadHex`).
- `aero_session`: gateway session cookie token (used as the `aero_session` cookie value).
  - Token format: `<payload_b64url>.<sig_b64url>`
  - Signature: HMAC-SHA256 over the **payload base64url string** (not the decoded bytes).
  - Includes fixed `secret`, `nowMs`, and sample tokens (`valid`, `expired`, `badSignature`) along
    with expected claims.
  - More exhaustive negative-case coverage (malformed base64url, missing fields, type mismatches)
    lives in `protocol-vectors/auth-tokens.json` (schema `1`).
- `aero-udp-relay-jwt-hs256`: HS256 JWT tokens used by the WebRTC UDP relay auth mode.
  - Includes fixed `secret`, `nowUnix`, and sample tokens (`valid`, `expired`, `badSignature`) along
    with expected claims.
  - More exhaustive negative-case coverage lives in `protocol-vectors/auth-tokens.json` (schema `1`).

## Consumers

These vectors are referenced by tests in:

- Rust: `crates/aero-l2-protocol/tests/test_vectors.rs`
- Rust: `crates/aero-l2-proxy/tests/auth_vectors.rs` and `crates/aero-l2-proxy/src/server.rs` (unit tests)
- Rust: `crates/aero-auth-tokens/tests/vectors.rs` (auth token verification)
- TypeScript (web): `web/src/shared/l2TunnelProtocol.test.ts`
- TypeScript (web): `web/test/l2TunnelProtocolVectors.test.ts` (Node conformance check)
- Node (gateway): `backend/aero-gateway/src/session.vectors.test.ts` and `backend/aero-gateway/src/udpRelay.vectors.test.ts`
- Node (gateway): `backend/aero-gateway/test/sessionToken.vectors.test.ts` and `backend/aero-gateway/test/udpRelayJwt.vectors.test.ts` (cross-checks)
- Go (relay): `proxy/webrtc-udp-relay/internal/auth/jwt_vectors_test.go`
- Go (relay): `proxy/webrtc-udp-relay/internal/l2tunnel/protocol_test.go`
