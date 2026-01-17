Your job is to thoroughly grok, explore, review, improve and refactor the codebase and system, systematically. This is a complex large task — you can do it fully, without hesitation, without narrowing. Work step by step.

Take full ownership and responsibility over entire codebase. Have full ambition, do not hold back, do not take shortcuts. You have full autonomy and agency to do whatever is in your judgement, taste. Use all your skills, world knowledge. You do not have to care about backwards compatibility, existing code, existing decisions, existing structure. You are free to do anything: big refactors, rip things out, majorly restructure.

Use scratchpad.md as your scratchpad — use it frequently, update before end of turn. Do not commit or push this scratchpad.md file.

This is a complex large-scale engineering project; use good judgement. Don't rush, take step back, do thoughtful elegant design and decisions. Make good engineering decisions: patterns, abstractions, modularity, separation of concerns, no unnecessary coupling, least surface area, testable, composable, self contained. Think of the great engineering patterns.

Simplicity and elegance and beauty is best: faster, easier to get correct, easier to understand and maintain; self contained and self describing. Not just code, but a mindset, worldview: packaging, CI, design, structure, etc. Be bold, modern, fresh, first principles (not just barebones for barebones sake, radical for radical sake, different for different sake, etc.)

Do the hard things. Work autonomously. Take responsibility over entire project — not just code, but docs, comments, organization, structure, etc. Make it fast to compile, easy to test. Avoid strange, weird, frictional patterns, devex. Record the meta: if you've decided on coding standards, design decisions, engineering patterns, etc., document these too for the project and future devs.

Do not do hacky code, fallbacks, shortcuts, workarounds, TODOs, partial implementations, sloppy code.

Make regular commits and push them frequently.

## Refactor plan (phased, test-backed)

### Phase 1: Bound attacker-controlled work (done)
Goal: ensure every network / protocol / cross-worker boundary is defensively parsed and cannot reflect or allocate on untrusted strings without conservative caps.

Outcomes:
- Client-visible error strings are single-line and UTF-8 byte-bounded by default.
- WebSocket upgrade parsing rejects oversized request targets/headers early and deterministically.
- Protocol error payloads are capped consistently (e.g. tcp-mux error message byte cap).
- Helpers that could become “future footguns” (sendText/respondError/etc.) clamp/sanitize defensively even when current call sites are constant.

### Phase 2: Prevent drift with parity/contract tests (done)
Goal: once invariants are in place, ensure they cannot silently drift across implementations.

Outcomes:
- Repo-root parity tests cover shared primitives that are imported by multiple runtimes (Node/TS/JS shims):
  - text sanitization + UTF-8 truncation
  - RFC7230 token validation
  - subprotocol parsing behavior
  - tcp-mux error message byte cap consistency
  - raw header scanning contract semantics

### Phase 3: Module boundaries and test strategy (done)
Goal: reduce “ESM/CJS impedance mismatch” and keep tests aligned with package runtime semantics.

Approach:
- Treat each workspace package as the authority for its own module format.
- Prefer package-local tests for package-internal TS sources when the package is CJS (avoid importing `src/*.ts` directly from repo-root ESM tests).
- Where cross-package parity is needed, compare either:
  - the ESM-exported shared utilities (repo-root `src/*`), or
  - the built artifacts for CJS packages (via their own test runners).

Outcomes:
- A repo-root module boundary contract test prevents “module format accident” imports across test trees.
- Workspace package module formats are explicit where needed (`"type": "module"` vs `"type": "commonjs"`).
- A fast repo-root contract runner (`npm run test:contracts`) exists for quick sanity checks.

### Phase 4: Opportunistic cleanup (done)
Goal: keep surfaces small, remove duplication, and simplify without weakening the invariants above.

Scope:
- Delete dead code and redundant wrappers.
- Tighten error typing and response construction helpers.
- Normalize validation boundaries so “unsafe defaults” cannot reappear.

Outcomes:
- Network/protocol surfaces use stable, single-line, UTF-8 byte-bounded client-visible errors by default.
- Shared parsing/formatting invariants are guarded by parity/contract tests to prevent silent drift.

### Phase 5: Cross-platform drift guards (done)
Goal: catch portability bugs (path separators, platform-specific tooling quirks) early and cheaply.

Approach:
- Keep the contract/parity suite fast enough to run on multiple OSes.
- Add targeted CI jobs when a portability bug is plausible and expensive jobs already exist.

Outcomes:
- A lightweight Windows CI job runs `npm run test:contracts` to exercise the contract/parity suite under Windows path semantics (see `node-contracts-windows` in `.github/workflows/ci.yml`).

### Phase 6: Contract-suite portability hardening (done)
Goal: keep the contract/parity suite correct under both POSIX and Windows path semantics, and make its intent harder to accidentally subvert.

Approach:
- Treat *filesystem paths* and *module specifiers* as distinct domains; normalize explicitly when converting between them.
- Where tests use regex scans (instead of AST parsing), ensure the patterns match both `/` and escaped Windows separators (`\\`) when appropriate.

Outcomes:
- Module-boundary scanning is robust to `..\\workspace\\src\\file.ts`-style specifiers (and resolves `.js` existence checks portably).

### Phase 7: CI action portability (done)
Goal: keep CI steps reliable across OSes by minimizing dependence on runner-specific shells.

Approach:
- Prefer Node scripts over bash for composite action logic (inputs, path resolution, output writing).
- Lock the behavior in with small contract tests so the contract suite exercises CI-critical parsing.

Outcomes:
- `setup-node-workspace` no longer requires bash for version/workspace detection, and its behavior is covered by the contract suite.

### Phase 8: CI action portability sweep (done)
Goal: reduce runner shell coupling across the remaining composite actions in `.github/actions/` (especially for Windows jobs).

Approach:
- Replace bash-only parsing logic with Node scripts (ESM) that explicitly handle paths and write `GITHUB_OUTPUT`/`GITHUB_ENV`.
- Keep scripts dependency-free and make command execution safe (`execFileSync` / `spawnSync` with `shell: false`).
- Add small contract tests for any new CI-parsing logic that would be painful to debug in CI.

Outcomes:
- `setup-rust`, `setup-playwright`, and `resolve-wasm-crate` no longer require bash for their internal logic.

### Phase 9: CI action script consolidation (done)
Goal: keep CI action scripts small and consistent by deduplicating common “GitHub IO” utilities (outputs/env/path normalization) without changing behavior.

Approach:
- Add a tiny shared helper module under `.github/actions/_shared/` for:
  - `GITHUB_OUTPUT` / `GITHUB_ENV` append helpers
  - error formatting + exit behavior
  - small path normalization helpers used across actions
- Refactor action-local scripts to import these helpers instead of reimplementing them.

Outcomes:
- CI action scripts share a single, well-tested implementation for output/env writing and path normalization.

### Phase 10: Composite action cwd robustness (done)
Goal: ensure composite actions work regardless of step working directory by avoiding repo-relative script paths.

Approach:
- Use `${{ github.action_path }}` when invoking action-local scripts so paths are cwd-independent.
- Add a contract test to prevent regressions (no `node .github/actions/...` in composite actions).

Outcomes:
- All composite actions invoke action-local scripts via `github.action_path`.

### Phase 11: Contract test helper consolidation (done)
Goal: keep the contract suite easy to maintain by deduplicating common test utilities.

Approach:
- Add a small `tests/_helpers` module for:
  - running Node scripts with env overrides (for action script contract tests)
  - parsing `GITHUB_OUTPUT`/`GITHUB_ENV` key/value files
- Refactor existing action contract tests to use the helpers (no behavior changes).

Outcomes:
- Action contract tests share one implementation for “run Node script” + “parse key/value output files”, reducing drift.

### Phase 12: CI action defensive execution (done)
Goal: avoid CI hangs and resource spikes by bounding “helper” command execution inside action scripts.

Approach:
- Add small `_shared` helpers for spawning subprocesses with timeouts / bounded buffers.
- Apply timeouts to detection/dry-run helpers where long runtimes indicate a hang (not real work).
- Add contract tests for parsing logic that depends on subprocess output (e.g. Playwright dry-run parsing).

Outcomes:
- Action scripts use timeouts for detection/dry-run subprocesses and have contract coverage for their parsing logic.

### Phase 13: Guardrail hardening (done)
Goal: keep the repo resilient to accidental regressions by expanding “cheap, high-signal” contracts.

Approach:
- Make CI guardrail tests discover targets automatically (e.g. scan all composite actions) so new additions inherit the guardrails by default.
- Keep rules conservative: flag only patterns that are known to be brittle (e.g. repo-relative action script paths).

Outcomes:
- The composite-action path contract automatically covers all actions under `.github/actions/`.

### Phase 14: Composite action shell guardrails (done)
Goal: prevent reintroducing brittle cross-OS patterns in composite actions (especially for Windows runners).

Approach:
- Add a contract test that scans all `.github/actions/**/action.yml` files and forbids:
  - `shell: bash` (forces a non-default shell on Windows)
  - heredoc-style inline scripts (`<<'NODE'`, etc.) that are shell-dependent

Outcomes:
- Composite actions remain shell-agnostic by default; regressions are caught by the contract suite.

### Phase 15: Guardrail test deduplication (done)
Goal: keep the guardrail contract tests concise by sharing common “action discovery” logic.

Approach:
- Add `tests/_helpers/github_actions_contract_helpers.js` to centralize discovery of `.github/actions/**/action.yml`.
- Refactor guardrail tests to use the helper.

Outcomes:
- Guardrail tests share a single implementation for composite action discovery.

### Phase 16: GitHub action output hardening (done)
Goal: reduce subtle CI breakage by centralizing correct `GITHUB_OUTPUT` multiline writing semantics.

Approach:
- Add shared helpers for multiline outputs (delimiter handling) in `.github/actions/_shared`.
- Update action scripts to use the shared helper instead of hand-rolled delimiter formatting.
- Lock in behavior with contract tests.

Outcomes:
- Action scripts use shared multiline output helpers, and contract tests validate delimiter formatting.

### Phase 17: GitHub action output/env correctness guards (done)
Goal: prevent accidental invalid writes to `GITHUB_OUTPUT` / `GITHUB_ENV` that can silently corrupt downstream steps.

Approach:
- Reject newline-containing values for single-line output/env helpers (`appendOutput`/`appendEnv`) and provide clear guidance to use multiline helpers.
- Add `appendMultilineEnv` to match `appendMultilineOutput`.
- Add contract coverage by spawning a node process to observe exit behavior (since helpers terminate on invalid inputs).

Outcomes:
- Shared helpers enforce correct GitHub file command formats and are covered by contract tests.

### Phase 18: Action subprocess helper consolidation (done)
Goal: keep composite action scripts consistent and defensive by centralizing subprocess execution patterns.

Approach:
- Extend `.github/actions/_shared/exec.mjs` with helpers for invoking Node-based CLIs with consistent encoding/timeouts.
- Refactor action scripts to use the shared helpers rather than ad-hoc `execFileSync` calls.

Outcomes:
- Action scripts share one implementation for “run Node CLI, capture UTF-8 output” and “run Node CLI with inherited stdio”.

### Phase 19: JS eval sink guardrails (done)
Goal: prevent accidental introduction of JavaScript eval sinks (`eval`, `new Function`) in production code paths.

Approach:
- Add a contract test that scans the repo’s JS/TS sources (excluding tests) for eval sinks.
- Keep the patterns conservative and focused on real sinks (direct `eval(` / `globalThis.eval(` / `new Function(`).

Outcomes:
- Contract suite fails if eval sinks appear in production code.

### Phase 20: DOM XSS sink guardrails (done)
Goal: prevent accidental introduction of unsafe DOM HTML injection patterns in production code.

Approach:
- Add a contract test that scans production JS/TS sources (excluding tests) for common XSS sinks:
  - `dangerouslySetInnerHTML`
  - `.innerHTML`, `.outerHTML`, `.insertAdjacentHTML`
- Mask strings/comments to avoid false positives from embedded text.

Outcomes:
- Contract suite fails if new DOM HTML injection sinks appear in production sources.

### Phase 21: JS source-scan consolidation + subprocess guardrails (done)
Goal: reduce duplicated source-scanner logic while adding a guardrail against unsafe subprocess APIs.

Approach:
- Add `tests/_helpers/js_source_scan_helpers.js` to centralize production JS/TS file discovery and string/comment masking.
- Refactor existing JS guardrail tests to use the helper.
- Add a new contract test that forbids `child_process.exec/execSync` imports and `shell: true` in production JS/TS sources.

Outcomes:
- JS guardrail tests share a single scanner implementation, and the contract suite blocks unsafe subprocess APIs in production sources.

### Phase 22: SQL injection guardrails (done)
Goal: prevent accidental introduction of known-unsafe Prisma raw query APIs.

Approach:
- Add a contract test that scans production JS/TS sources for Prisma unsafe raw query methods:
  - `queryRawUnsafe`, `executeRawUnsafe`, `$queryRawUnsafe`, `$executeRawUnsafe`
- Mask strings/comments to avoid false positives from documentation text.

Outcomes:
- Contract suite fails if Prisma unsafe raw query APIs appear in production sources.

### Phase 23: Expand DOM XSS sink guardrails (done)
Goal: broaden DOM sink guardrails to cover additional classic HTML injection APIs.

Approach:
- Extend the DOM XSS contract to forbid:
  - `document.write` / `document.writeln`
  - `Range.createContextualFragment`
- Keep masking of strings/comments to avoid false positives.

Outcomes:
- Contract suite blocks these additional DOM HTML injection sinks in production sources.

### Phase 24: JS source-scan correctness hardening (done)
Goal: make the JS/TS source masking helper more faithful to real JS syntax so security guardrails don’t miss code due to lexer edge cases.

Approach:
- Harden `stripStringsAndComments` to correctly handle:
  - Regex literals (so `/\//` doesn’t get misread as a `//` comment)
  - Nested strings/comments/regex inside template expressions (`\`${ ... }\``) without losing template-expression context
- Add focused contract coverage for these edge cases.

Outcomes:
- `tests/js_source_scan_helpers_contract.test.js` locks in masking behavior for regex + template-expression cases.

### Phase 25: Subprocess sink guardrail correctness (done)
Goal: ensure the subprocess sink contract actually detects forbidden `child_process` exec/execSync patterns without being defeated by string masking.

Approach:
- Refactor the subprocess sink scan to:
  - Use masked code only to find candidate `import`/`require` tokens (avoids matches in strings/comments/regex).
  - Parse the module specifier from the original source to correctly detect `"child_process"` / `"node:child_process"`.
- Add focused parsing contract coverage for the sink scanner.

Outcomes:
- `tests/js_subprocess_sinks_parsing_contract.test.js` ensures `import { exec } from "child_process"` and `require("child_process").execSync(` are detected.

### Phase 26: JS eval sink guardrail expansion (done)
Goal: cover additional eval-equivalent sinks beyond `eval()` and `new Function()`.

Approach:
- Extend the eval sink contract to also forbid `Function()` calls (same semantics as `new Function()`).

Outcomes:
- Contract suite fails if `Function(` appears in production sources (outside of allowlisted fixtures).

### Phase 27: Subprocess sink guardrail expansion (done)
Goal: catch additional common ways of reaching `child_process.exec/execSync` beyond the direct import/require call patterns.

Approach:
- Extend the subprocess sink scanner to detect:
  - Namespace/default `child_process` aliases (e.g. `import * as cp from "child_process"; cp.exec(...)`)
  - CommonJS aliases assigned from `require("child_process")`
  - Destructuring `exec` / `execSync` from `require("child_process")`
- Add focused parsing contracts to lock in these cases and prevent regressions.

Outcomes:
- Contract suite fails if `child_process.exec/execSync` is reachable via alias/namespace/destructuring patterns.

### Phase 28: Timer-string eval sink guardrails (done)
Goal: prevent accidental reintroduction of string-based timer eval sinks.

Approach:
- Extend the eval sink scan to detect:
  - `setTimeout("...")` / `setInterval("...")`
  - `window.setTimeout("...")` / `globalThis.setTimeout("...")` variants
- Add focused parsing contract coverage.

Outcomes:
- Contract suite fails if string-based timer eval sinks appear in production sources.

### Phase 29: Sink scanner helper consolidation (done)
Goal: reduce drift between security sink scanners by sharing a tiny “parse JS around a match” utility layer.

Approach:
- Add `tests/_helpers/js_scan_parse_helpers.js` for common utilities:
  - whitespace/comment skipping
  - parsing quoted string literals
- Refactor sink scanners to use the shared helper.

Outcomes:
- Sink scanner helpers share one implementation for basic parsing primitives, reducing future drift.

### Phase 30: DOM XSS sink guardrail completeness (done)
Goal: ensure DOM sink guardrails catch bracket-notation access (`obj["innerHTML"]`) in addition to dot access.

Approach:
- Add a shared DOM XSS sink scanner helper that:
  - uses masked code to locate bracket expressions outside strings/comments/regex
  - parses the string literal property name from source
- Refactor the DOM XSS contract to use the shared helper and add focused parsing contract coverage.

Outcomes:
- Contract suite fails if bracket-notation DOM HTML injection sinks appear in production sources.

### Phase 31: Bracket-notation eval/timer sink coverage (done)
Goal: prevent bypassing eval/timer sink guardrails via bracket notation (e.g. `globalThis["eval"]`).

Approach:
- Extend eval sink scanning to detect:
  - `globalThis["eval"](...)` / `window["eval"](...)`
  - `globalThis["setTimeout"]("...")` / `window["setInterval"]("...")` (string first-arg only)
- Add focused parsing contract coverage.

Outcomes:
- Contract suite fails if bracket-notation global eval/timer sinks appear in production sources.

### Phase 32: Sink scanner string-literal hardening (done)
Goal: prevent bypassing sink scanners via JS string literal escape tricks while keeping scans conservative (no full parser).

Approach:
- Harden the shared string-literal parser used by sink scanners to correctly interpret:
  - common escape sequences (`\xNN`, `\uNNNN`, `\u{...}`, line continuations)
  - no-substitution template literals (`` `...` ``) for bracket keys / dynamic import specifiers
  - JS line terminators (LF/CR/CRLF and U+2028/U+2029) in quoted strings and line-comment termination
- Add focused contract coverage to lock in parsing behavior and real bypass cases.

Outcomes:
- Contract suite catches escaped and no-subst-template variants like:
  - `document["wr\u0069te"]`, `globalThis[\`eval\`]`, `require("child\x5fprocess")`
- Contract suite treats U+2028/U+2029 as line terminators for both the source masker and parse helpers.
- Contract suite treats CR/CRLF as line terminators for `//` comments and line numbering.
- Shared parse helper behavior is guarded by contract tests to prevent drift.

### Phase 33: DOM bracket-sink false-positive hardening (done)
Goal: keep the DOM XSS guardrail high-signal by reducing obvious false positives from array literals without weakening real sink detection.

Approach:
- Keep bracket-notation sink detection focused on computed property access like `obj["innerHTML"]`.
- Avoid flagging array literals that merely contain sink-like strings, including common statement shapes:
  - `return ["innerHTML"]`
  - `if (x) ["innerHTML"];` (and similar control-flow headers)
- Lock in the expected behavior with focused contract coverage.

Outcomes:
- Contract suite does not fail on array literals containing sink-like strings.
- Contract suite still fails on computed-property sinks like `el["innerHTML"] = ...`.

### Phase 34: Subprocess sink reference-taking hardening (done)
Goal: prevent bypassing the subprocess sink guardrail by taking references to `child_process.exec/execSync` without calling them inline.

Approach:
- Extend the subprocess sink scanner to flag:
  - property access via child_process aliases (e.g. `cp.exec`, `cp["execSync"]`)
  - property access via direct `require("child_process").exec` / `["execSync"]`
- Keep detection scoped to confirmed `child_process` namespaces/specifiers to avoid false positives.
- Add focused contract coverage.

Outcomes:
- Contract suite fails if `child_process.exec/execSync` is reachable via reference-taking patterns, not just calls/destructuring.

### Phase 35: Awaited dynamic import subprocess sink closure (done)
Goal: prevent bypassing the subprocess sink guardrail via awaited dynamic import member access (e.g. `(await import("child_process")).execSync(...)`).

Approach:
- Extend the subprocess sink scanner to detect `await import("child_process")` followed by:
  - direct member access (dot or bracket), including optional chaining
  - `default` hop patterns where Node ESM interop exposes `child_process` as `module.default`
- Add focused parsing contract coverage for the bypass shapes.

Outcomes:
- Contract suite fails if `child_process.exec/execSync` is reachable via awaited dynamic import member access.

### Phase 36: Unicode-escaped identifier sink hardening (done)
Goal: prevent bypassing sink guardrails via unicode escapes in identifier names (e.g. `document.wr\u0069te(...)`, `globalThis.e\u0076al(...)`, `cp.e\u0078ec(...)`).

Approach:
- Add a conservative identifier parser for `\uXXXX` / `\u{...}` escapes (ASCII-only) for use in sink scanners (no full JS parser).
- Extend sink scanners to detect unicode-escaped identifier forms for:
  - DOM dot-access sinks (`innerHTML`, `outerHTML`, `insertAdjacentHTML`, `createContextualFragment`, and `document.write/writeln`)
  - global eval/timer sinks (`globalThis.e\u0076al`, `window.setTime\u006fut("...")`)
  - subprocess sinks (`cp.e\u0078ec`, `require("child_process").e\u0078ecSync`)
- Add focused parsing contracts for these bypass shapes.

Outcomes:
- Contract suite catches the unicode-escaped identifier bypass patterns above without expanding into a full JS lexer/parser.

### Phase 37: Eval/Function reference-taking hardening (done)
Goal: prevent bypassing eval-sink guardrails via taking references to global eval/Function (e.g. `const e = globalThis["eval"]`, `globalThis["Function"]("...")`).

Approach:
- Extend eval sink scanning to treat global-object dot/bracket access of:
  - `eval`
  - `Function`
  as sinks even when not immediately called (reference-taking), including unicode-escaped identifiers and escaped bracket-string properties.
- Add focused parsing contract coverage to lock in these bypass shapes.

Outcomes:
- Contract suite catches `globalThis.eval`, `globalThis["eval"]`, `globalThis["Function"]`, and unicode-escaped variants.

### Phase 38: Unicode-escaped direct eval/timer identifier hardening (done)
Goal: prevent bypassing eval/timer guardrails via unicode-escaped **direct** identifiers (e.g. `ev\u0061l(...)`, `setTime\u006fut("...")`).

Approach:
- Extend eval sink scanning to detect unicode-escaped direct identifier calls for:
  - `eval(...)`
  - `Function(...)`
  - `setTimeout("...")` / `setInterval("...")` (string first arg only)
- Keep it conservative: only treat direct identifiers (not `.prop` access) and only when the raw identifier text includes a `\u` escape.
- Add focused parsing contract coverage for these cases.

Outcomes:
- Contract suite catches direct-call unicode-escape bypasses without broadening the scanner into a full JS parser.

### Phase 39: Timer optional-call hardening (done)
Goal: prevent bypassing timer-string eval guardrails via optional-call syntax (e.g. `setTimeout?.("...")`, `window.setTimeout?.("...")`).

Approach:
- Update timer-string sink scanning to use `isOptionalCallStart` so both `(... )` and `?.( ... )` forms are recognized.
- Keep the guardrail conservative by requiring a literal string/template first argument.
- Add focused contract coverage for optional-call variants.

Outcomes:
- Contract suite catches `setTimeout?.("...")`, `setInterval?.("...")`, and `window.setTimeout?.("...")` string-timer sinks.

### Phase 40: HTTP error reflection guardrail (done)
Goal: prevent accidental reintroduction of client-visible error reflection in HTTP response bodies (large/untrusted `err.message` values leaking to clients or bloating logs).

Approach:
- Add a contract test that scans production JS/TS sources for high-risk response patterns:
  - `res.send(err.message)` / `reply.send(err.message)`
  - `res.end(String(err))` / `reply.end(String(error))`
  - `socket.end(err.message)` in ad-hoc HTTP responders
- Keep the scan conservative (only `err`/`error` variables; ignore strings/comments).
- Add a focused parsing contract test to lock in scanner correctness (detect sinks, avoid false positives from nearby `catch (err)` blocks).

Outcomes:
- Contract suite fails if new direct `err.message` / `String(err)` response-body reflection patterns appear in production sources.
- Scanner is statement-local: it inspects the matched call’s argument expression (parentheses-balanced) to avoid false positives from nearby `catch (err)` blocks.
- The guardrail also flags status-line reflection via `res.writeHead(status, err.message)` and JSON-body reflection via `JSON.stringify(err)` passed directly to response writers.
- The guardrail is chain-aware: it also covers fluent forms like `reply.code(...).send(...)` and `reply.raw.end(...)`.

### Phase 41: De-duplicate ESM text helpers (done)
Goal: reduce redundant copies of the ESM (JS) text helpers while keeping behavior stable and test-locked.

Approach:
- Make `server/src/text.js` and `tools/net-proxy-server/src/text.js` re-export from the canonical `src/text.js`.
- Keep the existing text parity/contract tests as the drift guard.

Outcomes:
- Removes large duplicated implementations without changing any public behavior (`npm run test:contracts` remains green).

### Phase 42: HTTP error reflection optional-chain hardening (done)
Goal: prevent bypassing the HTTP error reflection guardrail with optional chaining / bracket access on `err`/`error` (e.g. `res.send(err?.message)`, `reply.send(err?.["message"])`).

Approach:
- Extend HTTP response sink scanning to treat `err?.message` and `err?.["message"]` as equivalent to `err.message` when used in response writers.
- Add focused parsing contract coverage for these bypass shapes.

Outcomes:
- Contract suite fails if new optional-chaining / bracket-access error-message reflection patterns appear in production sources.

### Phase 43: HTTP error reflection response-method bracket hardening (done)
Goal: prevent bypassing the HTTP error reflection guardrail via bracket-notation response methods (e.g. `res["send"](err.message)`, `reply?.["send"]?.(err.message)`).

Approach:
- Extend the chain-aware response-call scanner to parse bracket string properties in receiver chains, so `res["send"](...)` and `reply["json"](...)` are scanned like `res.send(...)`.
- Add focused parsing contract coverage for bracket-method variants.

Outcomes:
- Contract suite catches bracket-method response sink bypasses without broadening into full dataflow.

### Phase 44: De-duplicate browser public text helpers (done)
Goal: remove duplicated `formatOneLineError` / UTF-8 one-line formatting helpers in `web/public` scripts while preserving behavior (and keeping CSP fixtures intact).

Approach:
- Add a small shared ESM module in `web/public/_shared/` for one-line UTF-8 formatting and safe error-to-message conversion (optionally including `err.name` fallback).
- Replace duplicated implementations in:
  - `web/public/wasm-jit-csp/main.js`
  - `web/public/assets/security_headers_worker.js`

Outcomes:
- `web/public` scripts stay self-contained (no bundler required) but no longer carry multiple copies of the same helper logic.

### Phase 45: Snapshot UI integration hygiene (done)
Goal: de-duplicate the ad-hoc snapshot UI script and make it usable as a first-class Vite-served page.

Approach:
- Convert `web/snapshot-ui.js` to use the shared `web/public/_shared/text_one_line.js` helper.
- Add `web/snapshot-ui.html` that wires up the expected DOM and loads the script as a module.
- Extend the shared helper to support a `includeNameFallback: "missing"` mode so we can preserve prior semantics where `err.name` is only used when `.message` is missing (not merely empty).

Outcomes:
- Snapshot UI is now a runnable page (`/snapshot-ui.html`) and no longer carries a private copy of one-line error formatting helpers.

### Phase 46: De-duplicate PoC Node server text helpers (done)
Goal: reduce duplicated one-line error formatting helpers in small Node ESM PoC servers.

Approach:
- For ESM PoC servers, import `formatOneLineError` from the canonical `src/text.js` instead of carrying a local `TextEncoder` + UTF-8 truncation implementation.

Outcomes:
- `poc/browser-memory/server.mjs` now uses `src/text.js` for safe, byte-bounded error messages without duplicating the implementation.

### Phase 47: De-duplicate CJS helper copies (done)
Goal: remove duplicated UTF-8 one-line text helpers across CJS utilities without forcing a full CJS→ESM migration.

Approach:
- Introduce a small shared CJS helper: `scripts/_shared/text_one_line.cjs`.
- Replace local helper copies in:
  - `web/scripts/serve.cjs`
  - `tools/disk-streaming-browser-e2e/src/servers.js`
- Add the new CJS helper to the existing text parity tests to prevent drift.

Outcomes:
- CJS utilities share one implementation for `formatOneLineUtf8`/`formatOneLineError`, and parity tests keep it aligned with the canonical `src/text.js`.

### Phase 48: De-duplicate perf dashboard text helpers (done)
Goal: remove duplicated one-line UTF-8 error formatting helpers from the nightly performance dashboard without breaking the gh-pages artifact.

Approach:
- Convert the dashboard script to ESM and import a shared helper from `web/public/_shared/text_one_line.js`.
- Add a local shim module under `bench/dashboard/_shared/` for repo-local serving, and ensure the perf-nightly workflow copies the canonical helper into the published `dist/perf-dashboard/_shared/` so the artifact remains self-contained.

Outcomes:
- `bench/dashboard/app.js` no longer embeds a local UTF-8 one-line formatting implementation; the published dashboard includes the shared helper file.

### Phase 49: HTTP error reflection parenthesized err hardening (done)
Goal: prevent bypassing the HTTP error reflection guardrail via parenthesized `err`/`error` message access (e.g. `res.send((err).message)`, `res.send((err)["message"])`).

Approach:
- Extend the response-argument scan patterns to also catch parenthesized `err`/`error` message access forms while staying conservative (avoid call-argument false positives like `foo(err).message`).
- Add focused parsing contract coverage for these bypass shapes.

Outcomes:
- Contract suite catches the parenthesized error-message reflection bypass shapes without expanding into full dataflow analysis.

### Phase 50: HTTP error reflection bracket-string correctness (done)
Goal: correctly detect `err["message"]` / `err?.["message"]` style sinks despite the scanner masking string literal contents.

Approach:
- Replace regex-based `["message"]` matching with a small parser-based check that scans only masked (non-string/comment) regions, then parses bracket-string properties from the original source and compares the decoded property value to `"message"`.
- Add focused parsing contract coverage for `err?.['message']`, `err['message']`, and escaped string forms like `err["m\\u0065ssage"]`.

Outcomes:
- Contract suite now genuinely enforces bracket-string error message reflection sinks (including escaped string literals) instead of relying on masked string contents.

### Phase 51: HTTP error reflection dot unicode-escape hardening (done)
Goal: prevent bypassing the HTTP error reflection guardrail with unicode-escaped identifier member access (e.g. `err.m\u0065ssage`, `(err).m\u0065ssage`).

Approach:
- Add a small parser-based check that looks for `err`/`error` followed by `.` (or `?.`) and parses the following identifier with unicode escapes, treating a decoded `"message"` property as a sink when the raw identifier includes a `\` escape.
- Add focused parsing contract coverage for `err.m\u0065ssage`, `err?.m\u0065ssage`, and `(err).m\u0065ssage`.

Outcomes:
- Contract suite catches unicode-escaped `.message` bypass shapes without expanding into full dataflow analysis.

### Phase 52: HTTP error reflection unicode optional-chain correctness (done)
Goal: ensure the unicode-escaped dot-property guardrail truly covers optional-chaining `?.` member access (e.g. `err?.m\u0065ssage`, `(err)?.m\u0065ssage`).

Approach:
- Fix the parser-based unicode-dot check to treat `?.` as including the dot (parse the identifier immediately after `?.`).
- Add a focused parsing contract test that isolates unicode-escaped dot-access sinks and asserts they are detected.

Outcomes:
- Contract suite now genuinely enforces unicode-escaped `.message` sinks for both `.` and `?.` forms.

### Phase 53: HTTP error reflection parenthesized direct-arg hardening (done)
Goal: prevent bypassing the HTTP error reflection guardrail via parenthesized direct `err`/`error` arguments (e.g. `reply.raw.end((err))`).

Approach:
- Extend direct-argument parsing to allow harmless grouping parentheses around `err`/`error` while staying statement-local.
- Add focused parsing contract coverage for `reply.raw.end((err))`.

Outcomes:
- Contract suite catches parenthesized direct-err response sinks without expanding into general expression dataflow.

### Phase 54: Text helper drift guard for `web/public` (done)
Goal: keep the browser-public helper `web/public/_shared/text_one_line.js` behavior aligned with the canonical `src/text.js`, preventing silent drift.

Approach:
- Add parity coverage that compares `formatOneLineUtf8` and default `formatOneLineError` outputs against `src/text.js`.
- Add contract coverage for `includeNameFallback` option modes used by public scripts.

Outcomes:
- Contract suite fails if `web/public/_shared/text_one_line.js` formatting diverges from the canonical semantics.

### Phase 55: HTTP error reflection parenthesized String/JSON args (done)
Goal: prevent bypassing HTTP error reflection guardrails via grouping parentheses in `String(...)` / `JSON.stringify(...)` (e.g. `res.send(String((err)))`, `res.send(JSON.stringify((err)))`).

Approach:
- Extend the existing `String(err)` / `JSON.stringify(err)` patterns to accept grouped `(err)`/`((err))` forms.
- Add focused parsing contract coverage for these bypass shapes.

Outcomes:
- Contract suite catches parenthesized `String((err))` and `JSON.stringify((err))` reflection patterns.

### Phase 56: Unicode-brace escape drift guards (done)
Goal: lock in brace-form unicode escape support (`\u{...}`) for identifier parsing and HTTP error reflection sink shapes.

Approach:
- Extend `js_scan_parse_helpers_contract` to cover `parseIdentifierWithUnicodeEscapes` with `\u{...}` (plus an out-of-range rejection case).
- Extend the HTTP error reflection parsing contract to cover `err["m\u{65}ssage"]` and `err.m\u{65}ssage` shapes (and `?.` / parenthesized variants).

Outcomes:
- Contract suite fails if brace-form escape decoding regresses, preventing bypass drift across scanners that rely on these parsing primitives.

### Phase 57: Brace-form unicode escapes end-to-end scanner coverage (done)
Goal: ensure sink scanners themselves remain robust to brace-form escapes (`\u{...}`), not just the shared parsing primitives.

Approach:
- Extend eval sink parsing contract to include brace-form escaped identifiers (e.g. `ev\u{61}l(...)`, `setTime\u{6f}ut("...")`).
- Extend DOM XSS parsing contract to include brace-form escaped sink properties (e.g. `el.inn\u{65}rHTML = ...`, `document.wr\u{69}te(...)`).
- Extend subprocess sink parsing contract to include brace-form escaped `exec`/`execSync` properties (e.g. `cp.e\u{78}ec(...)`).

Outcomes:
- Contract suite will fail if scanner-level detection regresses for `\u{...}` escape bypass shapes.

### Phase 58: Close-race crash hardening sweep (done)
Goal: prevent process crashes from synchronous throws in network send/close paths (WebSocket, streams, and `net.Socket`) under close races.

Approach:
- **Browser WebSocket**:
  - Centralize safe wrappers in `web/src/net/wsSafe.ts` (`wsSendSafe`, `wsCloseSafe`).
  - Use these helpers across the web networking clients so `WebSocket.send()` / `WebSocket.close()` cannot throw and abort the worker/UI.
- **WebRTC DataChannel**:
  - Treat `RTCDataChannel.send()` as a potentially-throwing operation; guard it and surface errors via the existing tunnel/proxy error channels.
- **Node WebSocket + sockets**:
  - Wrap `ws.send(...)`, `socket.write(...)`, `socket.end(...)`, and `socket.destroy()` in `try/catch` where close races can occur.
  - On sync send/write failure, prefer deterministic teardown (`destroy`) and stable, byte-bounded logging rather than reflecting raw error details to clients.

Outcomes:
- Network relay/upgrade paths and supporting scripts are resilient to close-race sync throws across Node and browser runtimes.
- The contract suite and targeted package test suites were used to validate refactor slices as they landed.

Some coding guidelines:

## General Principles

Write **modern, clean, elegant, concise, and direct** Rust code.

## Code Style

### Indentation
- **2 spaces** for indentation (not 4, not tabs)

### Control Flow
- **Early returns** - avoid nesting at all costs
- Use `for` loops over `.for_each()` unless using rayon for parallelism
- Functional/declarative style with iterators where it's clearer
- Imperative style where it's more readable

### Imports
- **Always import types, never use qualified syntax**
- Import what you use at the top of the file
- Good: `use std::io::Result;` then use `Result`
- Bad: `std::io::Result` in the code
- Good: `use MyModule::{Type1, Type2};` then use `Type1`, `Type2`
- Bad: `MyModule::Type1`, `MyModule::Type2` everywhere

## Rust-Specific Patterns

### Async Closures
- **`async ||` and `async |args|` are legal Rust syntax** - DO NOT rewrite them
- Use `async |item| { ... }` directly, NOT `|item| { async move { ... } }`
- Good: `.for_each_concurrent(n, async |item| { ... })`
- Bad: `.for_each_concurrent(n, |item| { async move { ... } })`

### Iterators
- Use iterator chains for data transformations
- `.collect_vec()` from itertools instead of `.collect::<Vec<_>>()`
- Prefer `.map()`, `.filter()`, `.flat_map()` over manual loops for transformations
- Use rayon's `.par_iter()` for parallelism

### Naming
- Function names should describe what they create/do
  - Good: `rocksdb_options()` (creates options)
  - Bad: `configure_rocksdb()` (sounds like it configures something)

### Type Casts
- Avoid unnecessary type casts
- Question every cast - is it really needed?

### Concurrency and Sharing
- **NEVER use Arc when you don't need it**
  - Rayon closures capture by reference - don't Arc or clone unnecessarily
  - Question every Arc/clone - why is this here?

## Libraries and APIs

### Research and Use Modern APIs
- Always research the current best practices for libraries you're using
- Use the latest stable APIs, not outdated patterns
- Don't assume - look up documentation and examples
- If something seems inefficient, research if there's a better way

## Logging and Observability

### Structured Logging
- Use `tracing` crate, not `println!` for logging
- **Structured logging means using fields**, not string interpolation:
  - Good: `info!(cpus = num_cpus, threads = num_threads, "starting")`
  - Bad: `info!("Starting with {} cpus and {} threads", num_cpus, num_threads)`
- **Use shorthand when variable name matches field name**:
  - Good: `info!(labels_file, count, "appending")`
  - Bad: `info!(labels_file = labels_file, count = count, "appending")`

## Performance

### Parallelism
- Use rayon for CPU-bound parallel operations
- Example: `files.par_iter().map(...).sum()`

## Cargo Commands

### Validation
- **NEVER use `cargo build` or `cargo build --release` just to validate code**
- **NEVER run binaries to test if code works** (see warning at top of this document)
- Use `cargo check` or `cargo check --workspace` for validation - it's 10-100x faster than building

### Prove correctness BEFORE implementing

Before making architectural changes or rewrites:

1. **Write down the algorithm** in plain language
2. **Prove it correct** - trace through edge cases, identify invariants
3. **Only then implement**

If you can't prove it correct on paper, you can't implement it correctly in code.

## When in Doubt

- **PROVE** correctness before implementing
- **Simpler is better** than clever
- **Direct is better** than abstracted
- **Explicit is better** than implicit
- **Fast failures** are better than silent failures
