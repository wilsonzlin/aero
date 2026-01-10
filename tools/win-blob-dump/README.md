# win-blob-dump (Windows-only)

Small harness for reverse-engineering / validating the binary format of the
registry `Blob` value used by the Windows 7 CryptoAPI system certificate store:

`HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

It:

1. Loads a DER-encoded X.509 certificate from disk.
2. Optionally sets persisted cert properties (`FriendlyName`, `KeyProvInfo`).
3. Serializes and dumps multiple variants:
   - no persisted properties
   - `FriendlyName` only
   - `FriendlyName` + `KeyProvInfo` (best-effort)
4. Calls `CertSerializeCertificateStoreElement()` and prints a hexdump +
   decoded offsets.
5. Round-trips the serialized bytes via `CertAddSerializedElementToStore()`.
6. Adds the cert to real registry-backed system stores and compares the registry
   `Blob` bytes to the serialized output:
   - current user store (`HKCU\Software\Microsoft\SystemCertificates\...`)
   - local machine store (`HKLM\Software\Microsoft\SystemCertificates\...`, requires admin)

## Build (MSVC)

Open a "Developer Command Prompt for VS" and run:

```bat
cd tools\win-blob-dump
cl /nologo /W4 /DUNICODE /D_UNICODE win_blob_dump.c ^
  /link crypt32.lib advapi32.lib
```

## Build (MinGW-w64)

```sh
cd tools/win-blob-dump
gcc -Wall -Wextra -municode -o win_blob_dump.exe win_blob_dump.c -lcrypt32 -ladvapi32
```

## Run

```bat
win_blob_dump.exe path\to\cert.der
```

An example DER certificate is included at:

`tools/win-blob-dump/examples/example_cert.der`

Another example certificate with a DER length that is *not* 4-byte aligned is
included at:

`tools/win-blob-dump/examples/example_unaligned_cert.der`

so from the repo root you can run:

```bat
tools\win-blob-dump\win_blob_dump.exe tools\win-blob-dump\examples\example_cert.der
```

The harness uses a dedicated per-user test store called `AERO_BLOB_DUMP` and
attempts to delete the added certificate when done.
