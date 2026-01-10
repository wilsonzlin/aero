# Certificates

Place the public certificate(s) needed to validate Aero driver signatures in this directory.

`AeroTestRoot.cer` is a development/test root certificate included as a default for packaging.
Driver `.cat` files must be signed with a certificate that chains up to one of the certificates in this folder.

The installer will import all `*.cer` and `*.p7b` files found here into:

- `Root` (Trusted Root Certification Authorities)
- `TrustedPublisher` (Trusted Publishers)

For Windows 7 x64, **test signing mode** may still be required for kernel drivers that are not WHQL / production-signed.
