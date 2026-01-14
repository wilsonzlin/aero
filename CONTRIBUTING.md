# Contributing to Aero

Thanks for helping build Aero.

Before contributing, please read:

- `LEGAL.md` (clean-room and distribution posture)
- `TRADEMARKS.md` (naming/branding guidance)
- `CODE_OF_CONDUCT.md`

## Licensing of contributions

Unless stated otherwise, contributions to this repository are accepted under
the project’s dual license: **MIT OR Apache-2.0** (see `LICENSE-MIT` and
`LICENSE-APACHE`).

Documentation under `docs/` is licensed under **CC BY 4.0** (see
`docs/LICENSE.md`).

By submitting a pull request, you agree that your contribution may be
distributed under these terms.

## Clean-room / IP rules (critical)

This project must not incorporate Microsoft copyrighted material or become a
derivative work of GPL-only emulator code.

### Do not copy from proprietary sources

Do **not** contribute code, tests, or docs derived from:

- Microsoft Windows source code (including leaked sources)
- Disassemblies/decompilations of Windows binaries
- Extracted Windows system files, drivers, fonts, icons, or UI assets
- Microsoft SDK headers/libraries that are not redistributable

### Do not copy from GPL emulators

Do **not** copy code (or mechanically translated code) from GPL-licensed
projects such as QEMU or other GPL-only emulators/virtualizers.

You may use such projects as **high-level behavioral references** (e.g., “it
does X”) but not as copy/paste sources. When in doubt, rely on public specs or
black-box testing and write original code.

### Prefer public specifications and citations

Good sources include:

- Intel/AMD Software Developer Manuals (SDM)
- PCI/PCIe, USB, SATA/AHCI, ACPI, VESA specifications
- W3C / WHATWG / WebGPU specifications for browser integration

When adding behavior based on a spec, include a link and section reference in
the PR description (and in docs when relevant).

## Prohibited files and artifacts

Do not commit or upload (including in issues/PRs):

- Windows ISOs, WIM/ESD files, update packages, or extracted file trees
- `.exe`, `.dll`, `.sys`, `.msi`, `.cab`, `.msu`, etc. from Windows
- Disk/VM images: `.vhd`, `.vhdx`, `.vmdk`, `.qcow2`, `.img`, `.raw`, etc.
  - Exception: a small number of **tiny, deterministic, license-safe** boot/test
    fixtures are allowlisted (see `docs/FIXTURES.md`, e.g.
    `tests/fixtures/boot/*.{bin,img}`).
- ROM/firmware dumps from real hardware
- Captured traces/dumps that contain copyrighted payloads

`.gitignore` includes common patterns, but you are responsible even if Git
doesn’t catch something.

CI enforces these rules via `scripts/ci/check-repo-policy.sh`. See
`docs/FIXTURES.md` for fixture alternatives (generate at runtime, download from
approved external sources, etc.).

If you need test programs, prefer:

- Small, self-authored test binaries whose source is included
- Existing permissively licensed test suites
- Synthetic test generators

## Source file headers (SPDX)

New source files should include an SPDX identifier:

- `SPDX-License-Identifier: MIT OR Apache-2.0` for code
- Documentation in `docs/` should follow `docs/LICENSE.md` (CC BY 4.0)

## Tests and validation

When submitting a change:

- Run `cargo fmt --all` before pushing (CI enforces `cargo fmt --all -- --check`).
- Include unit tests where feasible.
- For compatibility behavior, include a minimal reproducible test (even if it
  only runs in the emulator).
- If you compare against Windows behavior, do it locally and describe the
  methodology in the PR. Do not upload Windows binaries/media as evidence.

## Dependency policy (licenses + advisories)

### Automated updates (Dependabot)

This repository uses Dependabot to keep dependencies fresh with minimal maintainer
overhead. Update PRs are opened weekly and grouped to control noise:

- GitHub Actions: patch/minor updates grouped together; majors separated.
- npm: dev/tooling updates are grouped (Playwright + TS/Vite/Vitest toolchain).
- Rust (Cargo): patch/minor updates grouped; majors separated.
- Go modules: patch/minor updates grouped; majors separated.
- Terraform: updates grouped by provider (currently AWS).

### Auto-merge policy (safe updates only)

Some Dependabot PRs are automatically approved and set to auto-merge **only**
after required CI checks pass:

- GitHub Actions: patch/minor updates.
- npm: patch/minor updates for an allowlisted set of tooling dependencies
  (Playwright + TS/Vite/Vitest + type packages).

By default, Dependabot PRs that touch runtime/production dependencies are **not**
auto-merged. Maintainers can explicitly opt a specific PR into auto-merge by
adding the `automerge-deps` label.

Auto-merge logic lives in `.github/workflows/dependabot-auto-merge.yml`. The
allowlist is intentionally narrow; widen it only with intent.

### License allowlist (copyleft avoidance)

Aero’s IP posture explicitly forbids **GPL/LGPL/AGPL** (and similar copyleft)
contamination. CI enforces a strict **license allowlist** for third-party
dependencies across ecosystems.

Allowed licenses are expressed as SPDX identifiers (or SPDX expressions):

- `Apache-2.0`
- `MIT`
- `BSD-2-Clause`, `BSD-3-Clause`
- `ISC`
- `0BSD`
- `Zlib`
- `CC0-1.0`
- `CC-BY-3.0` (SPDX metadata packages in the npm ecosystem)
- `BSL-1.0`
- `Unicode-3.0`, `Unicode-DFS-2016`
- `BlueOak-1.0.0` (permissive; appears in the existing npm dependency graph)

How CI evaluates dependency licenses:

- **Dual-licensed** dependencies are allowed when their SPDX expression can be
  satisfied using allowlisted terms (e.g. `MIT OR Apache-2.0` is OK; `MIT OR
  GPL-3.0` is also OK because you can opt into MIT).
- **Conjunctive** expressions require every term to be allowlisted (e.g. `MIT AND
  Zlib` is OK; `MIT AND GPL-3.0` is not).
- **Unknown/missing** license metadata is treated as a CI failure. Fix it by
  switching dependencies or by ensuring upstream provides correct SPDX metadata
  (and re-run the check).
- **Vendored code** must include its license text in-tree and must use an
  allowlisted license. If you vendor code, include attribution + the license
  file(s) alongside the vendored directory.

### License + vulnerability gating

CI enforces a dependency policy to help keep the project compatible with the
repository’s **MIT OR Apache-2.0** licensing and to catch known vulnerabilities
early:

- Rust: `cargo-deny` (`deny.toml`) checks license allowlist + banned sources
  (no git deps by default) + RustSec advisories.
- npm: `scripts/ci/check-npm-licenses.mjs` checks dependency licenses against
  the allowlist (fails on copyleft/unknown licenses). This runs on PRs so
  Dependabot auto-merge is gated by it.
- Go: `govulncheck` runs for `proxy/webrtc-udp-relay` when Go deps/code change,
  plus nightly/manual runs to catch newly published advisories.
- Go: `go-licenses` checks module dependency licenses for
  `proxy/webrtc-udp-relay` against the allowlist.
- npm: a scheduled `npm audit` runs against the lockfile for high/critical
  issues (nightly/manual only to avoid PR noise).

If you add or update dependencies and CI fails:

- Prefer switching to an equivalent permissively licensed crate.
- If a new license is acceptable for the project, update `deny.toml` with a
  justification in the PR.

## Security issues

Please do not open public issues for security vulnerabilities. See
`SECURITY.md` for reporting instructions.
