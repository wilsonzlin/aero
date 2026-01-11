# Protocol vectors

This directory contains **canonical, shared golden vectors** for Aero’s bytes-on-the-wire
protocols.

These JSON files are the single source of truth used by conformance tests across
independent implementations (Go, TypeScript, JavaScript). Any protocol drift should
break CI by failing a vector test.

## Conventions

- All raw bytes are encoded as standard **base64** in fields ending with `_b64`.
- Numeric fields are plain JSON numbers.
- Error cases are represented with:
  - `expectError: true`
  - `errorContains`: substring that **all implementations** must include in the thrown error.

## Files

- `udp-relay.json` — WebRTC DataChannel / WebSocket `/udp` UDP relay framing (v1 + v2).
- `tcp-mux-v1.json` — `aero-tcp-mux-v1` WebSocket multiplexed TCP framing.

## Updating vectors

1. Update the protocol spec docs:
   - UDP relay: `proxy/webrtc-udp-relay/PROTOCOL.md`
   - TCP mux: `docs/backend/01-aero-gateway-api.md`
2. Regenerate or edit the vectors to match the new canonical bytes.
3. Run the conformance tests in all implementations (Go + npm workspaces) until green.

