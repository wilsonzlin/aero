# Fixtures & Test Assets Policy

This repository must **not** distribute proprietary operating system media (especially Microsoft Windows), BIOS/firmware dumps, proprietary drivers, or other copyrighted binaries.

In addition, large binary fixtures quickly bloat git history and slow down CI. To reduce this risk, CI enforces a repository policy check.

## What is NOT allowed in-repo

Do not commit files that are (or look like) OS installation media, disk images, or Windows binaries, including (non-exhaustive):

- Disk/VM images: `.iso`, `.img`, `.vhd`, `.vhdx`, `.vmdk`, `.qcow`, `.qcow2`, `.wim`
- Windows binaries: `.exe`, `.dll`
- Anything under Windows fixture directories such as `*/test_images/windows*` or `*/fixtures/windows*`

See `docs/13-legal-considerations.md` for the broader legal rationale.

## Size limits

New/changed blobs should generally stay **under 20MB**. If you need larger assets for tests, prefer:

- Generating fixtures at runtime (e.g., create minimal disk images during tests).
- Downloading fixtures as part of local-only setup or CI setup from an approved external source (public OSS mirror or private bucket).

If a fixture truly needs to live in-repo despite policy checks (for size or filetype), it must be explicitly allowlisted in `scripts/ci/check-repo-policy.sh` with a clear justification.

## Adding permissible fixtures (open source / small)

When adding a fixture that is safe to store in-repo:

1. Ensure the asset is **open-source / redistributable** and include provenance (source URL + license).
2. Keep it small, stable, and deterministic (avoid generated blobs when a tiny textual representation is possible).
3. Prefer placing fixtures in a dedicated directory with a short README describing:
   - Where it came from
   - License
   - Hash (SHA256) for integrity

## CI enforcement

CI runs `scripts/ci/check-repo-policy.sh` on PRs and pushes. If it fails, remove the disallowed file(s) and use one of the alternatives above.
