# Protocol vectors

This directory contains **canonical, shared golden vectors** for Aero’s bytes-on-the-wire
protocols.

These JSON files are used by conformance tests across independent implementations
(Go, TypeScript, JavaScript) for a subset of protocols.

Newer unified, versioned vectors (including auth tokens) live in
`crates/conformance/test-vectors/`.

## Conventions

- All raw bytes are encoded as standard **base64** in fields ending with `_b64`.
- Numeric fields are plain JSON numbers.
- Any embedded secrets (e.g. HMAC keys) are **test-only** and must never be used in production.
- Error cases are represented with:
  - `expectError: true`
  - `errorContains`: substring that **all implementations** must include in the thrown error.

## Files

- `udp-relay.json` — WebRTC DataChannel / WebSocket `/udp` UDP relay framing (v1 + v2).
- `tcp-mux-v1.json` — `aero-tcp-mux-v1` WebSocket multiplexed TCP framing.
- `auth-tokens.json` — gateway session cookie tokens + UDP relay HS256 JWT tokens.
- `l2-tunnel-v1.json` — legacy `aero-l2-tunnel-v1` L2 tunnel framing (FRAME/PING/PONG/ERROR).
  - New canonical cross-language vectors live in
    `crates/conformance/test-vectors/aero-vectors-v1.json` (key: `aero-l2-tunnel-v1`).
- `origin.json` — Browser `Origin` header normalization + allowlist matching semantics.

## Updating vectors

1. Update the protocol spec docs:
   - UDP relay: `proxy/webrtc-udp-relay/PROTOCOL.md`
   - TCP mux: `docs/backend/01-aero-gateway-api.md`
2. Regenerate or edit the vectors to match the new canonical bytes.
3. Run the conformance tests in all implementations (Go + npm workspaces) until green.
