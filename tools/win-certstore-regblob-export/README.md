# win-certstore-regblob-export

Windows utility that derives the *exact* `SystemCertificates` registry representation (thumbprint subkey + `Blob` bytes and any other values the provider creates) for a given X.509 certificate and store name.

The `Blob` stored under:

`HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

is a CryptoAPI-serialized structure (not guaranteed to be raw DER). This tool generates it by asking CryptoAPI on Windows to create the registry-backed store entry, then exports the resulting registry value bytes as a portable patch (JSON and optional `.reg`).

## Usage

```powershell
# JSON to stdout
win-certstore-regblob-export --store ROOT .\cert.pem > patch.json

# JSON to stdout + a .reg snippet written to a file
win-certstore-regblob-export --store ROOT --reg-out .\patch.reg .\cert.pem > patch.json

# Only emit a .reg snippet to stdout
win-certstore-regblob-export --format reg --store TrustedPublisher .\certs.pem > patch.reg
```

The tool accepts PEM (including multiple `BEGIN CERTIFICATE` blocks) and raw DER files.

## JSON output

For a single `(store, certificate)` pair the output is a single JSON object:

```json
{
  "store": "ROOT",
  "thumbprint_sha1": "0123ABCD...",
  "values": {
    "Blob": "base64..."
  }
}
```

If multiple stores and/or multiple certificates are provided, the output is a JSON array of objects in the same shape.

## How it works

1. Creates a temporary writable registry key under `HKCU\Software\__win-certstore-regblob-export-...` (deleted on exit).
2. Opens that key as a registry-backed cert store via `CertOpenStore(CERT_STORE_PROV_SYSTEM_REGISTRY, ...)`.
3. Adds the certificate via `CertAddEncodedCertificateToStore`.
4. Reads back the resulting `Certificates\<thumbprint>` registry subkey and exports *all* values it contains.
5. Deletes the temporary key tree (no admin required).
