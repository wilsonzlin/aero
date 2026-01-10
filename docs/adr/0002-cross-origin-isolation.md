# ADR 0002: Cross-origin isolation (COOP/COEP) for threads + SharedArrayBuffer

## Context

Aero’s performance model relies on:

- **WebAssembly threads** (Atomics) for multi-worker execution.
- **SharedArrayBuffer** for shared memory and low-latency signaling.

Modern browsers only enable `SharedArrayBuffer` in a **cross-origin isolated** context to mitigate speculative execution attacks. Cross-origin isolation is established by sending specific response headers on the top-level document *and* all relevant subresources.

## Decision

Support a **threaded build** that requires cross-origin isolation, and document the required deployment headers:

- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`
- `Cross-Origin-Resource-Policy: same-origin` (recommended)
- `Origin-Agent-Cluster: ?1` (recommended)

When these headers are present (and the page is in a secure context), `crossOriginIsolated === true`, enabling `SharedArrayBuffer` and WASM threads.

Also provide a **non-threaded fallback build** for environments where cross-origin isolation is not possible (see ADR 0004).

For the full production header set used by Aero (including CSP with `script-src 'wasm-unsafe-eval'` to support dynamic WASM compilation for the JIT tier),
see [`docs/security-headers.md`](../security-headers.md).

## Alternatives considered

1. **Ship only a non-threaded build**
   - Pros: simplest deployment.
   - Cons: performance ceiling too low for Windows 7-class workloads.

2. **Use `Cross-Origin-Embedder-Policy: credentialless`**
   - Pros: can reduce friction embedding some third-party resources.
   - Cons: compatibility and semantics vary; credentialed requests behave differently; still needs COOP and a secure context.

3. **Avoid shared memory; message-pass everything**
   - Pros: no COOP/COEP requirement.
   - Cons: significantly higher overhead; harder to reach target performance.

## Consequences

- Deployments must be capable of setting COOP/COEP on the **top-level HTML document** (including on cached `304 Not Modified` responses). If the document is not served with COOP/COEP, `crossOriginIsolated` will be `false` and the threaded build cannot run.
- In practice, deployments should set COOP/COEP on **all app-owned responses** (HTML, JS, WASM, worker scripts, etc.) to keep configuration simple and consistent across CDN/proxy layers.
- COEP (`require-corp`) means cross-origin subresources must be:
  - same-origin, or
  - fetched with CORS, or
  - served with a permissive `Cross-Origin-Resource-Policy` from the other origin.
- COOP changes browsing context behavior (e.g., `window.opener` isolation), which can affect popups/auth flows and integration with other sites.
