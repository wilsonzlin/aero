# aero-gateway property tests

This folder contains **property-based (“fuzz-style”) tests** for security-critical parsing and policy logic.

## Running

```bash
cd backend/aero-gateway
npm install
npm test
```

To run only the property tests:

```bash
npm run test:property
```

## What’s covered

- TCP target parsing (`target=` and `host`/`port`) including IPv6 bracket rules.
- Hostname normalization and wildcard matching (`*.example.com`).
- WebSocket mux frame encoding/decoding:
  - random valid frames round-trip `encode → decode`
  - invalid frames must fail safely (no throw, bounded payload sizes)
- DoH GET `dns=` base64url decoding and message size limits.

The property tests are configured to run quickly in CI (limited runs and per-test timeouts).
For deeper fuzzing locally, increase runs:

```bash
FC_NUM_RUNS=5000 npm test
```

