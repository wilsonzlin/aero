# Virtio Windows driver packaging (Windows 7 focus)

This directory is the **Aero packaging surface** for Windows virtio drivers.

Goals:

- Keep a stable directory layout that Aero can mount as a “drivers ISO”.
- Make it possible to either:
  - vendor a pinned set of driver binaries in-repo (`prebuilt/`), or
  - populate `prebuilt/` from an externally obtained virtio-win distribution.

Important:

- The `sample/` subtree contains **non-functional placeholder files** used for tooling/CI.
- Real driver artifacts (`.inf` / `.sys` / `.cat`, plus any INF-referenced payload files such as `WdfCoInstaller*.dll`) should be placed under `prebuilt/`.
- If you redistribute virtio-win derived binaries, ensure third-party attribution and license texts are included:
  - `THIRD_PARTY_NOTICES.md` (see `drivers/virtio/THIRD_PARTY_NOTICES.md`)
  - `licenses/virtio-win/` (best-effort copy of upstream LICENSE/NOTICE files when using `make-driver-pack.ps1`)

See `docs/virtio-windows-drivers.md` for installation steps and signing notes.
