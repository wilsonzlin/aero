# 16 - Windows 7 Driver Build and Signing

This document collects practical notes for building, cataloging, and test-signing Windows drivers intended to run on Windows 7 SP1.

## Toolchain validation (Win7 Inf2Cat)

### Why we validate

Catalog generation is performed with `Inf2Cat.exe`. For Windows 7 we specifically need the OS tokens:

```
Inf2Cat /os:7_X86,7_X64
```

Not every Windows Kits / WDK release has historically accepted older `/os:` tokens, and GitHub Actions runner images can change over time. A failing catalog-generation step is easy to miss until late in the build pipeline, so we validate it explicitly.

### Pinned Windows Kits version

CI pins the Windows Kits toolchain to:

- **Windows Kits 10.0.22621.0** (Windows 11 / Windows 10 22H2-era toolset)

The pin is implemented in `ci/install-wdk.ps1` (which installs the Windows SDK/WDK via `winget` on CI if needed) and verified by `ci/validate-toolchain.ps1`.

### How to run locally (Windows)

From PowerShell:

```powershell
./ci/install-wdk.ps1
./ci/validate-toolchain.ps1
```

The scripts write logs/artifacts under:

- `out/toolchain.json` (resolved tool paths)
- `out/toolchain-validation/` (validation transcript + Inf2Cat output)

### CI workflow

The GitHub Actions workflow `.github/workflows/toolchain-win7-smoke.yml` runs on `windows-latest` and:

1. Resolves/installs the pinned toolchain
2. Prints tool versions (`Inf2Cat`, `signtool`, `stampinf`, `msbuild`)
3. Generates a minimal dummy driver package and runs `Inf2Cat /os:7_X86,7_X64`
4. Uploads the logs as workflow artifacts

## CI test signing

`ci/sign-drivers.ps1` is intended for CI runners. It:

1. Creates a self-signed **code signing** certificate.
2. Uses `signtool` to sign every `*.sys` and `*.cat` under `out/packages/**` (configurable via `-InputRoot`).
3. Verifies signatures using `signtool verify`:
   - `.sys`: `signtool verify /kp /v`
   - `.cat`: `signtool verify /v`

The script imports the public cert into the current user **Trusted Root** and
**Trusted Publishers** stores (and will also try LocalMachine stores when allowed)
so `signtool verify` can succeed.

Outputs:

- Public cert (artifact-safe): `out/certs/aero-test.cer`
- Signing PFX (private key): `out/aero-test.pfx` (kept under `out/`, not `out/certs/`)

Typical usage:

```powershell
.\ci\sign-drivers.ps1
```

Dual signing (SHA-1 first, then append SHA-256):

```powershell
.\ci\sign-drivers.ps1 -DualSign
```

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
