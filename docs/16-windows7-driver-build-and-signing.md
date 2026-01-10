# 16 - Windows 7 Driver Build and Signing

This document collects practical notes for building and test-signing Windows drivers intended to run on Windows 7 SP1.

## CI test signing

`ci/sign-drivers.ps1` is intended for CI runners. It:

1. Creates a self-signed **code signing** certificate.
2. Uses `signtool` to sign common driver artifacts (`.sys`, `.cat`, etc.).
3. Verifies signatures using `signtool verify`.

## SHA-1 vs SHA-2

### File digest (`signtool /fd`) is not the whole story

Authenticode signing has two relevant hash/signature choices:

1. The **file digest** used in the Authenticode signature (`signtool sign /fd sha1|sha256`).
2. The **certificate signature algorithm** used to sign the *certificate itself* (for a self-signed cert, the cert is signed by its own key).

On stock Windows 7 SP1 **without SHA-2 updates** (notably **KB3033929** and **KB4474419**), a common failure mode is:

- the file is signed with `/fd sha1`, but
- the signing certificate is **SHA-256-signed**,

and the system fails to validate the certificate chain because it cannot process SHA-2 signatures in certificates.

### What we do in CI

When `ci/sign-drivers.ps1` is run with `-Digest sha1` (or `-DualSign`), it **attempts** to create the self-signed certificate using:

```
New-SelfSignedCertificate -HashAlgorithm sha1
```

This is what makes the certificate report a signature algorithm like `sha1RSA` / `sha1WithRSAEncryption` in logs, which is the desired result for maximum compatibility with unpatched Windows 7.

### Dual signing

If `-DualSign` is specified, the script signs twice:

1. SHA-1 signature first (`/fd sha1`)
2. Append SHA-256 signature (`/fd sha256 /as`)

This is the typical pattern for “works on old Win7, but still has SHA-2 for newer systems”.

### Fallback behaviour (explicit opt-in)

Some CI runners may refuse SHA-1 certificate creation. If SHA-1 certificate creation fails, `ci/sign-drivers.ps1`:

- **fails by default**, or
- continues only if `-AllowSha2CertFallback` is provided, in which case it creates a SHA-256-signed certificate and prints a loud warning that **stock Win7 without KB3033929/KB4474419 may fail**.

