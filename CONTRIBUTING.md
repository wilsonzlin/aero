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
- Disk images: `.vhd`, `.vhdx`, `.vmdk`, `.qcow2`, `.img`, `.raw`, etc.
- ROM/firmware dumps from real hardware
- Captured traces/dumps that contain copyrighted payloads

`.gitignore` includes common patterns, but you are responsible even if Git
doesn’t catch something.

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

- Include unit tests where feasible.
- For compatibility behavior, include a minimal reproducible test (even if it
  only runs in the emulator).
- If you compare against Windows behavior, do it locally and describe the
  methodology in the PR. Do not upload Windows binaries/media as evidence.

## Security issues

Please do not open public issues for security vulnerabilities. See
`SECURITY.md` for reporting instructions.
