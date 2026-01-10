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
        printf("  [0x%04zx] Property[%lu].dwPropId  = %lu (0x%lx)\n", off,
               (unsigned long)i, (unsigned long)propId, (unsigned long)propId);
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
            DWORD cch = cbProp / 2;
            const wchar_t *ws = (const wchar_t *)(buf + off);
            // Print up to first NUL or end.
            for (DWORD j = 0; j < cch; j++) {
                wchar_t ch = ws[j];
                if (!ch)
                    break;
                putwchar(ch);
            }
            putwchar(L'\n');
        }

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
    const wchar_t *name = L"Aero test friendly name";
    if (!CertSetCertificateContextProperty(cert, CERT_FRIENDLY_NAME_PROP_ID, 0, name)) {
        fwprintf(stderr, L"warning: failed to set FriendlyName (err=%lu)\n",
                 (unsigned long)GetLastError());
    }
}

static void try_set_key_prov_info(PCCERT_CONTEXT cert) {
    // Create a throwaway key container in the legacy CryptoAPI provider.
    // This should exist on Windows 7.
    const wchar_t *container = L"AERO_BLOB_DUMP_CONTAINER";
    const wchar_t *provName = MS_ENHANCED_PROV_W; // "Microsoft Enhanced Cryptographic Provider v1.0"
    DWORD provType = PROV_RSA_FULL;

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
    DWORD cbSer0 = 0;
    BYTE *ser0 = serialize_cert(certNoProps, &cbSer0);
    printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSer0);
    dump_serialized_cert_blob(ser0, cbSer0);
    hexdump(ser0, cbSer0);
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
    try_set_key_prov_info(certProps);

    printf("\n=== CertSerializeCertificateStoreElement (with FriendlyName + KeyProvInfo) ===\n");
    DWORD cbSer1 = 0;
    BYTE *ser1 = serialize_cert(certProps, &cbSer1);
    printf("Serialized size: %lu byte(s)\n", (unsigned long)cbSer1);
    dump_serialized_cert_blob(ser1, cbSer1);
    hexdump(ser1, cbSer1);
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
            dump_serialized_cert_blob(serStore, cbSerStore);
            hexdump(serStore, cbSerStore);

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
    return 0;
}
