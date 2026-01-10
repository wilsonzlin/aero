#define UNICODE
#define _UNICODE

#include <windows.h>
#include <wincrypt.h>

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

// This tool is intentionally "old school" C + CryptoAPI to keep it compatible
// with Windows 7 / WinPE and easy to compile without extra dependencies.

// MinGW / older SDKs vary slightly in which helper macros they expose. Define
// the context-type bitflags we need if they're missing.
#ifndef CERT_STORE_CERTIFICATE_CONTEXT_FLAG
#define CERT_STORE_CERTIFICATE_CONTEXT_FLAG 0x00000001
#endif

static void die_win32(const wchar_t *what) {
    DWORD err = GetLastError();
    fwprintf(stderr, L"%ls failed (GetLastError=%lu)\n", what, (unsigned long)err);
    ExitProcess(1);
}

static void die(const wchar_t *what) {
    fwprintf(stderr, L"%ls\n", what);
    ExitProcess(1);
}

static size_t align4(size_t n) {
    return (n + 3u) & ~3u;
}

static uint32_t read_u32le(const BYTE *p) {
    return ((uint32_t)p[0]) | ((uint32_t)p[1] << 8u) | ((uint32_t)p[2] << 16u) |
           ((uint32_t)p[3] << 24u);
}

static const char *cert_prop_name(DWORD propId) {
    switch (propId) {
    case CERT_KEY_PROV_INFO_PROP_ID:
        return "CERT_KEY_PROV_INFO_PROP_ID";
    case CERT_SHA1_HASH_PROP_ID:
        return "CERT_SHA1_HASH_PROP_ID";
    case CERT_FRIENDLY_NAME_PROP_ID:
        return "CERT_FRIENDLY_NAME_PROP_ID";
    case CERT_ARCHIVED_PROP_ID:
        return "CERT_ARCHIVED_PROP_ID";
    default:
        return NULL;
    }
}

static void hexdump(const BYTE *buf, DWORD len) {
    for (DWORD off = 0; off < len; off += 16) {
        printf("%08lx: ", (unsigned long)off);
        for (DWORD i = 0; i < 16; i++) {
            if (off + i < len) {
                printf("%02x ", buf[off + i]);
            } else {
                printf("   ");
            }
        }
        printf(" |");
        for (DWORD i = 0; i < 16 && off + i < len; i++) {
            BYTE c = buf[off + i];
            if (c >= 0x20 && c <= 0x7e) {
                putchar((char)c);
            } else {
                putchar('.');
            }
        }
        printf("|\n");
    }
}

static void print_thumbprint_hex(PCCERT_CONTEXT cert, wchar_t out_hex[41]) {
    BYTE hash[20];
    DWORD cbHash = sizeof(hash);
    if (!CertGetCertificateContextProperty(cert, CERT_SHA1_HASH_PROP_ID, hash, &cbHash)) {
        die_win32(L"CertGetCertificateContextProperty(CERT_SHA1_HASH_PROP_ID)");
    }
    if (cbHash != 20) {
        die(L"unexpected SHA1 hash length");
    }
    static const wchar_t *hex = L"0123456789ABCDEF";
    for (DWORD i = 0; i < 20; i++) {
        out_hex[i * 2 + 0] = hex[(hash[i] >> 4) & 0xF];
        out_hex[i * 2 + 1] = hex[hash[i] & 0xF];
    }
    out_hex[40] = 0;
}

static void hexdump_limit(const BYTE *buf, DWORD len, DWORD max_len) {
    DWORD n = len;
    if (n > max_len) {
        n = max_len;
    }
    hexdump(buf, n);
    if (n != len) {
        printf("(truncated; total %lu bytes)\n", (unsigned long)len);
    }
}

static int try_print_utf16le_string(const BYTE *buf, DWORD len) {
    if ((len % 2) != 0) {
        return 0;
    }
    const wchar_t *ws = (const wchar_t *)buf;
    DWORD cch = len / 2;
    DWORD max = cch;
    // Prefer to stop on NUL if present.
    for (DWORD i = 0; i < cch; i++) {
        if (ws[i] == 0) {
            max = i;
            break;
        }
    }

    int cbUtf8 = WideCharToMultiByte(CP_UTF8, 0, ws, (int)max, NULL, 0, NULL, NULL);
    if (cbUtf8 <= 0) {
        return 0;
    }
    char *utf8 = (char *)malloc((size_t)cbUtf8 + 1);
    if (!utf8) {
        die(L"malloc failed");
    }
    if (WideCharToMultiByte(CP_UTF8, 0, ws, (int)max, utf8, cbUtf8, NULL, NULL) != cbUtf8) {
        free(utf8);
        return 0;
    }
    utf8[cbUtf8] = 0;
    printf("%s", utf8);
    free(utf8);
    return 1;
}

static void dump_key_prov_info_guess(const BYTE *val, DWORD cbVal) {
    // The CERT_KEY_PROV_INFO property returned by CertGetCertificateContextProperty
    // is documented as a CRYPT_KEY_PROV_INFO with pointers, but the persisted form
    // inside serialized store elements must be architecture-independent.
    //
    // On Windows 7 this appears to be a 32-bit "offset-based" serialization:
    //
    //   u32 offContainerName;
    //   u32 offProvName;
    //   u32 dwProvType;
    //   u32 dwFlags;
    //   u32 cProvParam;
    //   u32 offProvParamArray; // array of serialized CRYPT_KEY_PROV_PARAM
    //   u32 dwKeySpec;
    //
    // followed by UTF-16LE strings and optional provider params.
    //
    // This function decodes that layout heuristically. If the heuristics don't
    // match the blob, it prints nothing.
    if (cbVal < 28) {
        return;
    }
    uint32_t offContainer = read_u32le(val + 0);
    uint32_t offProv = read_u32le(val + 4);
    uint32_t dwProvType = read_u32le(val + 8);
    uint32_t dwFlags = read_u32le(val + 12);
    uint32_t cProvParam = read_u32le(val + 16);
    uint32_t offParams = read_u32le(val + 20);
    uint32_t dwKeySpec = read_u32le(val + 24);

    if (offContainer >= cbVal || offProv >= cbVal) {
        return;
    }
    if ((offContainer % 2) != 0 || (offProv % 2) != 0) {
        return;
    }

    printf("    KeyProvInfo (heuristic decode):\n");
    printf("      dwProvType  = %lu (0x%lx)\n", (unsigned long)dwProvType,
           (unsigned long)dwProvType);
    printf("      dwFlags     = %lu (0x%lx)\n", (unsigned long)dwFlags, (unsigned long)dwFlags);
    printf("      dwKeySpec   = %lu (0x%lx)\n", (unsigned long)dwKeySpec,
           (unsigned long)dwKeySpec);
    printf("      cProvParam  = %lu\n", (unsigned long)cProvParam);
    printf("      offContainerName = 0x%lx\n", (unsigned long)offContainer);
    printf("      offProvName      = 0x%lx\n", (unsigned long)offProv);
    printf("      offProvParamArr  = 0x%lx\n", (unsigned long)offParams);

    printf("      ContainerName = ");
    if (!try_print_utf16le_string(val + offContainer, cbVal - offContainer)) {
        printf("(unprintable)\n");
    } else {
        printf("\n");
    }
    printf("      ProviderName  = ");
    if (!try_print_utf16le_string(val + offProv, cbVal - offProv)) {
        printf("(unprintable)\n");
    } else {
        printf("\n");
    }

    if (cProvParam != 0 && offParams != 0 && offParams < cbVal) {
        // Guess that params are an array of 16-byte entries:
        //   dwParam, offData, cbData, dwFlags
        size_t off = offParams;
        for (uint32_t i = 0; i < cProvParam; i++) {
            if (off + 16 > cbVal) {
                break;
            }
            uint32_t dwParam = read_u32le(val + off + 0);
            uint32_t offData = read_u32le(val + off + 4);
            uint32_t cbData = read_u32le(val + off + 8);
            uint32_t dwPFlags = read_u32le(val + off + 12);
            printf("      ProvParam[%lu]: dwParam=%lu offData=0x%lx cbData=%lu dwFlags=0x%lx\n",
                   (unsigned long)i, (unsigned long)dwParam, (unsigned long)offData,
                   (unsigned long)cbData, (unsigned long)dwPFlags);
            if (offData < cbVal && offData + cbData <= cbVal) {
                printf("        data (first 64 bytes):\n");
                hexdump_limit(val + offData, cbData, 64);
            }
            off += 16;
        }
    }
}

static void dump_serialized_cert_blob(const BYTE *buf, DWORD len) {
    if (len < 8) {
        printf("serialized blob too short (%lu)\n", (unsigned long)len);
        return;
    }

    // Serialized certificates are expected to start with:
    //   [dwCertEncodingType][cbCertEncoded][DER...]
    //
    // However, some serialized-store containers wrap elements with an additional
    // DWORD context type. Detect that here so the dump stays useful even if the
    // caller fed us a wrapped blob.
    uint32_t v0 = read_u32le(buf + 0);
    uint32_t v1 = read_u32le(buf + 4);
    uint32_t dwContextType = 0;
    uint32_t dwEncodingType = 0;
    uint32_t cbCert = 0;
    size_t cert_off = 0;

    if ((v0 == CERT_STORE_CERTIFICATE_CONTEXT || v0 == CERT_STORE_CRL_CONTEXT ||
         v0 == CERT_STORE_CTL_CONTEXT) &&
        (v1 == X509_ASN_ENCODING || v1 == (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING) ||
         v1 == PKCS_7_ASN_ENCODING)) {
        if (len < 12) {
            printf("serialized blob too short for wrapped header (%lu)\n", (unsigned long)len);
            return;
        }
        dwContextType = v0;
        dwEncodingType = v1;
        cbCert = read_u32le(buf + 8);
        cert_off = 12;
        printf("Decoded (NOTE: blob contains a leading context-type DWORD):\n");
        printf("  [0x0000] dwContextType      = 0x%08lx\n", (unsigned long)dwContextType);
        printf("  [0x0004] dwCertEncodingType = 0x%08lx\n", (unsigned long)dwEncodingType);
        printf("  [0x0008] cbCertEncoded      = 0x%08lx (%lu)\n", (unsigned long)cbCert,
               (unsigned long)cbCert);
        printf("  [0x000c] pbCertEncoded      = DER bytes\n");
    } else {
        dwEncodingType = v0;
        cbCert = v1;
        cert_off = 8;
        printf("Decoded:\n");
        printf("  [0x0000] dwCertEncodingType = 0x%08lx\n", (unsigned long)dwEncodingType);
        printf("  [0x0004] cbCertEncoded      = 0x%08lx (%lu)\n", (unsigned long)cbCert,
               (unsigned long)cbCert);
        printf("  [0x0008] pbCertEncoded      = DER bytes\n");
    }

    size_t off = cert_off;
    if (off + cbCert > len) {
        printf("  ERROR: cbCertEncoded exceeds total blob length\n");
        return;
    }
    off += cbCert;

    // Empirically, Windows stores the next DWORD on a 4-byte boundary. If the
    // certificate length is not a multiple of 4, padding bytes are inserted.
    size_t off_aligned = align4(off);
    if (off_aligned > len) {
        printf("  ERROR: alignment pushes past end\n");
        return;
    }
    if (off_aligned != off) {
        printf("  [0x%04zx] padding after DER: %zu byte(s)\n", off, off_aligned - off);
        off = off_aligned;
    }

    if (off + 4 > len) {
        printf("  [0x%04zx] (no room for property section)\n", off);
        return;
    }

    DWORD cProps = (DWORD)read_u32le(buf + off);
    printf("  [0x%04zx] cProperties       = %lu\n", off, (unsigned long)cProps);
    off += 4;

    for (DWORD i = 0; i < cProps; i++) {
        if (off + 8 > len) {
            printf("  ERROR: truncated property header at 0x%zx\n", off);
            return;
        }
        DWORD propId = (DWORD)read_u32le(buf + off);
        DWORD cbProp = (DWORD)read_u32le(buf + off + 4);
        const char *name = cert_prop_name(propId);
        printf("  [0x%04zx] Property[%lu].dwPropId  = %lu (0x%lx)", off, (unsigned long)i,
               (unsigned long)propId, (unsigned long)propId);
        if (name) {
            printf(" [%s]", name);
        }
        printf("\n");
        printf("  [0x%04zx] Property[%lu].cbValue   = %lu (0x%lx)\n", off + 4,
               (unsigned long)i, (unsigned long)cbProp, (unsigned long)cbProp);
        off += 8;
        if (off + cbProp > len) {
            printf("  ERROR: property value overruns blob at 0x%zx\n", off);
            return;
        }

        if (propId == CERT_FRIENDLY_NAME_PROP_ID) {
            // FriendlyName is UTF-16LE, usually NUL-terminated.
            printf("  [0x%04zx] Property[%lu].value (FriendlyName UTF-16LE): ", off,
                   (unsigned long)i);
            if (!try_print_utf16le_string(buf + off, cbProp)) {
                printf("(unprintable)");
            }
            printf("\n");
        }
        if (propId == CERT_KEY_PROV_INFO_PROP_ID) {
            dump_key_prov_info_guess(buf + off, cbProp);
        }

        printf("  [0x%04zx] Property[%lu].value bytes (first 128):\n", off,
               (unsigned long)i);
        hexdump_limit(buf + off, cbProp, 128);

        off += cbProp;
        size_t off2 = align4(off);
        if (off2 != off) {
            printf("  [0x%04zx] Property[%lu] padding: %zu byte(s)\n", off,
                   (unsigned long)i, off2 - off);
            off = off2;
        }
    }

    if (off != len) {
        printf("  NOTE: trailing bytes after properties: %lu byte(s)\n",
               (unsigned long)(len - off));
    }
}

static BYTE *read_file(const wchar_t *path, DWORD *out_len) {
    HANDLE h = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING,
                           FILE_ATTRIBUTE_NORMAL, NULL);
    if (h == INVALID_HANDLE_VALUE) {
        die_win32(L"CreateFileW");
    }
    LARGE_INTEGER size;
    if (!GetFileSizeEx(h, &size)) {
        CloseHandle(h);
        die_win32(L"GetFileSizeEx");
    }
    if (size.QuadPart <= 0 || size.QuadPart > 16 * 1024 * 1024) {
        CloseHandle(h);
        die(L"unexpected file size");
    }
    DWORD cb = (DWORD)size.QuadPart;
    BYTE *buf = (BYTE *)malloc(cb);
    if (!buf) {
        CloseHandle(h);
        die(L"malloc failed");
    }
    DWORD read = 0;
    if (!ReadFile(h, buf, cb, &read, NULL) || read != cb) {
        CloseHandle(h);
        free(buf);
        die_win32(L"ReadFile");
    }
    CloseHandle(h);
    *out_len = cb;
    return buf;
}

static void try_set_friendly_name(PCCERT_CONTEXT cert) {
    // Use a stable value so runs are easy to diff and match documentation.
    const wchar_t *name = L"AeroBlobDumpExample";
    if (!CertSetCertificateContextProperty(cert, CERT_FRIENDLY_NAME_PROP_ID, 0, name)) {
        fwprintf(stderr, L"warning: failed to set FriendlyName (err=%lu)\n",
                 (unsigned long)GetLastError());
    }
}

static const wchar_t *g_key_container = L"AERO_BLOB_DUMP_CONTAINER";
static const wchar_t *g_key_provider =
    MS_ENHANCED_PROV_W; // "Microsoft Enhanced Cryptographic Provider v1.0"
static DWORD g_key_provider_type = PROV_RSA_FULL;
static int g_created_keyset = 0;

static void cleanup_temp_key_container(void) {
    if (!g_created_keyset) {
        return;
    }
    HCRYPTPROV hDel = 0;
    // For CRYPT_DELETEKEYSET, the handle is not used.
    CryptAcquireContextW(&hDel, g_key_container, g_key_provider, g_key_provider_type,
                         CRYPT_DELETEKEYSET);
}

static void try_set_key_prov_info(PCCERT_CONTEXT cert) {
    // Create a throwaway key container in the legacy CryptoAPI provider.
    // This should exist on Windows 7.
    const wchar_t *container = g_key_container;
    const wchar_t *provName = g_key_provider;
    DWORD provType = g_key_provider_type;

    HCRYPTPROV hProv = 0;
    if (!CryptAcquireContextW(&hProv, container, provName, provType,
                              CRYPT_NEWKEYSET)) {
        DWORD err = GetLastError();
        if (err == NTE_EXISTS) {
            if (!CryptAcquireContextW(&hProv, container, provName, provType, 0)) {
                fwprintf(stderr, L"warning: CryptAcquireContextW existing failed (err=%lu)\n",
                         (unsigned long)GetLastError());
                return;
            }
        } else {
            fwprintf(stderr, L"warning: CryptAcquireContextW new failed (err=%lu)\n",
                     (unsigned long)err);
            return;
        }
    } else {
        g_created_keyset = 1;
    }

    HCRYPTKEY hKey = 0;
    if (!CryptGenKey(hProv, AT_KEYEXCHANGE, CRYPT_EXPORTABLE, &hKey)) {
        fwprintf(stderr, L"warning: CryptGenKey failed (err=%lu)\n",
                 (unsigned long)GetLastError());
        CryptReleaseContext(hProv, 0);
        return;
    }
    CryptDestroyKey(hKey);

    CRYPT_KEY_PROV_INFO kpi;
    ZeroMemory(&kpi, sizeof(kpi));
    kpi.pwszContainerName = (LPWSTR)container;
    kpi.pwszProvName = (LPWSTR)provName;
    kpi.dwProvType = provType;
    kpi.dwFlags = 0;
    kpi.cProvParam = 0;
    kpi.rgProvParam = NULL;
    kpi.dwKeySpec = AT_KEYEXCHANGE;

    if (!CertSetCertificateContextProperty(cert, CERT_KEY_PROV_INFO_PROP_ID, 0, &kpi)) {
        fwprintf(stderr, L"warning: failed to set KeyProvInfo (err=%lu)\n",
                 (unsigned long)GetLastError());
    }

    CryptReleaseContext(hProv, 0);
}

static void dump_context_properties(PCCERT_CONTEXT cert) {
    printf("Certificate context properties (CertEnumCertificateContextProperties):\n");
    DWORD propId = 0;
    for (;;) {
        propId = CertEnumCertificateContextProperties(cert, propId);
        if (propId == 0) {
            break;
        }
        const char *name = cert_prop_name(propId);
        if (name) {
            printf("  %lu (0x%lx) [%s]\n", (unsigned long)propId, (unsigned long)propId, name);
        } else {
            printf("  %lu (0x%lx)\n", (unsigned long)propId, (unsigned long)propId);
        }
    }
}

static void dump_context_property_bytes(PCCERT_CONTEXT cert, DWORD propId) {
    DWORD cb = 0;
    if (!CertGetCertificateContextProperty(cert, propId, NULL, &cb)) {
        return;
    }
    BYTE *buf = (BYTE *)malloc(cb);
    if (!buf) {
        die(L"malloc failed");
    }
    if (!CertGetCertificateContextProperty(cert, propId, buf, &cb)) {
        free(buf);
        return;
    }
    const char *name = cert_prop_name(propId);
    if (name) {
        printf("Property %lu [%s] from CertGetCertificateContextProperty (%lu bytes):\n",
               (unsigned long)propId, name, (unsigned long)cb);
    } else {
        printf("Property %lu from CertGetCertificateContextProperty (%lu bytes):\n",
               (unsigned long)propId, (unsigned long)cb);
    }
    hexdump_limit(buf, cb, 256);
    free(buf);
}

static int parse_serialized_cert_for_props(const BYTE *buf, DWORD len, size_t *props_off_out,
                                           DWORD *props_count_out) {
    if (len < 8) {
        return 0;
    }
    uint32_t v0 = read_u32le(buf + 0);
    uint32_t v1 = read_u32le(buf + 4);
    uint32_t cbCert = 0;
    size_t cert_off = 0;
    if ((v0 == CERT_STORE_CERTIFICATE_CONTEXT || v0 == CERT_STORE_CRL_CONTEXT ||
         v0 == CERT_STORE_CTL_CONTEXT) &&
        (v1 == X509_ASN_ENCODING || v1 == (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING) ||
         v1 == PKCS_7_ASN_ENCODING)) {
        if (len < 12) {
            return 0;
        }
        cbCert = read_u32le(buf + 8);
        cert_off = 12;
    } else {
        cbCert = v1;
        cert_off = 8;
    }
    size_t off = cert_off + (size_t)cbCert;
    if (off > len) {
        return 0;
    }
    off = align4(off);
    if (off + 4 > len) {
        return 0;
    }
    *props_off_out = off;
    *props_count_out = (DWORD)read_u32le(buf + off);
    return 1;
}

static int find_serialized_property(const BYTE *buf, DWORD len, DWORD propIdWanted,
                                    const BYTE **out_val, DWORD *out_len) {
    size_t props_off = 0;
    DWORD cProps = 0;
    if (!parse_serialized_cert_for_props(buf, len, &props_off, &cProps)) {
        return 0;
    }
    size_t off = props_off + 4;
    for (DWORD i = 0; i < cProps; i++) {
        if (off + 8 > len) {
            return 0;
        }
        DWORD propId = (DWORD)read_u32le(buf + off);
        DWORD cbProp = (DWORD)read_u32le(buf + off + 4);
        off += 8;
        if (off + cbProp > len) {
            return 0;
        }
        if (propId == propIdWanted) {
            *out_val = buf + off;
            *out_len = cbProp;
            return 1;
        }
        off += cbProp;
        off = align4(off);
    }
    return 0;
}

static BYTE *get_context_property_alloc(PCCERT_CONTEXT cert, DWORD propId, DWORD *out_len) {
    DWORD cb = 0;
    if (!CertGetCertificateContextProperty(cert, propId, NULL, &cb)) {
        return NULL;
    }
    BYTE *buf = (BYTE *)malloc(cb);
    if (!buf) {
        die(L"malloc failed");
    }
    if (!CertGetCertificateContextProperty(cert, propId, buf, &cb)) {
        free(buf);
        return NULL;
    }
    *out_len = cb;
    return buf;
}

typedef struct {
    DWORD propId;
    const BYTE *value;
    DWORD cbValue;
} PROP_ENTRY;

static BYTE *build_expected_blob(PCCERT_CONTEXT cert, const PROP_ENTRY *props, DWORD cProps,
                                 DWORD *out_len) {
    // Build a serialized certificate element according to this repository's spec:
    //   [dwEncodingType][cbCert][DER][pad-to-4][cProperties][prop...]
    //
    // This is used as a sanity-check against the bytes produced by
    // CertSerializeCertificateStoreElement() on Windows 7.
    size_t total = 0;
    total += 8;
    total += cert->cbCertEncoded;
    total = align4(total);
    total += 4; // cProperties
    for (DWORD i = 0; i < cProps; i++) {
        total += 8;
        total += props[i].cbValue;
        total = align4(total);
    }

    if (total > 0x7fffffff) {
        die(L"expected blob too large");
    }

    BYTE *buf = (BYTE *)malloc(total);
    if (!buf) {
        die(L"malloc failed");
    }
    ZeroMemory(buf, total);

    size_t off = 0;
    *(DWORD *)(buf + off) = cert->dwCertEncodingType;
    off += 4;
    *(DWORD *)(buf + off) = cert->cbCertEncoded;
    off += 4;
    memcpy(buf + off, cert->pbCertEncoded, cert->cbCertEncoded);
    off += cert->cbCertEncoded;
    off = align4(off);

    *(DWORD *)(buf + off) = cProps;
    off += 4;
    for (DWORD i = 0; i < cProps; i++) {
        *(DWORD *)(buf + off) = props[i].propId;
        off += 4;
        *(DWORD *)(buf + off) = props[i].cbValue;
        off += 4;
        memcpy(buf + off, props[i].value, props[i].cbValue);
        off += props[i].cbValue;
        off = align4(off);
    }

    if (off != total) {
        free(buf);
        die(L"internal expected-blob size mismatch");
    }

    *out_len = (DWORD)total;
    return buf;
}

static void print_first_diff(const BYTE *a, DWORD aLen, const BYTE *b, DWORD bLen) {
    DWORD minLen = aLen < bLen ? aLen : bLen;
    for (DWORD i = 0; i < minLen; i++) {
        if (a[i] != b[i]) {
            printf("  first diff at 0x%lx: actual=%02x expected=%02x\n", (unsigned long)i,
                   (unsigned)a[i], (unsigned)b[i]);
            DWORD start = (i > 32) ? (i - 32) : 0;
            DWORD end = i + 32;
            if (end > aLen)
                end = aLen;
            printf("  actual bytes around diff:\n");
            hexdump_limit(a + start, end - start, 256);
            end = i + 32;
            if (end > bLen)
                end = bLen;
            printf("  expected bytes around diff:\n");
            hexdump_limit(b + start, end - start, 256);
            return;
        }
    }
    if (aLen != bLen) {
        printf("  length mismatch: actual=%lu expected=%lu\n", (unsigned long)aLen,
               (unsigned long)bLen);
    } else {
        printf("  no diff found (unexpected)\n");
    }
}

static void compare_serialized_property_with_context(PCCERT_CONTEXT cert, const BYTE *ser,
                                                     DWORD cbSer, DWORD propId) {
    const BYTE *serVal = NULL;
    DWORD cbSerVal = 0;
    if (!find_serialized_property(ser, cbSer, propId, &serVal, &cbSerVal)) {
        const char *name = cert_prop_name(propId);
        if (name) {
            printf("Property %lu [%s]: not present in serialized blob\n", (unsigned long)propId,
                   name);
        } else {
            printf("Property %lu: not present in serialized blob\n", (unsigned long)propId);
        }
        return;
    }

    DWORD cbCtx = 0;
    BYTE *ctxVal = get_context_property_alloc(cert, propId, &cbCtx);

    const char *name = cert_prop_name(propId);
    if (name) {
        printf("Property %lu [%s]: serialized cb=%lu, context cb=%lu\n", (unsigned long)propId,
               name, (unsigned long)cbSerVal, (unsigned long)cbCtx);
    } else {
        printf("Property %lu: serialized cb=%lu, context cb=%lu\n", (unsigned long)propId,
               (unsigned long)cbSerVal, (unsigned long)cbCtx);
    }

    if (ctxVal && cbCtx == cbSerVal && memcmp(ctxVal, serVal, cbSerVal) == 0) {
        printf("  -> bytes MATCH CertGetCertificateContextProperty output\n");
    } else {
        printf("  -> bytes DIFFER from CertGetCertificateContextProperty output\n");
    }
    printf("  Serialized bytes (first 128):\n");
    hexdump_limit(serVal, cbSerVal, 128);
    if (ctxVal) {
        printf("  Context bytes (first 128):\n");
        hexdump_limit(ctxVal, cbCtx, 128);
    }
    free(ctxVal);
}

static BYTE *serialize_cert(PCCERT_CONTEXT cert, DWORD *out_len) {
    DWORD cb = 0;
    if (!CertSerializeCertificateStoreElement(cert, 0, NULL, &cb)) {
        die_win32(L"CertSerializeCertificateStoreElement(size)");
    }
    BYTE *buf = (BYTE *)malloc(cb);
    if (!buf) {
        die(L"malloc failed");
    }
    if (!CertSerializeCertificateStoreElement(cert, 0, buf, &cb)) {
        free(buf);
        die_win32(L"CertSerializeCertificateStoreElement");
    }
    *out_len = cb;
    return buf;
}

static void roundtrip_via_add_serialized(const BYTE *buf, DWORD len) {
    HCERTSTORE mem = CertOpenStore(CERT_STORE_PROV_MEMORY, 0, 0, 0, NULL);
    if (!mem) {
        die_win32(L"CertOpenStore(MEMORY)");
    }
    // Restrict the deserialization to "certificate context" so we can treat a
    // successful round-trip as proof that the bytes are a valid serialized cert.
    DWORD ctxFlags = CERT_STORE_CERTIFICATE_CONTEXT_FLAG;
    if (!CertAddSerializedElementToStore(mem, buf, len, CERT_STORE_ADD_ALWAYS, 0, ctxFlags,
                                         NULL)) {
        CertCloseStore(mem, 0);
        die_win32(L"CertAddSerializedElementToStore");
    }
    CertCloseStore(mem, 0);
}

static void compare_registry_blob(const wchar_t *storeName, PCCERT_CONTEXT storeCert,
                                  const BYTE *expected, DWORD expectedLen) {
    wchar_t thumb[41];
    print_thumbprint_hex(storeCert, thumb);

    wchar_t keyPath[512];
    swprintf(keyPath, 512,
             L"Software\\Microsoft\\SystemCertificates\\%ls\\Certificates\\%ls",
             storeName, thumb);

    HKEY hKey = NULL;
    LONG rc = RegOpenKeyExW(HKEY_CURRENT_USER, keyPath, 0, KEY_QUERY_VALUE, &hKey);
    if (rc != ERROR_SUCCESS) {
        fwprintf(stderr, L"warning: RegOpenKeyExW(%ls) failed (rc=%ld)\n", keyPath,
                 (long)rc);
        return;
    }

    DWORD type = 0;
    DWORD cb = 0;
    rc = RegQueryValueExW(hKey, L"Blob", NULL, &type, NULL, &cb);
    if (rc != ERROR_SUCCESS) {
        RegCloseKey(hKey);
        fwprintf(stderr, L"warning: RegQueryValueExW(Blob size) failed (rc=%ld)\n",
                 (long)rc);
        return;
    }
    if (type != REG_BINARY) {
        RegCloseKey(hKey);
        fwprintf(stderr, L"warning: Blob is not REG_BINARY (type=%lu)\n",
                 (unsigned long)type);
        return;
    }
    BYTE *blob = (BYTE *)malloc(cb);
    if (!blob) {
        RegCloseKey(hKey);
        die(L"malloc failed");
    }
    rc = RegQueryValueExW(hKey, L"Blob", NULL, &type, blob, &cb);
    RegCloseKey(hKey);
    if (rc != ERROR_SUCCESS) {
        free(blob);
        fwprintf(stderr, L"warning: RegQueryValueExW(Blob) failed (rc=%ld)\n", (long)rc);
        return;
    }

    printf("Registry Blob: %lu byte(s)\n", (unsigned long)cb);
    if (cb == expectedLen && memcmp(blob, expected, cb) == 0) {
        printf("Registry Blob matches CertSerializeCertificateStoreElement() output.\n");
    } else {
        printf("Registry Blob DOES NOT match serialized output.\n");
        printf("First 256 bytes of registry blob:\n");
        hexdump(blob, cb < 256 ? cb : 256);
        printf("First 256 bytes of expected blob:\n");
        hexdump(expected, expectedLen < 256 ? expectedLen : 256);
    }
    free(blob);
}

static void cleanup_cert_from_store(const wchar_t *storeName, PCCERT_CONTEXT certToMatch) {
    HCERTSTORE store = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0,
                                     CERT_SYSTEM_STORE_CURRENT_USER, storeName);
    if (!store) {
        return;
    }
    PCCERT_CONTEXT found = NULL;
    found = CertFindCertificateInStore(store, X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, 0,
                                       CERT_FIND_EXISTING, certToMatch, NULL);
    if (found) {
        // This also frees 'found' on success.
        if (!CertDeleteCertificateFromStore(found)) {
            CertFreeCertificateContext(found);
        }
    }
    CertCloseStore(store, 0);
}

int wmain(int argc, wchar_t **argv) {
    if (argc != 2) {
        fwprintf(stderr, L"usage: %ls <cert.der>\n", argv[0]);
        return 2;
    }

    const wchar_t *storeName = L"AERO_BLOB_DUMP";

    DWORD cbDer = 0;
    BYTE *der = read_file(argv[1], &cbDer);

    PCCERT_CONTEXT certNoProps =
        CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, der, cbDer);
    if (!certNoProps) {
        free(der);
        die_win32(L"CertCreateCertificateContext(no-props)");
    }

    printf("=== CertSerializeCertificateStoreElement (no extra properties) ===\n");
    dump_context_properties(certNoProps);
    DWORD cbSer0 = 0;
    BYTE *ser0 = serialize_cert(certNoProps, &cbSer0);
    printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSer0);
    dump_serialized_cert_blob(ser0, cbSer0);
    hexdump(ser0, cbSer0);
    // Spec sanity-check: for a freshly-created context with no explicit persisted
    // properties, the expected serialized form is header+DER+pad+cProperties(0).
    DWORD cbExpected0 = 0;
    BYTE *expected0 = build_expected_blob(certNoProps, NULL, 0, &cbExpected0);
    if (cbSer0 == cbExpected0 && memcmp(ser0, expected0, cbSer0) == 0) {
        printf("Spec check (no-props): PASS\n");
    } else {
        printf("Spec check (no-props): FAIL\n");
        print_first_diff(ser0, cbSer0, expected0, cbExpected0);
    }
    free(expected0);
    compare_serialized_property_with_context(certNoProps, ser0, cbSer0, CERT_SHA1_HASH_PROP_ID);
    roundtrip_via_add_serialized(ser0, cbSer0);
    free(ser0);
    CertFreeCertificateContext(certNoProps);

    PCCERT_CONTEXT certProps =
        CertCreateCertificateContext(X509_ASN_ENCODING | PKCS_7_ASN_ENCODING, der, cbDer);
    if (!certProps) {
        free(der);
        die_win32(L"CertCreateCertificateContext(with-props)");
    }

    // Properties must be set on the context that we serialize / add to store.
    try_set_friendly_name(certProps);

    printf("\n=== CertSerializeCertificateStoreElement (FriendlyName only) ===\n");
    dump_context_properties(certProps);
    dump_context_property_bytes(certProps, CERT_FRIENDLY_NAME_PROP_ID);
    DWORD cbSerFriendly = 0;
    BYTE *serFriendly = serialize_cert(certProps, &cbSerFriendly);
    printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSerFriendly);
    dump_serialized_cert_blob(serFriendly, cbSerFriendly);
    hexdump(serFriendly, cbSerFriendly);

    DWORD cbFriendlyVal = 0;
    BYTE *friendlyVal =
        get_context_property_alloc(certProps, CERT_FRIENDLY_NAME_PROP_ID, &cbFriendlyVal);
    if (friendlyVal) {
        PROP_ENTRY prop;
        prop.propId = CERT_FRIENDLY_NAME_PROP_ID;
        prop.value = friendlyVal;
        prop.cbValue = cbFriendlyVal;
        DWORD cbExpectedFriendly = 0;
        BYTE *expectedFriendly = build_expected_blob(certProps, &prop, 1, &cbExpectedFriendly);
        if (cbSerFriendly == cbExpectedFriendly &&
            memcmp(serFriendly, expectedFriendly, cbSerFriendly) == 0) {
            printf("Spec check (FriendlyName only): PASS\n");
        } else {
            printf("Spec check (FriendlyName only): FAIL\n");
            print_first_diff(serFriendly, cbSerFriendly, expectedFriendly, cbExpectedFriendly);
        }
        free(expectedFriendly);
        free(friendlyVal);
    }

    compare_serialized_property_with_context(certProps, serFriendly, cbSerFriendly,
                                             CERT_FRIENDLY_NAME_PROP_ID);
    roundtrip_via_add_serialized(serFriendly, cbSerFriendly);
    free(serFriendly);

    try_set_key_prov_info(certProps);

    printf("\n=== CertSerializeCertificateStoreElement (FriendlyName + KeyProvInfo) ===\n");
    dump_context_properties(certProps);
    dump_context_property_bytes(certProps, CERT_FRIENDLY_NAME_PROP_ID);
    dump_context_property_bytes(certProps, CERT_KEY_PROV_INFO_PROP_ID);
    DWORD cbSer1 = 0;
    BYTE *ser1 = serialize_cert(certProps, &cbSer1);
    printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSer1);
    dump_serialized_cert_blob(ser1, cbSer1);
    hexdump(ser1, cbSer1);
    compare_serialized_property_with_context(certProps, ser1, cbSer1, CERT_FRIENDLY_NAME_PROP_ID);
    compare_serialized_property_with_context(certProps, ser1, cbSer1, CERT_KEY_PROV_INFO_PROP_ID);
    roundtrip_via_add_serialized(ser1, cbSer1);

    // Cross-check against registry provider by adding to a real system store.
    HCERTSTORE sysStore = CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0,
                                        CERT_SYSTEM_STORE_CURRENT_USER, storeName);
    if (!sysStore) {
        fwprintf(stderr, L"warning: CertOpenStore(system) failed (err=%lu)\n",
                 (unsigned long)GetLastError());
    } else {
        PCCERT_CONTEXT added = NULL;
        if (!CertAddCertificateContextToStore(sysStore, certProps,
                                              CERT_STORE_ADD_REPLACE_EXISTING, &added)) {
            fwprintf(stderr, L"warning: CertAddCertificateContextToStore failed (err=%lu)\n",
                     (unsigned long)GetLastError());
        } else {
            // Re-serialize the context that came back from the system store.
            // This is the closest representation to what the registry provider
            // actually persisted.
            DWORD cbSerStore = 0;
            BYTE *serStore = serialize_cert(added, &cbSerStore);
            printf("\n=== CertSerializeCertificateStoreElement (context returned from system store) ===\n");
            printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSerStore);
            dump_context_properties(added);
            dump_serialized_cert_blob(serStore, cbSerStore);
            hexdump(serStore, cbSerStore);
            compare_serialized_property_with_context(added, serStore, cbSerStore,
                                                     CERT_FRIENDLY_NAME_PROP_ID);
            compare_serialized_property_with_context(added, serStore, cbSerStore,
                                                     CERT_KEY_PROV_INFO_PROP_ID);

            compare_registry_blob(storeName, added, serStore, cbSerStore);
            free(serStore);
            CertFreeCertificateContext(added);
        }
        CertCloseStore(sysStore, 0);
    }

    cleanup_cert_from_store(storeName, certProps);

    free(ser1);
    CertFreeCertificateContext(certProps);
    free(der);

    cleanup_temp_key_container();
    return 0;
}
