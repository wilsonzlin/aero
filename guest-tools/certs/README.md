# Certificates

Place the public certificate(s) needed to validate Aero driver signatures in this directory.

The repository includes `AeroTestRoot.cer` as a **placeholder** development/test root certificate.
When building Guest Tools media from CI-signed driver packages (for example via `ci/sign-drivers.ps1` +
`ci/package-guest-tools.ps1`), the packaging step replaces any placeholder certs with the **actual**
public signing certificate used for the driver catalogs (by default: `out/certs/aero-test.cer`).

Driver `.cat` files must be signed with a certificate that chains up to one of the certificates in this folder.

For Guest Tools media with `manifest.json` `signing_policy=test`, the installer (`setup.cmd`) imports all `*.cer`, `*.crt`, and `*.p7b` files found here into:

- `Root` (Trusted Root Certification Authorities)
- `TrustedPublisher` (Trusted Publishers)

For `signing_policy=production|none`, `setup.cmd` skips certificate installation by default (even if certificate files exist) and logs a warning. Production/WHQL media should not ship any custom certificate files in this directory.

If this directory contains no certificate files, `setup.cmd` will skip certificate installation with a warning (and may fail with an error when `signing_policy=test`).

For Windows 7 x64, **Test Signing** (or `nointegritychecks`) may still be required for kernel drivers that are not WHQL / production-signed.

## WHQL / production-signed drivers (no custom certs)

If you are building Guest Tools media that ships only WHQL/production-signed drivers (for example from `virtio-win`), you must **not** ship any custom certificates (trust anchors) here.

When building with `tools/packaging/aero_packager`, set:

- `--signing-policy production` (or `none`)

and ensure this directory contains **zero** certificate files (`*.cer`, `*.crt`, `*.p7b`).

`aero_packager` will fail fast if any certificate files are present when using `--signing-policy production` or `--signing-policy none`:

- Remove the cert files (you may keep `certs/README.md`), **or**
- Re-run with `--signing-policy test` when building media for test-signed drivers.

Newer `setup.cmd` versions also refuse to import certificates when `signing_policy=production|none` unless `/installcerts` is explicitly provided, but the recommended and safest production configuration is still an empty `certs\` directory.
