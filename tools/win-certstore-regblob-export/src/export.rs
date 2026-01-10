use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

pub const REG_SZ: u32 = 1;
pub const REG_BINARY: u32 = 3;
pub const REG_DWORD: u32 = 4;

#[derive(Debug, Clone)]
pub struct RegistryValue {
    pub ty: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct CertRegistryPatch {
    pub store: String,
    pub thumbprint_sha1: String,
    pub values: BTreeMap<String, RegistryValue>,
}

pub fn load_certificates_from_file(path: &Path) -> Result<Vec<Vec<u8>>> {
    let data = std::fs::read(path).with_context(|| format!("read certificate file: {}", path.display()))?;

    if data.windows(b"-----BEGIN".len()).any(|w| w == b"-----BEGIN") {
        let pem_text = std::str::from_utf8(&data)
            .with_context(|| format!("certificate file is not valid UTF-8 PEM: {}", path.display()))?;

        let mut certs = Vec::new();
        for block in pem::parse_many(pem_text).with_context(|| format!("parse PEM: {}", path.display()))? {
            if block.tag() == "CERTIFICATE" {
                certs.push(block.contents().to_vec());
            }
        }
        if certs.is_empty() {
            bail!("no CERTIFICATE blocks found in PEM file: {}", path.display());
        }
        Ok(certs)
    } else {
        if data.is_empty() {
            bail!("certificate file is empty: {}", path.display());
        }
        Ok(vec![data])
    }
}

pub fn render_reg_file(patches: &[CertRegistryPatch]) -> Result<String> {
    let mut out = String::new();
    out.push_str("Windows Registry Editor Version 5.00\r\n\r\n");

    for patch in patches {
        let key_path = format!(
            "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\SystemCertificates\\{}\\Certificates\\{}",
            patch.store, patch.thumbprint_sha1
        );
        out.push('[');
        out.push_str(&key_path);
        out.push_str("]\r\n");

        for (name, val) in &patch.values {
            out.push_str(&render_reg_value(name, val)?);
            out.push_str("\r\n");
        }

        out.push_str("\r\n");
    }

    Ok(out)
}

fn render_reg_value(name: &str, value: &RegistryValue) -> Result<String> {
    let mut out = String::new();

    if name.is_empty() {
        out.push('@');
    } else {
        out.push('"');
        out.push_str(&escape_reg_string(name));
        out.push('"');
    }
    out.push('=');

    match value.ty {
        REG_DWORD => {
            if value.bytes.len() != 4 {
                bail!("REG_DWORD value {name:?} expected 4 bytes, got {}", value.bytes.len());
            }
            let v = u32::from_le_bytes(value.bytes.as_slice().try_into().unwrap());
            out.push_str(&format!("dword:{:08x}", v));
        }
        REG_SZ => {
            let s = decode_reg_sz(&value.bytes)?;
            out.push('"');
            out.push_str(&escape_reg_string(&s));
            out.push('"');
        }
        REG_BINARY => {
            out.push_str("hex:");
            out.push_str(&format_hex_bytes(&value.bytes, "  "));
        }
        other => {
            out.push_str(&format!("hex({:x}):", other));
            out.push_str(&format_hex_bytes(&value.bytes, "  "));
        }
    }

    Ok(out)
}

fn escape_reg_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn format_hex_bytes(bytes: &[u8], indent: &str) -> String {
    let mut out = String::new();
    for (idx, b) in bytes.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        if idx > 0 && idx % 16 == 0 {
            out.push_str("\\\r\n");
            out.push_str(indent);
        }
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn decode_reg_sz(bytes: &[u8]) -> Result<String> {
    if bytes.len() % 2 != 0 {
        return Err(anyhow!("REG_SZ bytes must be valid UTF-16LE (even length), got {}", bytes.len()));
    }

    let mut utf16: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    while utf16.last() == Some(&0) {
        utf16.pop();
    }

    String::from_utf16(&utf16).context("decode REG_SZ as UTF-16LE")
}

#[cfg(windows)]
mod windows_impl {
    use super::{hex_upper, CertRegistryPatch, RegistryValue, REG_BINARY};
    use anyhow::{anyhow, bail, Context, Result};
    use std::collections::BTreeMap;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_SUCCESS};
    use windows::Win32::Security::Cryptography::{
        CertAddEncodedCertificateToStore, CertCloseStore, CertFreeCertificateContext,
        CertGetCertificateContextProperty, CertOpenStore, CERT_SHA1_HASH_PROP_ID,
        CERT_STORE_ADD_ALWAYS, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_REGISTRY_W,
        HCERTSTORE, HCRYPTPROV_LEGACY, PCCERT_CONTEXT, PKCS_7_ASN_ENCODING, X509_ASN_ENCODING,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
        RegQueryInfoKeyW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
        REG_OPTION_NON_VOLATILE,
    };

    pub fn export_system_cert_reg_patch(store: &str, der_cert: &[u8]) -> Result<CertRegistryPatch> {
        let unique = temp_suffix();
        let temp_root = format!("Software\\__win-certstore-regblob-export-{unique}");
        let store_path = format!("{temp_root}\\SystemCertificates\\{store}");

        let _cleanup = TempRegTree::new(&temp_root)?;
        let store_key = create_hkcu_key(&store_path)?;

        let cert_store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_REGISTRY_W,
                (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING) as u32,
                HCRYPTPROV_LEGACY::default(),
                CERT_STORE_OPEN_EXISTING_FLAG,
                Some((&store_key.0 as *const HKEY).cast::<c_void>()),
            )
        }
        .context("CertOpenStore(CERT_STORE_PROV_SYSTEM_REGISTRY)")?;
        let cert_store = CertStore(cert_store);

        let cert_ctx = add_cert(&cert_store, der_cert)?;
        let expected_thumb = sha1_thumbprint(&cert_ctx)?;
        drop(cert_ctx);

        drop(cert_store);

        let certs_key = open_subkey(&store_key, "Certificates")?;
        let certs_key = OwnedHKey(certs_key);
        let thumbprint_key_name = resolve_cert_subkey_name(&certs_key, &expected_thumb)?;
        let cert_key = open_subkey(&certs_key, &thumbprint_key_name)?;
        let cert_key = OwnedHKey(cert_key);

        let values = read_all_values(&cert_key)?;

        match values.get("Blob") {
            Some(v) if v.ty == REG_BINARY && !v.bytes.is_empty() => {}
            _ => {
            bail!("registry-backed store did not create a non-empty REG_BINARY Blob value");
            }
        }

        Ok(CertRegistryPatch {
            store: store.to_string(),
            thumbprint_sha1: thumbprint_key_name,
            values,
        })
    }

    fn temp_suffix() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{pid}-{millis}-{counter}")
    }

    struct TempRegTree {
        subkey_path: String,
    }

    impl TempRegTree {
        fn new(subkey_path: &str) -> Result<Self> {
            // Ensure the key exists so deletion is deterministic.
            let _ = create_hkcu_key(subkey_path)?;
            Ok(Self {
                subkey_path: subkey_path.to_string(),
            })
        }
    }

    impl Drop for TempRegTree {
        fn drop(&mut self) {
            unsafe {
                let wide = to_wide(&self.subkey_path);
                // Best-effort cleanup; ignore failures (e.g. if already deleted).
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wide.as_ptr()));
            }
        }
    }

    #[derive(Debug)]
    struct OwnedHKey(HKEY);

    impl Drop for OwnedHKey {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    #[derive(Debug)]
    struct CertStore(HCERTSTORE);

    impl Drop for CertStore {
        fn drop(&mut self) {
            unsafe {
                let _ = CertCloseStore(self.0, 0);
            }
        }
    }

    #[derive(Debug)]
    struct CertContext(PCCERT_CONTEXT);

    impl Drop for CertContext {
        fn drop(&mut self) {
            unsafe { CertFreeCertificateContext(self.0) };
        }
    }

    fn create_hkcu_key(sub_path: &str) -> Result<OwnedHKey> {
        unsafe {
            let wide = to_wide(sub_path);
            let mut out = HKEY::default();
            let mut disposition = 0u32;
            let status = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(wide.as_ptr()),
                0,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut out,
                Some(&mut disposition),
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }
            Ok(OwnedHKey(out))
        }
    }

    fn open_subkey(parent: &OwnedHKey, name: &str) -> Result<HKEY> {
        unsafe {
            let wide = to_wide(name);
            let mut out = HKEY::default();
            let status = RegOpenKeyExW(
                parent.0,
                PCWSTR(wide.as_ptr()),
                0,
                KEY_READ | KEY_WRITE,
                &mut out,
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }
            Ok(out)
        }
    }

    fn add_cert(store: &CertStore, der: &[u8]) -> Result<CertContext> {
        let mut ctx = PCCERT_CONTEXT::default();
        let ok = unsafe {
            CertAddEncodedCertificateToStore(
                store.0,
                (X509_ASN_ENCODING | PKCS_7_ASN_ENCODING) as u32,
                der.as_ptr(),
                der.len().try_into().context("certificate is too large")?,
                CERT_STORE_ADD_ALWAYS,
                Some(&mut ctx),
            )
        };
        if !ok.as_bool() {
            return Err(anyhow!("CertAddEncodedCertificateToStore failed: {:?}", unsafe {
                GetLastError()
            }));
        }
        Ok(CertContext(ctx))
    }

    fn sha1_thumbprint(cert: &CertContext) -> Result<String> {
        let mut hash = [0u8; 20];
        let mut size = hash.len() as u32;
        let ok = unsafe {
            CertGetCertificateContextProperty(
                cert.0,
                CERT_SHA1_HASH_PROP_ID,
                Some(hash.as_mut_ptr().cast::<c_void>()),
                &mut size,
            )
        };
        if !ok.as_bool() {
            return Err(anyhow!(
                "CertGetCertificateContextProperty(CERT_SHA1_HASH_PROP_ID) failed: {:?}",
                unsafe { GetLastError() }
            ));
        }
        Ok(hex_upper(&hash[..(size as usize)]))
    }

    fn resolve_cert_subkey_name(certs_key: &OwnedHKey, expected: &str) -> Result<String> {
        let subkeys = enumerate_subkeys(certs_key)?;
        if let Some(found) = subkeys.iter().find(|name| name.eq_ignore_ascii_case(expected)) {
            return Ok(found.clone());
        }
        if subkeys.len() == 1 {
            return Ok(subkeys[0].clone());
        }
        bail!(
            "could not uniquely identify cert subkey (expected thumbprint {expected}, found subkeys: {subkeys:?})"
        )
    }

    fn enumerate_subkeys(key: &OwnedHKey) -> Result<Vec<String>> {
        unsafe {
            let mut count = 0u32;
            let mut max_len = 0u32;
            let status = RegQueryInfoKeyW(
                key.0,
                None,
                None,
                None,
                Some(&mut count),
                Some(&mut max_len),
                None,
                None,
                None,
                None,
                None,
                None,
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }

            let mut names = Vec::with_capacity(count as usize);
            let mut buf = vec![0u16; (max_len + 1) as usize];

            for index in 0..count {
                let mut len = max_len + 1;
                let status = RegEnumKeyExW(
                    key.0,
                    index,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                );
                if status != ERROR_SUCCESS.0 as i32 {
                    return Err(std::io::Error::from_raw_os_error(status).into());
                }
                names.push(String::from_utf16_lossy(&buf[..len as usize]));
            }

            Ok(names)
        }
    }

    fn read_all_values(key: &OwnedHKey) -> Result<BTreeMap<String, RegistryValue>> {
        unsafe {
            let mut count = 0u32;
            let mut max_name_len = 0u32;
            let mut max_data_len = 0u32;
            let status = RegQueryInfoKeyW(
                key.0,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(&mut count),
                Some(&mut max_name_len),
                Some(&mut max_data_len),
                None,
                None,
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }

            let mut values = BTreeMap::new();
            let mut name_buf = vec![0u16; (max_name_len + 1) as usize];
            let mut data_buf = vec![0u8; max_data_len as usize];

            for index in 0..count {
                let mut name_len = max_name_len + 1;
                let mut ty = 0u32;
                let mut data_len = max_data_len;
                let status = RegEnumValueW(
                    key.0,
                    index,
                    PWSTR(name_buf.as_mut_ptr()),
                    &mut name_len,
                    None,
                    Some(&mut ty),
                    Some(data_buf.as_mut_ptr()),
                    Some(&mut data_len),
                );
                if status != ERROR_SUCCESS.0 as i32 {
                    return Err(std::io::Error::from_raw_os_error(status).into());
                }
                let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
                let data = data_buf[..data_len as usize].to_vec();
                values.insert(name, RegistryValue { ty, bytes: data });
            }

            // Some values can be larger than max_data_len if they were added after RegQueryInfoKeyW
            // (unlikely for our use-case, but handle explicitly by re-querying).
            for (name, val) in values.iter_mut() {
                if val.bytes.len() == max_data_len as usize && max_data_len != 0 {
                    let (ty, data) = query_value(key, name)?;
                    val.ty = ty;
                    val.bytes = data;
                }
            }

            Ok(values)
        }
    }

    fn query_value(key: &OwnedHKey, name: &str) -> Result<(u32, Vec<u8>)> {
        unsafe {
            let mut ty = 0u32;
            let mut data_len = 0u32;
            let name_w = to_wide(name);
            let status = RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut data_len),
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }

            let mut data = vec![0u8; data_len as usize];
            let status = RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                Some(&mut ty),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            );
            if status != ERROR_SUCCESS.0 as i32 {
                return Err(std::io::Error::from_raw_os_error(status).into());
            }
            data.truncate(data_len as usize);
            Ok((ty, data))
        }
    }

    fn to_wide(s: &str) -> Vec<u16> {
        use std::os::windows::prelude::*;
        let mut wide: Vec<u16> = s.encode_wide().collect();
        wide.push(0);
        wide
    }

    use windows::core::PWSTR;
}

#[cfg(windows)]
pub use windows_impl::export_system_cert_reg_patch;

#[cfg(not(windows))]
pub fn export_system_cert_reg_patch(_store: &str, _der_cert: &[u8]) -> Result<CertRegistryPatch> {
    bail!("this tool only runs on Windows")
}

pub fn hex_upper(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02X}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_upper_is_uppercase() {
        assert_eq!(hex_upper(&[0x00, 0xAB, 0x5f]), "00AB5F");
    }

    #[test]
    fn format_hex_bytes_wraps() {
        let bytes: Vec<u8> = (0u8..40).collect();
        let s = format_hex_bytes(&bytes, "  ");
        assert!(s.contains("\\\r\n  "));
    }
}
