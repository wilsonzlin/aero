# Windows 7 CryptoAPI registry certificate `Blob` format (byte-level)

Windows 7 / WinPE system certificate stores persisted in the registry under:

`HKLM\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

store each certificate as a `REG_BINARY` value called `Blob`.

Per-user system stores use the same format under:

`HKCU\SOFTWARE\Microsoft\SystemCertificates\<STORE>\Certificates\<SHA1>\Blob`

This document specifies the **exact** binary layout of that `Blob` value on
Windows 7, and how it relates to CryptoAPI serialization APIs.

## Ground truth / relationship to CryptoAPI

On Windows 7, the registry provider uses the same serialized form produced by:

- `CertSerializeCertificateStoreElement()` (for a `PCCERT_CONTEXT`)

and the resulting bytes can be round-tripped back into a store with:

- `CertAddSerializedElementToStore()`

The included harness (`tools/win-blob-dump/`) prints and validates:

1. The raw output of `CertSerializeCertificateStoreElement()`
2. A byte-for-byte comparison with the registry `Blob` value for the same cert:
   - `HKCU\...` always (current user store)
   - `HKLM\...` when run with sufficient privileges (local machine store)

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
- The blob uses **4-byte padding** (0x00 bytes) to align the following DWORD:
  - after the certificate DER
  - after each property value

## Padding / alignment examples

### Padding after DER (`cbCertEncoded % 4 != 0`)

Example certificate (unaligned length):

- File: `tools/win-blob-dump/examples/example_unaligned_cert.der`
- `cbCertEncoded = 506 (0x01fa)`

The next DWORD (`cProperties`) starts at the next 4-byte boundary, so after the
DER bytes there are 2 bytes of `0x00` padding:

```text
... (last 16 bytes of DER) ...
000001f2: 74 55 c0 65 5b ee f2 b3 5f 72 4f 89 52 9a f5 15 |tU.e[..._rO.R...|
00000202: 00 00 00 00 00 00                               |......|
          ^^ ^^
          pad0 = 2 bytes
                ^^^^^^^^^^^
                cProperties = 0 (DWORD)
```

### Padding after a property value (`cbValue % 4 != 0`)

Example: a `CERT_FRIENDLY_NAME_PROP_ID` value of `"Aero\0"` has:

- `cbValue = 10 (0x0a)` bytes (UTF-16LE)
- followed by 2 bytes of padding to align the next property header to a DWORD boundary

```text
00000224: 01 00 00 00 0b 00 00 00 0a 00 00 00 41 00 65 00 |............A.e.|
          ^^^^^^^^^^^
          cProperties = 1
                      ^^^^^^^^^^^  ^^^^^^^^^^^
                      propId = 11  cbValue = 10
                                            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                                            UTF-16LE "Aero\0"
00000234: 72 00 6f 00 00 00 00 00                         |r.o.....|
                    ^^^^^^
                    terminator (00 00)
                          ^^ ^^
                          padding (2 bytes)
```

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
Below are full hexdumps for a small, reproducible example certificate, laid out
according to the format described in this document (i.e. these are the expected
bytes for that input).

Example certificate:

- File: `tools/win-blob-dump/examples/example_cert.der`
- SHA1 (thumbprint / registry key name): `FDA7D93129AF9CE5317A0FA9CD466FB562A3982C`
- `cbCertEncoded = 540 (0x21c)`

### Example A: no extra properties (`cProperties = 0`)

Notes:

- `dwCertEncodingType = 0x00010001` (`X509_ASN_ENCODING | PKCS_7_ASN_ENCODING`)
- `cbCertEncoded = 0x0000021c`
- Since `cbCertEncoded % 4 == 0`, there is **no** padding after DER before `cProperties`.

```text
00000000: 01 00 01 00 1c 02 00 00 30 82 02 18 30 82 01 81 |........0...0...|
00000010: a0 03 02 01 02 02 14 27 ea 81 00 1b f1 cd 55 50 |.......'......UP|
00000020: 68 8b d0 8a 3f b4 af ab b3 59 31 30 0d 06 09 2a |h...?....Y10...*|
00000030: 86 48 86 f7 0d 01 01 0b 05 00 30 1e 31 1c 30 1a |.H........0.1.0.|
00000040: 06 03 55 04 03 0c 13 41 65 72 6f 42 6c 6f 62 44 |..U....AeroBlobD|
00000050: 75 6d 70 45 78 61 6d 70 6c 65 30 1e 17 0d 32 36 |umpExample0...26|
00000060: 30 31 31 30 31 32 30 35 34 32 5a 17 0d 33 36 30 |0110120542Z..360|
00000070: 31 30 38 31 32 30 35 34 32 5a 30 1e 31 1c 30 1a |108120542Z0.1.0.|
00000080: 06 03 55 04 03 0c 13 41 65 72 6f 42 6c 6f 62 44 |..U....AeroBlobD|
00000090: 75 6d 70 45 78 61 6d 70 6c 65 30 81 9f 30 0d 06 |umpExample0..0..|
000000a0: 09 2a 86 48 86 f7 0d 01 01 01 05 00 03 81 8d 00 |.*.H............|
000000b0: 30 81 89 02 81 81 00 f5 a7 65 2c e8 85 f2 0b ad |0........e,.....|
000000c0: 5f b4 a9 ae f4 eb ba 3a ef 2e 81 e0 de cf 31 54 |_......:......1T|
000000d0: d4 4e 3c 22 59 01 0c 67 ba af e1 ee 0c b6 55 fd |.N<"Y..g......U.|
000000e0: 1c 5c 51 53 ee 5c ef cf 04 9e e7 36 f7 ab c5 98 |.\QS.\.....6....|
000000f0: a5 e4 e7 d1 3e 2d 96 00 7a d5 c6 cd 13 e1 83 05 |....>-..z.......|
00000100: 16 cb af 5d 77 2e ba 0f 2b 00 7c 12 d1 2e 4a 79 |...]w...+.|...Jy|
00000110: 68 14 3d 34 15 63 94 f3 e5 71 b0 be 60 d4 01 c1 |h.=4.c...q..`...|
00000120: c6 8d cc d6 4b 7f c4 ce 91 6b a6 9d 4a c2 c0 c0 |....K....k..J...|
00000130: 25 ba b3 12 19 3a a7 02 03 01 00 01 a3 53 30 51 |%....:.......S0Q|
00000140: 30 1d 06 03 55 1d 0e 04 16 04 14 8d 61 90 7f 9f |0...U.......a...|
00000150: 0b fa 51 23 b5 14 40 8a 69 11 67 e9 2e bc f4 30 |..Q#..@.i.g....0|
00000160: 1f 06 03 55 1d 23 04 18 30 16 80 14 8d 61 90 7f |...U.#..0....a..|
00000170: 9f 0b fa 51 23 b5 14 40 8a 69 11 67 e9 2e bc f4 |...Q#..@.i.g....|
00000180: 30 0f 06 03 55 1d 13 01 01 ff 04 05 30 03 01 01 |0...U.......0...|
00000190: ff 30 0d 06 09 2a 86 48 86 f7 0d 01 01 0b 05 00 |.0...*.H........|
000001a0: 03 81 81 00 df 96 50 6f 7c 89 1b f4 60 17 be 64 |......Po|...`..d|
000001b0: af d1 65 86 1c 08 a9 2f b3 20 fe 0e 57 07 f3 c0 |..e..../. ..W...|
000001c0: ed 90 71 03 f0 49 14 42 7c 1d 60 7b 4f 1a ce a6 |..q..I.B|.`{O...|
000001d0: 49 f9 60 0a a5 37 18 76 6f 79 ae 19 75 6d 56 0f |I.`..7.voy..umV.|
000001e0: 3c 03 3d 32 0d dd bd a0 0a 84 7f 54 76 fd 8e 00 |<.=2.......Tv...|
000001f0: 3a 6f 68 71 25 f9 6b e2 39 ff 3b 4b ac 9d 92 0a |:ohq%.k.9.;K....|
00000200: 57 14 33 0c d4 44 24 9f cf 52 a2 37 0d 73 26 bc |W.3..D$..R.7.s&.|
00000210: ab 1e 27 ef a3 50 39 f3 a8 6b d6 db c5 d0 17 03 |..'..P9..k......|
00000220: fb 5a 5d db 00 00 00 00                         |.Z].....|
```

### Example B: with a persisted property (`FriendlyName`, `cProperties = 1`)

Notes:

- `cProperties = 1`
- One property entry is appended:
  - `dwPropId = 11` (`CERT_FRIENDLY_NAME_PROP_ID`)
  - `cbValue = 0x28`
  - Value is UTF-16LE `"AeroBlobDumpExample\0"`

```text
00000000: 01 00 01 00 1c 02 00 00 30 82 02 18 30 82 01 81 |........0...0...|
00000010: a0 03 02 01 02 02 14 27 ea 81 00 1b f1 cd 55 50 |.......'......UP|
00000020: 68 8b d0 8a 3f b4 af ab b3 59 31 30 0d 06 09 2a |h...?....Y10...*|
00000030: 86 48 86 f7 0d 01 01 0b 05 00 30 1e 31 1c 30 1a |.H........0.1.0.|
00000040: 06 03 55 04 03 0c 13 41 65 72 6f 42 6c 6f 62 44 |..U....AeroBlobD|
00000050: 75 6d 70 45 78 61 6d 70 6c 65 30 1e 17 0d 32 36 |umpExample0...26|
00000060: 30 31 31 30 31 32 30 35 34 32 5a 17 0d 33 36 30 |0110120542Z..360|
00000070: 31 30 38 31 32 30 35 34 32 5a 30 1e 31 1c 30 1a |108120542Z0.1.0.|
00000080: 06 03 55 04 03 0c 13 41 65 72 6f 42 6c 6f 62 44 |..U....AeroBlobD|
00000090: 75 6d 70 45 78 61 6d 70 6c 65 30 81 9f 30 0d 06 |umpExample0..0..|
000000a0: 09 2a 86 48 86 f7 0d 01 01 01 05 00 03 81 8d 00 |.*.H............|
000000b0: 30 81 89 02 81 81 00 f5 a7 65 2c e8 85 f2 0b ad |0........e,.....|
000000c0: 5f b4 a9 ae f4 eb ba 3a ef 2e 81 e0 de cf 31 54 |_......:......1T|
000000d0: d4 4e 3c 22 59 01 0c 67 ba af e1 ee 0c b6 55 fd |.N<"Y..g......U.|
000000e0: 1c 5c 51 53 ee 5c ef cf 04 9e e7 36 f7 ab c5 98 |.\QS.\.....6....|
000000f0: a5 e4 e7 d1 3e 2d 96 00 7a d5 c6 cd 13 e1 83 05 |....>-..z.......|
00000100: 16 cb af 5d 77 2e ba 0f 2b 00 7c 12 d1 2e 4a 79 |...]w...+.|...Jy|
00000110: 68 14 3d 34 15 63 94 f3 e5 71 b0 be 60 d4 01 c1 |h.=4.c...q..`...|
00000120: c6 8d cc d6 4b 7f c4 ce 91 6b a6 9d 4a c2 c0 c0 |....K....k..J...|
00000130: 25 ba b3 12 19 3a a7 02 03 01 00 01 a3 53 30 51 |%....:.......S0Q|
00000140: 30 1d 06 03 55 1d 0e 04 16 04 14 8d 61 90 7f 9f |0...U.......a...|
00000150: 0b fa 51 23 b5 14 40 8a 69 11 67 e9 2e bc f4 30 |..Q#..@.i.g....0|
00000160: 1f 06 03 55 1d 23 04 18 30 16 80 14 8d 61 90 7f |...U.#..0....a..|
00000170: 9f 0b fa 51 23 b5 14 40 8a 69 11 67 e9 2e bc f4 |...Q#..@.i.g....|
00000180: 30 0f 06 03 55 1d 13 01 01 ff 04 05 30 03 01 01 |0...U.......0...|
00000190: ff 30 0d 06 09 2a 86 48 86 f7 0d 01 01 0b 05 00 |.0...*.H........|
000001a0: 03 81 81 00 df 96 50 6f 7c 89 1b f4 60 17 be 64 |......Po|...`..d|
000001b0: af d1 65 86 1c 08 a9 2f b3 20 fe 0e 57 07 f3 c0 |..e..../. ..W...|
000001c0: ed 90 71 03 f0 49 14 42 7c 1d 60 7b 4f 1a ce a6 |..q..I.B|.`{O...|
000001d0: 49 f9 60 0a a5 37 18 76 6f 79 ae 19 75 6d 56 0f |I.`..7.voy..umV.|
000001e0: 3c 03 3d 32 0d dd bd a0 0a 84 7f 54 76 fd 8e 00 |<.=2.......Tv...|
000001f0: 3a 6f 68 71 25 f9 6b e2 39 ff 3b 4b ac 9d 92 0a |:ohq%.k.9.;K....|
00000200: 57 14 33 0c d4 44 24 9f cf 52 a2 37 0d 73 26 bc |W.3..D$..R.7.s&.|
00000210: ab 1e 27 ef a3 50 39 f3 a8 6b d6 db c5 d0 17 03 |..'..P9..k......|
00000220: fb 5a 5d db 01 00 00 00 0b 00 00 00 28 00 00 00 |.Z].........(...|
00000230: 41 00 65 00 72 00 6f 00 42 00 6c 00 6f 00 62 00 |A.e.r.o.B.l.o.b.|
00000240: 44 00 75 00 6d 00 70 00 45 00 78 00 61 00 6d 00 |D.u.m.p.E.x.a.m.|
00000250: 70 00 6c 00 65 00 00 00                         |p.l.e...|
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
