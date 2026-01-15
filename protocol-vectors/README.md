# Protocol vectors

This directory contains **canonical, shared golden vectors** for Aero’s bytes-on-the-wire
protocols.

These JSON files are used by conformance tests across independent implementations
(Go, TypeScript, JavaScript) for a subset of protocols.

Newer unified, versioned vectors live in `crates/conformance/test-vectors/`.
Some protocols also have additional, more exhaustive vectors here for legacy
consumers and extra negative-case coverage.

## Conventions

- All raw bytes are encoded as standard **base64** in fields ending with `_b64`.
- Numeric fields are plain JSON numbers.
- Any embedded secrets (e.g. HMAC keys) are **test-only** and must never be used in production.
- Error cases are represented with:
  - `expectError: true`
  - Optional `errorContains`: substring that **all implementations** must include in the thrown error (only for APIs that throw, vs returning `null`/`None`).

## Files

- `udp-relay.json` — WebRTC DataChannel / WebSocket `/udp` UDP relay framing (v1 + v2).
- `tcp-mux-v1.json` — `aero-tcp-mux-v1` WebSocket multiplexed TCP framing.
- `auth-tokens.json` — gateway session cookie tokens + UDP relay HS256 JWT tokens.
  - Includes negative vectors for **base64url-no-pad canonicalization** (reject encodings where
    unused bits are non-zero). Some of these are intentionally **correctly signed** so they would
    pass signature validation in a lenient decoder, and therefore strictly test canonicalization.
- `l2-tunnel-v1.json` — legacy `aero-l2-tunnel-v1` L2 tunnel framing vectors (FRAME/PING/PONG/ERROR).
  - Canonical, versioned cross-language vectors live in
    `crates/conformance/test-vectors/aero-vectors-v1.json` (key: `aero-l2-tunnel-v1`).
- `origin.json` — Browser `Origin` header normalization + allowlist matching semantics.

## Auth tokens (format + defensive parsing)

The canonical Rust implementation is `crates/aero-auth-tokens`. These vectors are designed to
prevent cross-language ambiguity and bound attacker-controlled work.

High-level invariants enforced by the shared verifiers:

- **base64url-no-pad**: `A-Z a-z 0-9 - _`, no padding, reject `len % 4 == 1`
- **canonical base64url**: reject encodings where the final quantum has non-zero unused bits
- **strict shape**:
  - session token: `<payload_b64url>.<sig_b64url>`
  - JWT: `<header_b64url>.<payload_b64url>.<sig_b64url>`
  - reject extra delimiters and empty segments
- **coarse size caps** (before any JSON parse):
  - session `payload_b64url` ≤ 16KiB; `sig_b64url` must be exactly **43** chars (HMAC-SHA256)
  - JWT `header_b64url` ≤ 4KiB; `payload_b64url` ≤ 16KiB; `sig_b64url` must be exactly **43** chars
- **verify signature first**, then decode/parse JSON (to avoid attacker-controlled allocations)

## Updating vectors

1. Update the protocol spec docs:
   - UDP relay: `proxy/webrtc-udp-relay/PROTOCOL.md`
   - TCP mux: `docs/backend/01-aero-gateway-api.md`
2. Regenerate or edit the vectors to match the new canonical bytes.
3. Run the conformance tests in all implementations (Go + npm workspaces) until green.
