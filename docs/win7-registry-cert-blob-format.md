# Windows 7 CryptoAPI registry certificate `Blob` format (byte-level)

Windows 7 / WinPE system certificate stores persisted in the registry under:

`HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

store each certificate as a `REG_BINARY` value called `Blob`.

This document specifies the **exact** binary layout of that `Blob` value on
Windows 7, and how it relates to CryptoAPI serialization APIs.

## Ground truth / relationship to CryptoAPI

On Windows 7, the registry provider uses the same serialized form produced by:

- `CertSerializeCertificateStoreElement()` (for a `PCCERT_CONTEXT`)

and the resulting bytes can be round-tripped back into a store with:

- `CertAddSerializedElementToStore()`

The included harness (`tools/win-blob-dump/`) prints and validates:

1. The raw output of `CertSerializeCertificateStoreElement()`
2. A byte-for-byte comparison with the registry `Blob` value for the same cert

> Note: Some *container* formats (e.g. a serialized store file created via
> `CertSaveStore(CERT_STORE_SAVE_AS_STORE, ...)`) may wrap each element with an
> additional record header (length, context type, etc). The registry `Blob`
> value is just the per-certificate element bytes, not the outer container
> framing.

## High-level shape

The blob is **not** only `[dwEncodingType][cbCert][DER]`.

It is:

1. A fixed header (`dwCertEncodingType`, `cbCertEncoded`)
2. The DER-encoded certificate bytes
3. A persisted **property section** (count + property entries)

All integer fields are **little-endian**.

## Struct layout (packed, with 4-byte alignment rules)

```c
// Little-endian.
//
// Note: "alignment" here refers to *padding inside the serialized blob*,
// not in-memory struct padding. Windows aligns the start of the property
// section and each subsequent property entry to a 4-byte boundary.

struct Win7RegistryCertBlob {
    u32 dwCertEncodingType; // Usually 0x00010001 (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING)
    u32 cbCertEncoded;      // Length in bytes of pbCertEncoded (DER)
    u8  pbCertEncoded[cbCertEncoded];
    u8  pad0[(4 - (cbCertEncoded % 4)) % 4]; // zero bytes

    u32 cProperties;        // Count of persisted properties following
    Win7SerializedProperty properties[cProperties];
};

struct Win7SerializedProperty {
    u32 dwPropId;   // CERT_*_PROP_ID
    u32 cbValue;    // Length in bytes of value[] (not including padding)
    u8  value[cbValue];
    u8  pad[(4 - (cbValue % 4)) % 4]; // zero bytes
};
```

### Notes / invariants

- `dwCertEncodingType` is the same encoding value you would pass to
  `CertCreateCertificateContext()`.
  - In practice Windows uses `X509_ASN_ENCODING | PKCS_7_ASN_ENCODING` (`0x00010001`)
    for system stores.
- `pbCertEncoded` is the **raw DER certificate**, exactly as imported.
- `cProperties` counts only the properties that CryptoAPI considers **persistable**
  for that context at the time it is written.
- `dwPropId` ordering is the order produced by `CertEnumCertificateContextProperties`
  on Win7 (observed to be ascending numeric order in practice).
- The blob uses **4-byte padding**:
  - after the certificate DER
  - after each property value
  Padding bytes are `0x00`.

## Persisted properties: what you actually see

The persisted properties depend on how the cert was added/imported, but common
examples include:

| Property | ID | Typical meaning |
|---|---:|---|
| `CERT_KEY_PROV_INFO_PROP_ID` | 2 | Links the cert to a legacy CryptoAPI private key container (CSP) |
| `CERT_FRIENDLY_NAME_PROP_ID` | 11 | User-facing display name (UTF-16LE string, typically NUL-terminated) |
| `CERT_ARCHIVED_PROP_ID` | 19 | Whether a cert is archived in the store |

Property **values** are stored as opaque byte blobs; most are the same as
returned by `CertGetCertificateContextProperty()`, but some property IDs (notably
`CERT_KEY_PROV_INFO_PROP_ID`) use an internal serialized form suitable for
persistence (i.e., not raw process pointers).

### `CERT_FRIENDLY_NAME_PROP_ID` (11)

- Value is a UTF-16LE string (typically NUL-terminated).
- `cbValue` is the byte length of the UTF-16LE buffer stored in the blob.

### `CERT_KEY_PROV_INFO_PROP_ID` (2)

The `CERT_KEY_PROV_INFO` property is documented as a `CRYPT_KEY_PROV_INFO`
structure containing pointers, but the persisted bytes inside the registry
`Blob` must be architecture-independent.

The included harness prints a heuristic decode of the property value that
matches an **offset-based** serialization (32-bit offsets from the start of the
property value) of the form:

```c
// Little-endian, offsets are relative to the start of this value blob.
// Strings are UTF-16LE and typically NUL-terminated.
struct Win7PersistedCryptKeyProvInfo {
    u32 offContainerName;     // -> wchar_t[]
    u32 offProvName;          // -> wchar_t[]
    u32 dwProvType;
    u32 dwFlags;
    u32 cProvParam;
    u32 offProvParamArray;    // -> Win7PersistedCryptKeyProvParam[cProvParam] (or 0 if none)
    u32 dwKeySpec;
    // ...variable data (strings, params, param data)...
};

struct Win7PersistedCryptKeyProvParam {
    u32 dwParam;
    u32 offData;              // -> u8[cbData]
    u32 cbData;
    u32 dwFlags;
};
```

If you need to generate byte-identical `CERT_KEY_PROV_INFO_PROP_ID` payloads,
run the harness on Win7 and use the printed offsets/bytes as the reference.

## Annotated hexdumps (from the harness)

The harness (`tools/win-blob-dump/win_blob_dump.c`) prints full hexdumps.
Below are abbreviated, annotated excerpts that highlight field boundaries.

### Example A: no extra properties (`cProperties = 0`)

```
00000000: 01 00 01 00  90 02 00 00  30 82 02 8c ...
          ^^^^^^^^^^^  ^^^^^^^^^^^
          dwEncoding   cbCertEncoded = 0x00000290

00000298: 00 00 00 00
          ^^^^^^^^^^^
          cProperties = 0
```

### Example B: with persisted properties (`FriendlyName`, `KeyProvInfo`)

```
... (DER certificate bytes) ...

00000298: 02 00 00 00
          ^^^^^^^^^^^
          cProperties = 2

0000029c: 0b 00 00 00  1c 00 00 00  41 00 65 00 72 00 6f 00 ...
          ^^^^^^^^^^^  ^^^^^^^^^^^   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
          propId=11    cbValue=0x1c  UTF-16LE "Aero test friendly name\0"

... (possible 0x00 padding to 4-byte boundary) ...

000002c0: 02 00 00 00  ?? ?? ?? ??  ...
          ^^^^^^^^^^^  ^^^^^^^^^^^
          propId=2     cbValue = (KeyProvInfo bytes)
```

## How to generate byte-identical blobs outside Windows

To generate a Win7-compatible registry `Blob` for a cert:

1. Write the 8-byte header (`dwCertEncodingType`, `cbCertEncoded`).
2. Append the DER bytes.
3. Append `pad0` to a 4-byte boundary.
4. Append `cProperties` (DWORD).
5. For each persisted property:
   1. Append `dwPropId` (DWORD)
   2. Append `cbValue` (DWORD)
   3. Append `value` bytes
   4. Append zero padding to a 4-byte boundary

The main remaining complexity is producing byte-identical `value` bytes for
properties like `CERT_KEY_PROV_INFO_PROP_ID`. The harness is designed to make
this visible by printing the serialized property payloads.
