# Certificates

The Guest Tools media may include one or more public certificate files (`*.cer`, `*.crt`, `*.p7b`)
under this directory.

- For **test-signed** driver builds, `setup.cmd` installs these certificates so Windows trusts the
  driver catalog signatures.
- For **production/WHQL-signed** driver builds, this directory may contain only this README (no
  certificates are required).
