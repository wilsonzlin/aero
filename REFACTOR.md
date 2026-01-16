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
