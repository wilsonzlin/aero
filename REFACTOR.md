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
  - Centralize best-effort wrappers in `web/src/net/rtcSafe.ts` (`dcSendSafe`, `dcCloseSafe`, `pcCloseSafe`).
  - Treat `RTCDataChannel.send()` as a potentially-throwing operation; use `dcSendSafe` in WebRTC transports and surface failures via existing tunnel/proxy error channels (then close deterministically).
- **Node WebSocket + sockets**:
  - Wrap `ws.send(...)`, `socket.write(...)`, `socket.end(...)`, and `socket.destroy()` in `try/catch` where close races can occur.
  - On sync send/write failure, prefer deterministic teardown (`destroy`) and stable, byte-bounded logging rather than reflecting raw error details to clients.
  - Prefer the shared best-effort helpers in `src/ws_safe.js` (`wsSendSafe`, `wsCloseSafe`) to avoid duplicating close-race guards. (`wsCloseSafe` formats close reasons as one-line UTF-8 and caps them to 123 bytes by default per RFC6455.) For convenience, `scripts/_shared/ws_safe.js` re-exports these helpers.
  - When using `wsSendSafe(ws, data, cb)`, be careful with API differences: browser `WebSocket.send(data)` does **not** accept a callback argument. The shared helper must not pass `cb` to 1-arg `send()` implementations (treat callback as a post-send notification only).
    - Note: some ws-style implementations accept callbacks but expose a rest-arg `send(data, ...args)` signature (arity 1). Don’t key solely off `send.length`; use a ws-style indicator (e.g. `.terminate()`) and lock in the heuristic with a contract test.
  - When using stream wrappers like `ws.createWebSocketStream(...)`, do **not** destroy the wrapper stream before sending a close frame (it can prevent the close control frame/code from reaching the peer). Prefer: send close → then destroy on `close`/`error` events (best-effort, try/catch).
  - When implementing raw-upgrade WebSocket protocols (writing frames directly to a `Duplex`), avoid destroying the upgrade socket immediately after writing/echoing a close frame: use `end()` and only `destroy()` after a short timeout so the close response has a chance to flush.
- For raw HTTP upgrade **rejections** (writing `HTTP/1.1 <status>` to a `Duplex`), prefer:
  - `encodeHttpTextResponse` from `src/http_text_response.js` to build the response bytes with a correct `Content-Length` + single `\r\n\r\n` delimiter (and to reject CR/LF in header fields).
  - `endThenDestroyQuietly` from `src/socket_end_then_destroy.js` to ensure the rejection response flushes and the socket doesn’t linger forever.

Outcomes:
- Network relay/upgrade paths and supporting scripts are resilient to close-race sync throws across Node and browser runtimes.
- The contract suite and targeted package test suites were used to validate refactor slices as they landed.

### Phase 59: HTTP streaming pipeline hygiene sweep (done)
Goal: avoid brittle `Readable.pipe(res)` error/teardown behavior by using `pipeline(...)` and making abort/disconnect handling deterministic and non-noisy.

Approach:
- Use `pipeline(stream, res)` (from `node:stream/promises`) for file-to-response streaming paths so backpressure and errors are handled consistently.
- Avoid writing response headers before the stream has actually opened when we need `Content-Length` / `Content-Type` (wait for the stream `open` event first).
- Treat common client abort errors as expected (`ERR_STREAM_PREMATURE_CLOSE`, `ECONNRESET`, `EPIPE`) and suppress them from error logs while still logging real stream failures.

Outcomes:
- Production server static file streaming uses `pipeline(...)` with best-effort teardown and reduced log noise on client disconnects.
- Dev helper servers that stream files over HTTP were updated to use `pipeline(...)`, best-effort `res.destroy()` in error paths, and to suppress noisy expected client abort errors.

### Phase 60: Node WebSocket send helper arity hardening (done)
Goal: avoid accidental callback misuse across ws-style and browser-style WebSocket implementations while preserving error reporting for ws-style `send(..., cb)` APIs.

Approach:
- Keep a shared Node-only helper in `src/ws_safe.js` (`wsSendSafe`, `wsCloseSafe`) for tools/tests/prototypes (re-exported by `scripts/_shared/ws_safe.js`).
- For `wsSendSafe(ws, data, cb)`:
  - Do **not** pass callbacks into browser-style `WebSocket.send(data)` (no callback parameter).
  - Do pass callbacks into ws-style implementations, including those that expose rest-arg `send(data, ...args)` signatures (arity 1) while still accepting callbacks.
- Lock in the behavior with a small contract test (`tests/ws_safe_contract.test.js`).

Outcomes:
- Callback behavior is deterministic across ws-style (`ws`, `tools/minimal_ws.js`) and browser-style WebSocket surfaces.
- Contract suite prevents regressions in the callback arity heuristics.
- The Node-side helpers are robust to `null` / invalid inputs (no “safe helper crashed” footguns); contract tests lock in the behavior.
- `wsCloseSafe` avoids sending empty close reasons and bounds/sanitizes close reasons by default (one-line, UTF-8, 123 bytes).

### Phase 61: ws-shim close-race hardening (done)
Goal: prevent close-race synchronous throws in the `ws` fallback shim from crashing contract tests / tools (especially when exceptions occur inside `socket.write(...)` completion callbacks).

Approach:
- Wrap upgrade handshake `socket.write(...)` in best-effort `try/catch` and bail out on sync failure.
- Ensure `end()` / `destroy()` calls made from inside `socket.write(..., cb)` callbacks are also best-effort (`try/catch`) so exceptions in completion handlers cannot abort the process.
- Avoid relying on direct `socket.write/end/destroy` (and internal `res.writeHead/end/destroy`) method getter reads by fetching methods via `tryGetProp` before invoking (covers hostile/monkeypatched getter sync-throws).

Outcomes:
- `scripts/ws-shim.mjs` is resilient to close races during handshake and close-control-frame mirroring, and does not crash on hostile method getters, matching the broader “no sync-throw on send/close” posture.

### Phase 62: Long-tail boundary hygiene sweep (completed)
Goal: keep shrinking the remaining “long tail” of Node-side boundary surfaces that can synchronously throw (HTTP response writes and socket sends) so non-core tooling can’t crash the process in edge cases (close races, poisoned globals, hostile getters/mocks).

Approach:
- Prefer small, local refactors that preserve behavior while making boundary writes best-effort:
  - wrap `res.writeHead`/`res.setHeader`/`res.end` in `try/catch` with `res.destroy()` fallback
  - wrap `socket.write`/`socket.end` similarly (or reuse existing safe helpers when appropriate)
- Use fast, cheap validation:
  - `npm run test:contracts` for cross-repo guardrails
  - `node --check <file>` for standalone scripts that aren’t exercised by tests

Progress:
- Web + tooling HTTP servers:
  - `web/demo/server.js`: hardened response/header writes with safe helpers + destroy fallback.
  - `web/serve-smoke-test.mjs`: hardened response/header writes with safe helpers + destroy fallback.
  - `web/vite.config.ts`: hardened dev/preview middlewares (`res.setHeader/res.end`) with destroy fallback.
  - `bench/server.js`, `bench/gpu_bench.ts`: hardened response header/body writes against close-race sync throws.
- Aero Gateway (Node):
  - `backend/aero-gateway/src/routes/*`: guarded long-tail socket methods (`setNoDelay`, timeouts, `write/end/destroy`) in upgrade/bridge handlers.
  - `backend/aero-gateway/bench/run.mjs`: hardened UDP `socket.send(...)` and TCP sink ACK `socket.write/end` paths against sync throws and repeated writes.
  - `backend/aero-gateway/src/dns/upstream.ts`: hardened `queryUdpUpstream` against sync `send()` throws and added focused regression coverage.
- Net proxy + related tools (Node):
  - `net-proxy/src/*`: hardened upgrade/relay boundaries (hostile getters + send/close races) and tightened UDP send failure behavior (fatal close in multiplexed mode, with regression coverage).
  - `tools/net-proxy-server/src/server.js`: guarded pause/resume and backpressure paths to avoid sync-throw crashes in less-traveled tooling.
  - `tools/disk-streaming-browser-e2e/src/servers.js`, `tools/minimal_ws.js`, `scripts/ws-shim.mjs`, `scripts/ci/run_browser_perf.mjs`: hardened long-tail `res.*` / `req.end()` / socket init / `.unref()` boundaries while preserving behavior.
- Shared teardown helper:
  - `src/socket_end_then_destroy.{js,cjs}`: hardened against hostile/monkeypatched method getters (`end/destroy/once/off/removeListener/unref`) and extended contract coverage to lock in “never throw” behavior.
- Shared timer helper:
  - `src/unref_safe.js`: added `unrefBestEffort(...)` so we don’t rely on `timer.unref?.()` (which still reads the getter and can synchronously throw). Refactored `src/` call sites and added contract coverage.
  - Follow-up: refactored additional Node-side packages (gateway/proxy/tools) to use `unrefBestEffort` for timers (connect timeouts, GC intervals, test timeouts) to keep the hostile-getter posture consistent outside `src/`.
- Guardrails:
  - `tests/unref_usage_contract.test.js`: prevents reintroducing direct `.unref()` calls in production sources (prefer `.unref?.()`).
  - `tests/dgram_usage_contract.test.js`: restricts `dgram.createSocket(...)` usage to known modules.

### Phase 63: Post-sweep type-safety polish (done)
Goal: after the heavy boundary hardening sweeps, do small follow-up refactors that keep the new guards **type-safe** and avoid “`any` + repetition” patterns that can rot over time.

Approach:
- Prefer local helpers that reduce repeated try/catch boilerplate without changing behavior.
- Keep middleware signatures accurate (`http.IncomingMessage` / `http.ServerResponse`) so future edits don’t reintroduce unsafe assumptions.
- Validate with cheap checks (`npm -w <pkg> run typecheck`, `npm run test:contracts`).

Progress:
- `web/vite.config.ts`: replaced ad-hoc `(res as any).destroy?.()` fallbacks with a typed `destroyResponseQuietly` helper and typed middleware signatures.

### Phase 64: Close long-tail helper drift gaps (done)
Goal: where we deliberately keep multiple copies of “safe boundary” helpers (ESM vs CJS workspaces), ensure their high-risk behavior doesn’t silently drift.

Approach:
- Prefer **package-local tests** over cross-workspace imports (avoids ESM/CJS tooling friction).
- Mirror the “high signal” contract expectations from repo-root `ws_safe` tests inside the CJS packages that carry their own implementations.

Progress:
- `net-proxy/src/wsClose.ts`: added missing unit coverage for `wsSendSafe` arity heuristics in `net-proxy`’s own test suite.
- `net-proxy/src/wsClose.ts`: aligned `wsIsOpenSafe` “fail open when readyState is not observable” semantics with the canonical `src/ws_safe.js` behavior (with local tests).

### Phase 65: Net-proxy wsClose helper completeness (done)
Goal: ensure the CJS `net-proxy` copies of the WebSocket safety helpers have the same “high signal” defensive behavior as the canonical repo implementation.

Progress:
- `net-proxy/src/test/ws-close.test.ts`: added missing coverage for:
  - `wsCloseSafe` empty reason behavior (treat as absent)
  - `wsCloseSafe` hostile reason input (toString throws)
  - `wsCloseSafe` invalid ws input (no-op)
  - `wsSendSafe` invalid ws input (returns false, cb async)
  - `wsIsOpenSafe` invalid ws input + OPEN getter throw behavior

### Phase 66: Clamp hostile negative bufferedAmount (done)
Goal: treat negative `bufferedAmount` readings as invalid and clamp them to `0` so hostile/buggy implementations cannot undercount backlog and bypass backpressure.

Progress:
- `src/ws_backpressure.js`: clamp negative `ws.bufferedAmount` to `0`.
- `web/src/net/{wsSafe.ts,rtcSafe.ts}`: clamp negative `bufferedAmount` to `0`.
- `net-proxy/src/wsBufferedAmount.ts`: clamp negative `bufferedAmount` to `0`.
- Tests:
  - `tests/ws_backpressure_contract.test.js`: added regression for negative bufferedAmount.
  - `tests/web_ws_safe_contract.test.js`: added `wsBufferedAmountSafe` regression (incl. negative).
  - `tests/web_rtc_safe_contract.test.js`: extended regression to include negative.
  - `net-proxy/src/test/ws-buffered-amount.test.ts`: added negative bufferedAmount coverage.

### Phase 67: De-duplicate socket/stream safe method helpers (done)
Goal: reduce drift and repetition by single-sourcing the “hostile getter + close-race safe” socket/stream method invocations used across Node packages.

Approach:
- Add canonical helpers in repo-root `src/` with ESM+CJS parity:
  - `src/socket_safe.js`, `src/socket_safe.cjs` (plus `.d.ts` stubs)
- Convert workspace-local helpers into thin re-exports:
  - `net-proxy/src/socketSafe.ts` → re-export from `src/socket_safe.cjs`
  - `backend/aero-gateway/src/routes/socketSafe.ts` → re-export from `src/socket_safe.js` (keep `destroyQuietly` as a stable local alias)
- Add contract/parity coverage:
  - `tests/socket_safe_contract.test.js`
  - `tests/socket_safe_parity.test.js`

Outcomes:
- Socket/stream best-effort calls (`destroy/end/pause/resume/...`) now share one implementation across Node workspaces, making behavior changes easier to audit and test-lock.

### Phase 68: De-duplicate HTTP response write helpers (done)
Goal: reduce drift by single-sourcing the “best-effort writeHead/end with destroy fallback” pattern used by Node HTTP servers.

Approach:
- Add canonical helpers with ESM+CJS parity:
  - `src/http_response_safe.js`, `src/http_response_safe.cjs` (plus `.d.ts` stubs)
- Convert workspace-local helper into a thin re-export:
  - `net-proxy/src/httpResponseSafe.ts` → re-export from `src/http_response_safe.cjs`
- Add contract/parity coverage:
  - `tests/http_response_safe_contract.test.js`
  - `tests/http_response_safe_parity.test.js`

Outcomes:
- Node HTTP responder helpers are single-sourced and guarded by the contract suite, preventing subtle drift across packages.

### Phase 69: De-duplicate web helper server response writes (done)
Goal: reduce drift and boilerplate in the repo’s small Node “web helper servers” by reusing the canonical best-effort HTTP response writer.

Approach:
- Refactor simple web servers to use `tryWriteResponse` from `src/http_response_safe.js` instead of local `tryGetProp` + `writeHead/end/destroy` wrappers:
  - `web/demo/server.js`
  - `web/serve-smoke-test.mjs`
- Preserve behavior by keeping existing headers/caching semantics; only centralize the write path and its error handling.

Outcomes:
- Web helper servers have simpler, more consistent response writing and inherit the same hostile-getter/close-race hardening as the rest of the repo.

### Phase 70: De-duplicate web tooling response safety helpers (done)
Goal: reduce drift by removing ad-hoc “destroy on response write failure” helpers in web tooling and reusing the canonical socket/HTTP helpers.

Approach:
- `web/vite.config.ts`: replace `destroyResponseQuietly` (and its `tryGetProp` dependency) with `destroyBestEffort` from `src/socket_safe.js`.
- `web/scripts/serve.cjs`: replace local `writeHeadSafe/endSafe/destroySafe` helpers with `tryWriteResponse` from `src/http_response_safe.cjs`.

Outcomes:
- Web tooling now uses the same canonical safety helpers as production services, shrinking duplicated “best-effort response” implementations.

### Phase 71: De-duplicate bench server response safety helpers (done)
Goal: reduce drift in bench tooling by reusing the canonical “destroy best-effort” and “tryWriteResponse” primitives for non-streaming responses.

Approach:
- `bench/server.js`:
  - Replace local `destroyResponseQuietly` with `destroyBestEffort` from `src/socket_safe.js`.
  - Refactor `sendText(...)` to use `tryWriteResponse` from `src/http_response_safe.js` (preserving existing headers like `Cache-Control: no-store` and bounded body formatting).

Outcomes:
- Bench’s dev-only static server now shares the same hardened response teardown/write path as production Node services, without changing the streaming file path (`pipeline`).

### Phase 72: De-duplicate GPU bench server response writes (done)
Goal: reduce drift in bench tooling by removing ad-hoc response “destroy on failure” wrappers and reusing the canonical helpers.

Approach:
- `bench/gpu_bench.ts`:
  - Replace local `destroyResponseQuietly` wrappers with `destroyBestEffort` from `src/socket_safe.js`.
  - Serve the bench HTML using `tryWriteResponse` from `src/http_response_safe.js` (instead of `setHeaderBestEffort` + `endBestEffort`).

Outcomes:
- Bench tooling uses the same hardened HTTP response writer and destroy semantics as the rest of the repo, shrinking duplicated “best-effort response” logic.

### Phase 73: De-duplicate dev server end() safety wrappers (done)
Goal: reduce drift in small Node dev helper servers by removing local “end + destroy-on-throw” wrappers and reusing the canonical response writer.

Approach:
- `server/range_server.js`: replace local `endSafe` with `tryWriteResponse(..., headers=null)` in all non-streaming response paths (OPTIONS / HEAD / 304 / errors).
- `server/chunk_server.js`: same replacement for non-streaming response paths.

Outcomes:
- Dev helper servers now share the same hardened “writeHead + end with destroy fallback” behavior as production Node services, while keeping their streaming `pipeline(...)` paths unchanged.

### Phase 74: Remove remaining bench server response helper drift (done)
Goal: finish de-duplicating bench tooling response boundaries by removing remaining local `tryGetProp`-based `setHeader/end` wrappers.

Approach:
- `bench/server.js`:
  - Remove local `setHeaderBestEffort` / `endBestEffort` wrappers and stop importing `src/safe_props.js` here.
  - Use `tryWriteResponse` for all non-streaming responses (e.g. OPTIONS / HEAD / 405 errors).
  - Use a single `res.writeHead(200, headers)` + `pipeline(...)` for streaming file responses.

Outcomes:
- Bench static server now has no ad-hoc response-method wrappers; it uses the same canonical best-effort HTTP writer as the rest of the repo and keeps the streaming path minimal and explicit.

### Phase 75: De-duplicate “call a socket method and capture error” helpers (done)
Goal: reduce drift by single-sourcing the “call method if present; return thrown error (or missing-method error) instead of throwing” helper used by TCP backpressure/teardown paths.

Approach:
- Extend canonical helpers:
  - `src/socket_safe.js` / `src/socket_safe.cjs`: add `callMethodCaptureErrorBestEffort(obj, key, ...args)` with ESM/CJS parity + `.d.ts` declarations.
  - Add contract/parity coverage:
    - `tests/socket_safe_contract.test.js`
    - `tests/socket_safe_parity.test.js`
- Migrate callsites:
  - `server/src/tcpProxy.js`: remove local `tryGetMethod`/`destroyBestEffort`/`callMethodCaptureError` implementations; reuse canonical helpers.
  - `tools/net-proxy-server/src/server.js`: remove local `tryGetMethod`/`callMethod*`/`destroyQuietly`/`removeAllListenersQuietly` and reuse canonical helpers.

Outcomes:
- TCP pause/resume/end error capture is now consistent across Node packages and locked by the repo-root contract suite.

### Phase 76: De-duplicate disk-streaming browser e2e server response helpers (done)
Goal: reduce drift in browser e2e harness servers by removing local response “destroy on failure” wrappers and reusing the canonical HTTP response writer.

Approach:
- `tools/disk-streaming-browser-e2e/src/servers.js`:
  - Remove local `destroyQuietly` / `setHeaderBestEffort` / `endBestEffort` / `withCommonAppHeaders` wrappers.
  - Use `tryWriteResponse` from `src/http_response_safe.cjs` and a shared `SAB_HEADERS` constant so all responses consistently include COOP/COEP for `crossOriginIsolated`.

Outcomes:
- The browser e2e harness server now uses the same hardened response writer as production Node services, shrinking duplicated “best-effort response” logic.

### Phase 77: De-duplicate ws-shim socket method wrappers (done)
Goal: reduce drift in the `ws` fallback shim by reusing canonical “destroy/end best-effort” and “call method + capture error” helpers.

Approach:
- `scripts/ws-shim.mjs`:
  - Remove local `tryGetProp` + `tryGetMethod` + `callMethodOptional` helpers.
  - Replace local `destroyQuietly` / `endQuietly` / `writeCaptureError` / `callRequiredMethodCaptureError` logic with imports from `src/socket_safe.js`:
    - `destroyBestEffort`
    - `endBestEffort`
    - `callMethodCaptureErrorBestEffort`

Outcomes:
- The shim’s close-race/hostile-getter-safe socket method calls are now single-sourced, and the contract suite continues to validate shim behavior.

### Phase 78: De-duplicate socket_end_then_destroy internal helpers (done)
Goal: reduce drift by removing duplicated “safe method lookup + optional invocation” logic from the canonical `socket_end_then_destroy` helper itself.

Approach:
- `src/socket_safe.{js,cjs}`:
  - Export `tryGetMethodBestEffort(obj, key)` for safe method retrieval.
  - Export `callMethodBestEffort(obj, key, ...args)` for safe optional invocations (returns boolean).
  - Update `.d.ts` stubs accordingly.
- `src/socket_end_then_destroy.{js,cjs}`:
  - Replace local `tryGetProp`/`tryGetMethod`/`callMethodOptional` helpers with the canonical exports above.
  - Replace ad-hoc `timer.unref` best-effort call with `unrefBestEffort(...)`.

Outcomes:
- `socket_end_then_destroy` is now implemented in terms of the same canonical safety helpers it conceptually depends on, making future behavior changes easier to audit and test-lock.

### Phase 79: Test-lock new socket_safe exports (done)
Goal: prevent drift in newly-exported socket helper primitives (`tryGetMethodBestEffort`, `callMethodBestEffort`) by extending contract/parity coverage.

Approach:
- `tests/socket_safe_contract.test.js`: add coverage for:
  - `tryGetMethodBestEffort` return shape (function or null)
  - `callMethodBestEffort` return behavior (true on missing method, false on thrown method)
- `tests/socket_safe_parity.test.js`: add basic ESM/CJS parity assertions for the new exports.

Outcomes:
- The canonical helper surface is guarded by the contract suite, reducing the chance of subtle ESM/CJS drift when these primitives are reused by other modules.

### Phase 80: De-duplicate raw socket write/destroy fallbacks (done)
Goal: reduce drift by reusing canonical socket safety helpers in remaining “raw socket write” boundaries.

Approach:
- `src/http_upgrade_reject.js`: replace ad-hoc `socket?.destroy?.()` fallback with `destroyBestEffort` from `src/socket_safe.js`.
- `src/ws_handshake_response.js`: replace `try { socket.write(...) } catch { socket.destroy?.() }` with:
  - `callMethodCaptureErrorBestEffort(socket, "write", ...)`
  - `destroyBestEffort(socket)` on error

Outcomes:
- Raw socket handshake/rejection paths now use the same hardened “hostile getter + close-race safe” helpers as the rest of the repo, further reducing ad-hoc best-effort logic.

### Phase 81: Web: De-duplicate best-effort `.destroy?.()` calls (done)
Goal: reduce drift in browser/WebWorker code by centralizing “best-effort optional method call” behavior (and avoid direct optional-chaining method calls that can throw on hostile proxies).

Approach:
- Add `web/src/safeMethod.ts`:
  - `callMethodBestEffort(obj, key, ...args)` for safe, best-effort optional method invocation (returns boolean).
  - `destroyBestEffort(obj)` convenience wrapper.
- Replace ad-hoc `.destroy?.()` / `.unconfigure?.()` calls with the canonical helper:
  - `web/src/gpu/webgpu-presenter.ts`
  - `web/src/gpu/webgpu-presenter-backend.ts`
  - `web/src/workers/gpu-worker.ts`
  - `web/src/workers/io.worker.ts`
  - `web/src/bench/webgpu_bench.ts`

Outcomes:
- WebGPU presenter/worker teardown paths now share a single hardened “call optional method best-effort” primitive, reducing repetition and making future hardening changes one-touch.

### Phase 82: Web: Test-lock and extend safeMethod usage for event callbacks (done)
Goal: make web-side “optional method call” behavior more robust and prevent drift by covering hostile getter cases with tests.

Approach:
- `web/src/safeMethod.ts`:
  - Export `tryGetMethodBestEffort(...)` so callsites can safely detect presence (without treating missing as success).
- Replace remaining high-value optional-chaining method calls in WebGPU paths:
  - Use `callMethodBestEffort(ev, "preventDefault")` instead of `ev?.preventDefault?.()` / casted variants.
  - Use `tryGetMethodBestEffort(device, "addEventListener"/"removeEventListener")` to install/remove `"uncapturederror"` handlers without exposing hostile getter throws.
  - `web/src/bench/webgpu_bench.ts`: simplify teardown by calling `callMethodBestEffort(device, "removeEventListener", ...)`.
- Add contract coverage:
  - `tests/safe_method_web_contract.test.js`: covers hostile getters, missing methods, and throwing methods for `tryGetMethodBestEffort`/`callMethodBestEffort`/`destroyBestEffort`.

Outcomes:
- WebGPU uncaptured-error handler install/remove and `preventDefault` calls are now centralized and hostile-getter-safe, with contract tests guarding future changes.

### Phase 83: Gateway: remove legacy destroyQuietly alias (done)
Goal: reduce drift and improve naming clarity in the gateway routes by using the canonical helper name (`destroyBestEffort`) consistently.

Approach:
- `backend/aero-gateway/src/routes/socketSafe.ts`: stop re-exporting `destroyBestEffort` under the legacy alias `destroyQuietly`.
- Update gateway route code to import/call `destroyBestEffort` directly:
  - `tcpProxy.ts`
  - `tcpMuxBridge.ts`
  - `tcpBridge.ts`
  - `wsDuplexClose.ts`

Outcomes:
- Gateway routes now use a single, repo-wide name for best-effort teardown (`destroyBestEffort`), reducing cognitive overhead and avoiding “quietly” vs “best-effort” naming drift.

### Phase 84: De-duplicate “write + capture ok + capture error” patterns (done)
Goal: remove remaining ad-hoc `try/catch` wrappers around `stream.write(...)` in backpressure-sensitive code that needs both:
- the write return value (`ok`) and
- a best-effort error capture path that is safe against hostile getters.

Approach:
- `src/socket_safe.{js,cjs}`:
  - Add `writeCaptureErrorBestEffort(stream, ...args) -> { ok: boolean, err: unknown | null }`.
  - Update `.d.ts` stubs and extend contract/parity tests to lock behavior.
- Replace local `try { ok = x.write(...) } catch { ... }` blocks with the canonical helper:
  - `backend/aero-gateway/src/routes/tcpMuxBridge.ts`
  - `backend/aero-gateway/src/routes/tcpBridge.ts`
  - `net-proxy/src/tcpRelay.ts`
  - `net-proxy/src/tcpMuxRelay.ts`
- Extend per-package shim exports where used:
  - `backend/aero-gateway/src/routes/socketSafe.ts`
  - `net-proxy/src/socketSafe.ts`

Outcomes:
- “write return + sync throw” handling is now single-sourced and test-locked, reducing drift and making backpressure-related safety changes one-touch.

### Phase 85: Apply writeCaptureErrorBestEffort to remaining tool callsites (done)
Goal: remove the last remaining `let ok=false; try { ok = socket.write(...) } catch { ... }` patterns in dev/test tooling so backpressure handling stays consistent across the repo.

Approach:
- `tools/net-proxy-server/src/server.js`:
  - Replace the remaining TCP stream `.write(...)` try/catch blocks with `writeCaptureErrorBestEffort`.

Outcomes:
- Tooling now uses the same canonical “write return + sync throw” primitive as production code, eliminating drift.

### Phase 86: Prefer specialized capture helpers over generic callMethodCaptureErrorBestEffort (done)
Goal: further reduce drift by using more specific, self-documenting primitives (`writeCaptureErrorBestEffort`, `endCaptureErrorBestEffort`) where callsites were still using the generic “call method + capture error” helper.

Approach:
- `src/ws_handshake_response.js`: use `writeCaptureErrorBestEffort` for handshake writes.
- `tools/net-proxy-server/src/server.js`: use `endCaptureErrorBestEffort` for FIN forwarding.

Outcomes:
- Remaining “write/end” capture callsites now share the canonical helpers dedicated to those operations, keeping behavior consistent and intent clearer.

### Phase 87: De-duplicate minimal_ws socket write/destroy wrappers (done)
Goal: reduce drift in `tools/minimal_ws.js` by reusing canonical “safe write capture” + “destroy best-effort” helpers (including hostile getter safety).

Approach:
- `tools/minimal_ws.js`:
  - Implement `trySocketWrite(...)` via `writeCaptureErrorBestEffort(...)` (preserving callback error microtask semantics).
  - Implement `trySocketDestroy(...)` via `destroyBestEffort(...)`.

Outcomes:
- The minimal WebSocket shim now shares the repo-wide hardened socket write/destroy primitives, without changing contract behavior.

### Phase 88: HTTP response safe API alignment (done)
Goal: keep the canonical “best-effort response writer” aligned with Node’s real `ServerResponse.writeHead(...)` overloads while preserving the repo’s “never throw at boundaries” posture.

Approach:
- `src/http_response_safe.{js,cjs}`:
  - Treat `headers` as one of:
    - `OutgoingHttpHeaders` (object map)
    - `OutgoingHttpHeader[]` raw header list (validated; must be even-length and alternating `string` keys with `string | number | string[]` values)
  - Ignore invalid header shapes (including empty arrays) and fall back to `writeHead(statusCode)` instead of throwing and tearing down.
  - Harden `sendJsonNoStore` / `sendTextNoStore` against hostile `opts.contentType` getters and missing/invalid values (stable defaults).
- Typings:
  - Update `src/http_response_safe*.d.ts` to reflect the supported header shapes and optional `contentType`.
- Tests:
  - Extend contract + parity coverage to lock in header-array handling and `opts.contentType` hardening.

Outcomes:
- Canonical response writer supports Node’s `writeHead(status, rawHeadersArray)` path safely and deterministically.
- `sendJsonNoStore` / `sendTextNoStore` no longer rely on trusted `opts` shapes for content-type selection.
- ESM/CJS parity is test-locked for the new behaviors.

### Phase 89: Web helper parity polish (done)
Goal: keep browser/worker best-effort helpers aligned with the canonical “nullish + object/function receiver” posture so hostile proxies and primitive inputs cannot crash teardown paths.

Approach:
- `web/src/unrefSafe.ts`: align the guard to `handle == null` (avoid “falsy” semantics drift).
- `web/src/safeMethod.ts`: bail out early for non-object/non-function receivers.

Outcomes:
- Web helper behavior stays consistent with the canonical Node-side helpers, and contract coverage continues to prevent drift.

### Phase 90: ws_safe invalid-input hardening (done)
Goal: ensure canonical Node-side WebSocket safety helpers fail closed on clearly-invalid inputs while preserving “fail open when state is not observable” semantics for real WebSocket-like objects.

Approach:
- `src/ws_safe.js`:
  - Make `wsIsOpenSafe` return `false` for non-object/non-function inputs (avoid treating primitives as open).
- Tests:
  - Extend `tests/ws_safe_contract.test.js` to lock in invalid-input behavior.

Outcomes:
- `wsIsOpenSafe(123)` (and other primitive inputs) reliably returns `false`.
- Existing behavior for object-like inputs without observable `readyState` remains unchanged and contract-locked.

### Phase 91: ws_backpressure invalid-input hardening + typing alignment (done)
Goal: keep `createWsSendQueue` robust to invalid `ws` inputs and keep its `.d.ts`/JSDoc aligned with its defaulting behavior.

Approach:
- `src/ws_backpressure.js`:
  - Make the internal `isOpen()` fail closed for non-object/non-function `ws` inputs.
  - Update JSDoc to mark `ws` / `highWatermarkBytes` / `lowWatermarkBytes` as optional (runtime defaults).
- `src/ws_backpressure.d.ts`:
  - Make `ws`, `highWatermarkBytes`, and `lowWatermarkBytes` optional.
  - Allow `createWsSendQueue(opts?)` to be called with `opts` omitted, matching runtime behavior.
- Tests:
  - Add a contract regression to ensure primitive `ws` inputs are not treated as open.

Outcomes:
- Backpressure callbacks are not triggered for nonsense primitive `ws` inputs.
- Type declarations reflect the actual supported call shapes (defaults) without affecting runtime behavior.

### Phase 92: Contract-suite drift + timing hardening (done)
Goal: keep the contract/parity suite reliable across environments and prevent “type stub drift” between ESM and CJS helper surfaces.

Approach:
- Add a contract test that auto-discovers `src/**/*.cjs.d.ts` files and enforces:
  - the matching `src/**/*.d.ts` exists, and
  - normalized contents match (ignoring CRLF and trailing whitespace).
- Add a contract test that ensures dual-module helper stubs match the *runtime* export surface:
  - parse `export function ...` declarations from the `.d.ts` files, and
  - assert the same names exist as `function` exports in both `src/<name>.js` (ESM) and `src/<name>.cjs` (CJS).
- Extend the same runtime export contract to cover ESM-only `src/**/*.d.ts` stubs:
  - when there is no `.cjs.d.ts` pair, require a matching `src/<name>.js` runtime module and validate exported function names there.
- Add a module boundary contract that prevents workspaces from deep-importing repo-root `src/*` helpers directly:
  - allow only minimal `export * from ".../src/..."` shim modules inside each workspace.
- Replace brittle fixed sleeps in the `ws_backpressure` contract with deterministic scheduling primitives / bounded waits.

Outcomes:
- The contract suite fails if any dual-module `.d.ts` pair in `src/` drifts.
- The contract suite fails if any helper `.d.ts` declares a function that isn’t exported at runtime (ESM and/or CJS, depending on module format).
- The contract suite fails if `net-proxy` or `aero-gateway` code deep-imports repo-root `src/*` helpers outside of shim modules.
- `ws_backpressure` contract timing is deterministic and less sensitive to runtime load or event-loop scheduling differences.

### Phase 93: Helper typing ergonomics (done)
Goal: make the repo’s defensive helper surfaces feel “type-correct” in TypeScript without changing runtime behavior.

Approach:
- Prefer `PropertyKey` over `string` for helper APIs that accept property/method keys, matching real JS semantics (including `symbol` keys).
- Add small contract coverage for symbol-key method lookup/invocation where helpers explicitly accept a key argument.

Outcomes:
- `safe_props` and `socket_safe` TypeScript stubs accept `PropertyKey` where appropriate.
- Contract suite includes symbol-key regressions for:
  - the web helper (`web/src/safeMethod.ts`)
  - the canonical Node socket helper (`src/socket_safe.js`)
  - the canonical safe getter helper (`src/safe_props.js`)
- `safe_props` runtime guards use the repo-standard “nullish + object/function” posture (`obj == null`) to avoid falsy-vs-nullish drift.

### Phase 94: Landing polish + PR visibility (done)
Goal: make it easy to open PRs and track CI status in environments without GitHub CLI tooling, without weakening CI enforcement or adding manual steps.

Approach:
- `scripts/safe-run.sh`: silence non-fatal Node version mismatch notes by default under Node major mismatch (opt-out via `AERO_CHECK_NODE_QUIET=0`), while keeping enforcement behavior unchanged.
- Add a tiny repo-root helper (`scripts/print-pr-url.mjs`) to print compare/PR URLs (and optionally Actions URLs) for the current branch:
  - support both env and cross-platform CLI flags (`--actions`, `--base`, `--remote`, `--branch`)
  - provide repo-root `npm run pr:url` / `npm run pr:links` shortcuts
  - contract-test the output and script wiring to prevent drift and portability regressions.
- Docs: update `AGENTS.md`, `README.md`, and `CONTRIBUTING.md` to point developers at the canonical commands and to avoid copy/paste footguns.

Outcomes:
- Agent/local runs are quieter by default under Node major mismatch (without changing enforcement behavior).
- PR creation + CI visibility can be done via copy/paste links even when `gh` isn’t available.
- Cross-platform usage is guarded by contracts (no POSIX-only env assignment in npm scripts).

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
