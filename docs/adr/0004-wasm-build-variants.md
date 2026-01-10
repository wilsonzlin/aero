# ADR 0004: WebAssembly build variants (threaded vs single-threaded; runtime selection)

## Context

Aero benefits significantly from multi-threading, but **WASM threads require `SharedArrayBuffer`**, which in turn requires **cross-origin isolation** (ADR 0002). Not all deployment environments can set COOP/COEP headers, and not all browsers/users will run in an isolated context.

We need a strategy that:

- Uses threads where available for performance.
- Still allows the project to run (with reduced performance) without special hosting.

## Decision

Ship **two WebAssembly build variants**:

1. **Threaded build**
   - Compiled with WASM atomics/threads enabled.
   - Uses `WebAssembly.Memory({ shared: true })`.
   - Requires `crossOriginIsolated === true` at runtime.

2. **Single-threaded build**
   - Compiled without threads/atomics.
   - Uses `WebAssembly.Memory({ shared: false })`.
   - Works without cross-origin isolation.

At runtime, select the variant via feature detection:

- `crossOriginIsolated === true`
- `typeof SharedArrayBuffer !== 'undefined'`
- (optionally) validate that shared `WebAssembly.Memory` is constructible

The JS/TS host should present a consistent API to the rest of the app regardless of the selected variant.

## Alternatives considered

1. **Threaded-only**
   - Pros: simplest code path; best performance.
   - Cons: blocks many hosting scenarios; harder to iterate/share demos.

2. **Single-threaded-only**
   - Pros: simplest deployment.
   - Cons: likely cannot meet performance goals.

3. **One build with “optional threads”**
   - Pros: fewer artifacts.
   - Cons: in practice still requires compiling different feature sets; the memory sharing model differs fundamentally.

## Consequences

- Build and CI must produce and test two artifacts.
- Host code must handle runtime selection and expose a stable interface.
- Documentation must clearly state that “threads/SAB are required for the high-performance path” and explain deployment headers.

