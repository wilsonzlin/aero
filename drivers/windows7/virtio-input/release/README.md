# Aero virtio-input (Windows 7) release packaging

This folder documents how to produce a redistributable driver bundle once you have built the driver binaries.

## What the packaging script does

`../scripts/package-release.ps1` creates a zip containing:

- The built driver `*.sys` for the selected architecture(s)
- The matching `*.inf` from `drivers/windows7/virtio-input/inf/` (see naming below)
- The matching `*.cat` if present (either next to the INF, or under `-InputDir`)
- A KMDF coinstaller `WdfCoInstaller*.dll` **if present** (either referenced by the INF, or discovered under `-InputDir`)
- An `INSTALL.txt` with minimal Windows 7 test-signing + “Have Disk…” install steps
- (Optional) the public **test-signing certificate** `aero-virtio-input-test.cer` when `-IncludeTestCert` is specified
- A `manifest.json` describing file hashes + metadata (driver id, arch, version, etc.)
- A `SHA256SUMS` file containing SHA-256 for every file in the zip (including `manifest.json`) for easy integrity checks

> Default policy: the Win7 virtio-input driver targets **KMDF 1.9** (in-box on Windows 7 SP1), so no WDF coinstaller
> is expected or required. The packaging script only includes `WdfCoInstaller*.dll` if you intentionally add one (for
> example, after rebuilding the driver against KMDF > 1.9).

The output filename is:

`aero-virtio-input-win7-<arch>-<version>.zip`

`<arch>` is `x86` or `amd64` (you can pass `x64` as an alias for `amd64`). The zip name and `manifest.json` always use the normalized `amd64` spelling.

The `<version>` value is derived from the `DriverVer=...,<version>` line in the packaged INF.

## Usage

From the repository root:

```powershell
# Package both architectures (produces two zips)
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/package-release.ps1 `
  -Arch both `
  -InputDir <path-to-built-binaries> `
  -OutDir drivers/windows7/virtio-input/release/out

# Package and include the test certificate (for manual Win7 test installs)
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/package-release.ps1 `
  -Arch amd64 `
  -InputDir <path-to-built-binaries> `
  -OutDir drivers/windows7/virtio-input/release/out `
  -IncludeTestCert

# Package a single architecture
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/package-release.ps1 `
  -Arch x64 `
  -InputDir <path-to-built-binaries> `
  -OutDir drivers/windows7/virtio-input/release/out
```

`release/out/` and generated `*.zip` files are ignored by git (see `release/.gitignore`).

## Verifying a packaged zip

`../scripts/verify-release.ps1` validates a packaged release zip by extracting it to a temporary directory and checking every file listed in `manifest.json`:

- file exists
- file size matches
- SHA-256 hash matches

It also validates `schemaVersion == 1`, `driver.id == aero-virtio-input`, and `driver.targetOs == win7`.

```powershell
powershell -ExecutionPolicy Bypass -File drivers/windows7/virtio-input/scripts/verify-release.ps1 `
  -ZipPath drivers/windows7/virtio-input/release/out/aero-virtio-input-win7-amd64-<version>.zip
```

## One-shot local workflow (optional)

If you want a single command that runs the typical local signing flow:

1. stage SYS into `inf/`
2. generate the catalog (`Inf2Cat`)
3. sign (`signtool`)
4. produce ZIP(s)

Use:

```powershell
# From drivers/windows7/virtio-input/
powershell -ExecutionPolicy Bypass -File .\scripts\build-release.ps1 -Arch both -InputDir <path-to-built-binaries>
```

### `-InputDir` expectations

`-InputDir` should point at a directory containing your built artifacts. The script searches **recursively** under `-InputDir` for:

- The driver SYS file named by the INF (`ServiceBinary=...\.sys`) (fallback: `aero_virtio_input.sys`)
- The catalog file named by the INF (`CatalogFile=...\.cat`) (optional)
- A KMDF coinstaller DLL (`WdfCoInstaller*.dll`) (optional)
- When `-IncludeTestCert` is specified: a `*.cer` file (preferably `aero-virtio-input-test.cer`)

If multiple matching files exist (e.g. because multiple build outputs are present), the script will fail with a list of candidates to keep packaging deterministic.

### Test certificate inclusion

If you pass `-IncludeTestCert`, `package-release.ps1` will copy a public certificate into the zip as:

`aero-virtio-input-test.cer`

It searches in this order:

1. `drivers/windows7/virtio-input/cert/aero-virtio-input-test.cer` (created by `scripts/make-cert.ps1`), otherwise
2. any `*.cer` found under `-InputDir`

The private key (`*.pfx`) is **never** included.

### INF naming

The script looks for either:

- `drivers/windows7/virtio-input/inf/aero_virtio_input.inf` (unified INF), or
- `drivers/windows7/virtio-input/inf/aero_virtio_input-<arch>.inf` (per-arch INF)

## Notes

- The script is safe to run before the driver exists: it will emit clear errors if required files (INF/SYS) are missing.
- The zip includes a `manifest.json` so consumers can verify exactly what was shipped.
- The zip also includes `SHA256SUMS`, so after extracting you can run:
  - `sha256sum -c SHA256SUMS` (Linux/macOS, or Windows environments that provide `sha256sum`)
  - PowerShell (rough equivalent):
    ```powershell
    Get-Content .\SHA256SUMS | ForEach-Object {
      if ($_ -match '^([0-9a-fA-F]{64})\\s\\s(.+)$') {
        $expected = $matches[1].ToLowerInvariant()
        $file = $matches[2]
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
        if ($actual -ne $expected) { throw "$file: FAILED" }
      }
    }
    ```
