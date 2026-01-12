# Manual testing checklists

This directory contains **manual**, **reproducible** smoke-test procedures for behaviors that are hard to validate automatically.

Guidelines:

- Keep steps deterministic and copy/paste-friendly.
- Prefer built-in guest OS actions (e.g. Windows Control Panel tests) over external tools.
- Do **not** commit proprietary artifacts (Windows images/ISOs, driver binaries, etc.). Link to existing repo guidance instead.

## Checklists

- Windows 7 audio (in-box HD Audio / Intel HDA): [`audio-windows7.md`](./audio-windows7.md)

