# Certificates

Place the public certificate(s) needed to validate Aero driver signatures in this directory.

`AeroTestRoot.cer` is a development/test root certificate included as a default for packaging.
Driver `.cat` files must be signed with a certificate that chains up to one of the certificates in this folder.

The installer (`setup.cmd`) will import all `*.cer`, `*.crt`, and `*.p7b` files found here into:

- `Root` (Trusted Root Certification Authorities)
- `TrustedPublisher` (Trusted Publishers)

If this directory contains no certificate files, `setup.cmd` will skip certificate installation with a warning.

For Windows 7 x64, **Test Signing** (or `nointegritychecks`) may still be required for kernel drivers that are not WHQL / production-signed.

## WHQL / production-signed drivers (no custom certs)

If you are building Guest Tools media that ships only WHQL/production-signed drivers (for example from `virtio-win`), you typically should **not** include any custom certificates here.

When building with `tools/packaging/aero_packager`, set:

- `--signing-policy none`

and ensure this directory contains **zero** certificate files (`*.cer/*.crt/*.p7b`) so the installer will not import anything into the machine trust stores.
