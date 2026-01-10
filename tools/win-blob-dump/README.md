# win-blob-dump (Windows-only)

Small harness for reverse-engineering / validating the binary format of the
registry `Blob` value used by the Windows 7 CryptoAPI system certificate store:

`HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

It:

1. Loads a DER-encoded X.509 certificate from disk.
2. Optionally sets persisted cert properties (`FriendlyName`, `KeyProvInfo`).
3. Calls `CertSerializeCertificateStoreElement()` and prints a hexdump +
   decoded offsets.
4. Round-trips the serialized bytes via `CertAddSerializedElementToStore()`.
5. Adds the cert to a real registry-backed system store (current user), reads
   the registry `Blob` value, and compares it byte-for-byte with the serialized
   output.

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

The harness uses a dedicated per-user test store called `AERO_BLOB_DUMP` and
attempts to delete the added certificate when done.

