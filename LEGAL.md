# Legal & Compliance Summary

This document is **not legal advice**. It summarizes the project’s intent and
repo rules so development and distribution can proceed safely.

## What this project is (and is not)

**Aero is an emulator** (software that re-implements hardware behavior). Aero
does **not** include Microsoft Windows, Microsoft drivers, Microsoft firmware,
or any other Microsoft copyrighted code.

## Windows media, keys, and licensing (users supply their own)

If you run Windows inside Aero:

- You must supply your **own legally obtained Windows installation media**
  (e.g., ISO) and any required product keys.
- You are responsible for complying with the applicable Windows EULA and any
  other third-party license terms for software you run.
- Aero must **not** bypass Windows activation or DRM. Aero aims to provide
  compatibility, not circumvention.

The project **does not**:

- Distribute Windows images, ISOs, WIM/ESD files, updates, drivers, or other
  Microsoft components.
- Provide links to pirated content, activation cracks, or instructions to
  circumvent protections.

## Prohibited content in this repository

Do not commit or attach (including in issues/PRs) any of the following:

- Windows installation media or system files (ISO/WIM/ESD/CAB/MSU/MSI, etc.)
- Microsoft binaries (e.g., `.exe`, `.dll`, `.sys`) or extracted file trees
- ROM/firmware dumps from real hardware
- Licensed or proprietary SDKs/tools that cannot be redistributed
- Captured traces or dumps that include copyrighted payloads

See `.gitignore` and `CONTRIBUTING.md` for examples and enforcement guidance.

## Clean-room expectations

Aero is intended to be implemented from:

- Public specifications and standards
- Clean-room reverse engineering (black-box observation)
- Original work authored by contributors

Contributors **must not** copy code from:

- Microsoft Windows source/binaries or leaked materials
- GPL-licensed emulators/virtualizers in a way that creates a derivative work
  (e.g., QEMU) unless the licensing implications are understood and accepted
  by the project (current intent: **avoid GPL-only code paths**)

See `CONTRIBUTING.md` for concrete rules on citations, testing, and acceptable
reference material.

## Trademarks / no affiliation

Microsoft, Windows, Windows 7, and related marks are trademarks of Microsoft
Corporation. Aero is an independent project and is **not affiliated with,
endorsed by, or sponsored by** Microsoft.

See `TRADEMARKS.md` for naming and branding guidelines (including allowed
nominative use like “runs Windows 7” vs. confusing branding).

## DMCA posture (takedowns & repeat infringement)

If you believe content in this repository infringes your rights, see
`DMCA_POLICY.md` for the takedown/counter-notice process.

The project’s intent is to:

- Remove infringing material when properly notified
- Keep an auditable record of actions taken
- Discourage repeat infringement

## Licensing overview

- **Source code** (and most repository content) is dual-licensed:
  **MIT OR Apache-2.0** (see `LICENSE-MIT` and `LICENSE-APACHE`).
- **Documentation in `docs/`** is licensed under **CC BY 4.0**
  (see `docs/LICENSE.md`).

For attribution guidance, see `NOTICE` and `AUTHORS`.

## Hosted service templates

If Aero is ever offered as a hosted demo/service, start from:

- `TERMS_OF_SERVICE_TEMPLATE.md`
- `PRIVACY_POLICY_TEMPLATE.md`

These templates include the expected “users provide their own Windows media”
and “no circumvention / no infringement” posture.
