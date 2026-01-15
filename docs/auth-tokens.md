# Auth tokens (formats + strict verification contract)

This repo defines a small set of **cross-language authentication tokens** that are minted/verified by
multiple implementations (Rust/TypeScript/Go). The contract is intentionally strict to avoid:

- cross-language parsing ambiguity
- attacker-controlled allocations / DoS
- “lenient verifier accepts something the strict verifier rejects”

The **canonical negative-case vectors** live in [`protocol-vectors/auth-tokens.json`](../protocol-vectors/auth-tokens.json).

## Canonical implementations

- **Rust (canonical spec + vectors)**: `crates/aero-auth-tokens`
- **Gateway (TypeScript)**: `backend/aero-gateway/src/session.ts` (session cookie token), `backend/aero-gateway/src/udpRelay.ts` (UDP relay JWT minting)
- **WebRTC UDP relay (Go)**: `proxy/webrtc-udp-relay/internal/auth/jwt.go`

## Token formats

### 1) Gateway session token (cookie)

Used by the gateway as the session cookie value (`aero_session`).

- **Format**: `<payload_b64url_no_pad>.<sig_b64url_no_pad>`
- **Signature**: `sig = HMAC_SHA256(secret, payload_b64url_no_pad_ascii_bytes)`
- **`sig_b64url_no_pad` length**: MUST be **43 chars** (32-byte HMAC-SHA256)

Payload JSON (decoded from `payload_b64url_no_pad`):

- Required:
  - `v`: JSON number, MUST be `1` (JS semantics allow `1.0`)
  - `sid`: non-empty string
  - `exp`: JSON number (seconds since unix epoch)

Time semantics:

- Token is expired if \(nowMs \ge exp \times 1000\).

### 2) UDP relay HS256 JWT

Used to authenticate to the UDP relay (WebRTC signaling + `/udp` fallback).

- **Format**: `<header_b64url_no_pad>.<payload_b64url_no_pad>.<sig_b64url_no_pad>`
- **Signature**: `sig = HMAC_SHA256(secret, "<header>.<payload>"_ascii_bytes)`
- **`sig_b64url_no_pad` length**: MUST be **43 chars** (32-byte HMAC-SHA256)

Header JSON (decoded from `header_b64url_no_pad`):

- MUST be an object with `alg: "HS256"`
- `typ` MAY be present; if present it MUST be a string

Payload JSON (decoded from `payload_b64url_no_pad`):

- Required:
  - `sid`: non-empty string
  - `iat`: integer (seconds since unix epoch)
  - `exp`: integer (seconds since unix epoch)
- Optional:
  - `origin`: string
  - `aud`: string
  - `iss`: string
  - `nbf`: integer (not-before; if present and \(nowSec < nbf\), token is not yet valid)

Time semantics:

- Token is expired if \(nowSec \ge exp\).
- If `nbf` is present, token is invalid if \(nowSec < nbf\).

## Strict decoding & verification contract (all token types)

### Base64url canonicalization

All segments are **base64url without padding**:

- Alphabet: `A-Z a-z 0-9 - _`
- No `=` padding
- Reject `len % 4 == 1`
- Reject non-canonical encodings where **unused bits are non-zero** in the final quantum

### Format + size caps

Verifiers MUST:

- require the exact delimiter count (2 parts for session tokens; 3 parts for JWTs)
- reject empty segments
- enforce coarse size caps (token length + per-segment encoded length) **before** decoding
- enforce signature segment length (43 chars for HS256)

### Verification order

To match the canonical hardening (and avoid DoS):

- MUST validate base64url shape/caps first
- SHOULD verify the HMAC signature **before** decoding/parsing JSON
- only if signature is valid, decode JSON and validate claims

## Where to look for exhaustive semantics

- **Vectors**: [`protocol-vectors/auth-tokens.json`](../protocol-vectors/auth-tokens.json)
- **Spec + reference code**: `crates/aero-auth-tokens` crate-level docs (`crates/aero-auth-tokens/src/lib.rs`)
- **Gateway public contract**: [`docs/backend/01-aero-gateway-api.md`](./backend/01-aero-gateway-api.md)

