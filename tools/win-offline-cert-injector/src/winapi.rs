#![cfg(windows)]

use std::ffi::{c_void, OsStr};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub type DWORD = u32;
pub type BOOL = i32;
pub type HANDLE = isize;
pub type HKEY = isize;
pub type HCERTSTORE = *mut c_void;

#[derive(Debug, Clone)]
pub struct WinError {
    pub context: String,
    pub code: DWORD,
}

impl std::fmt::Display for WinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (win32 error {})", self.context, self.code)
    }
}

impl std::error::Error for WinError {}

fn last_error(context: impl Into<String>) -> WinError {
    let code = unsafe { GetLastError() };
    WinError {
        context: context.into(),
        code,
    }
}

pub fn wide_null_from_os_str(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

pub fn wide_null_from_path(path: &Path) -> Vec<u16> {
    wide_null_from_os_str(path.as_os_str())
}

pub fn wide_null_from_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub const HKEY_LOCAL_MACHINE: HKEY = 0x80000002u32 as i32 as isize;

const ERROR_SUCCESS: DWORD = 0;
const ERROR_FILE_NOT_FOUND: DWORD = 2;
const ERROR_NOT_ALL_ASSIGNED: DWORD = 1300;

const TOKEN_ADJUST_PRIVILEGES: DWORD = 0x0020;
const TOKEN_QUERY: DWORD = 0x0008;

const SE_PRIVILEGE_ENABLED: DWORD = 0x0002;

const KEY_READ: DWORD = 0x20019;
const KEY_ALL_ACCESS: DWORD = 0xF003F;

const CERT_STORE_PROV_SYSTEM_REGISTRY_W: *const i8 = 13 as *const i8;

const X509_ASN_ENCODING: DWORD = 0x00000001;
const PKCS_7_ASN_ENCODING: DWORD = 0x00010000;
const CERT_STORE_MAXIMUM_ALLOWED_FLAG: DWORD = 0x00001000;
const CERT_STORE_READONLY_FLAG: DWORD = 0x00008000;
const CERT_STORE_ADD_REPLACE_EXISTING: DWORD = 3;
const CERT_SHA1_HASH_PROP_ID: DWORD = 3;

#[repr(C)]
struct LUID {
    low_part: DWORD,
    high_part: i32,
}

#[repr(C)]
struct LUID_AND_ATTRIBUTES {
    luid: LUID,
    attributes: DWORD,
}

#[repr(C)]
struct TOKEN_PRIVILEGES {
    privilege_count: DWORD,
    privileges: [LUID_AND_ATTRIBUTES; 1],
}

#[repr(C)]
pub struct CERT_CONTEXT {
    _unused: [u8; 0],
}

pub type PCCERT_CONTEXT = *const CERT_CONTEXT;

#[link(name = "advapi32")]
extern "system" {
    fn RegLoadKeyW(hkey: HKEY, lp_sub_key: *const u16, lp_file: *const u16) -> DWORD;
    fn RegUnLoadKeyW(hkey: HKEY, lp_sub_key: *const u16) -> DWORD;
    fn RegOpenKeyExW(
        hkey: HKEY,
        lp_sub_key: *const u16,
        ul_options: DWORD,
        sam_desired: DWORD,
        phk_result: *mut HKEY,
    ) -> DWORD;
    fn RegCreateKeyExW(
        hkey: HKEY,
        lp_sub_key: *const u16,
        reserved: DWORD,
        lp_class: *mut u16,
        dw_options: DWORD,
        sam_desired: DWORD,
        lp_security_attributes: *mut c_void,
        phk_result: *mut HKEY,
        lpdw_disposition: *mut DWORD,
    ) -> DWORD;
    fn RegCloseKey(hkey: HKEY) -> DWORD;

    fn OpenProcessToken(
        process_handle: HANDLE,
        desired_access: DWORD,
        token_handle: *mut HANDLE,
    ) -> BOOL;
    fn LookupPrivilegeValueW(
        lp_system_name: *const u16,
        lp_name: *const u16,
        lp_luid: *mut LUID,
    ) -> BOOL;
    fn AdjustTokenPrivileges(
        token_handle: HANDLE,
        disable_all_privileges: BOOL,
        new_state: *const TOKEN_PRIVILEGES,
        buffer_length: DWORD,
        previous_state: *mut TOKEN_PRIVILEGES,
        return_length: *mut DWORD,
    ) -> BOOL;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> DWORD;
    fn GetCurrentProcess() -> HANDLE;
    fn CloseHandle(handle: HANDLE) -> BOOL;
    fn GetCurrentProcessId() -> DWORD;
}

#[link(name = "crypt32")]
extern "system" {
    fn CertOpenStore(
        lpsz_store_provider: *const i8,
        dw_msg_and_cert_encoding_type: DWORD,
        hcrypt_prov: HANDLE,
        dw_flags: DWORD,
        pv_para: *const c_void,
    ) -> HCERTSTORE;
    fn CertCloseStore(hcertstore: HCERTSTORE, dw_flags: DWORD) -> BOOL;
    fn CertAddEncodedCertificateToStore(
        hcertstore: HCERTSTORE,
        dw_cert_encoding_type: DWORD,
        pb_cert_encoded: *const u8,
        cb_cert_encoded: DWORD,
        dw_add_disposition: DWORD,
        pp_cert_context: *mut PCCERT_CONTEXT,
    ) -> BOOL;
    fn CertCreateCertificateContext(
        dw_cert_encoding_type: DWORD,
        pb_cert_encoded: *const u8,
        cb_cert_encoded: DWORD,
    ) -> PCCERT_CONTEXT;
    fn CertGetCertificateContextProperty(
        pcert_context: PCCERT_CONTEXT,
        dw_prop_id: DWORD,
        pv_data: *mut c_void,
        pcb_data: *mut DWORD,
    ) -> BOOL;
    fn CertFreeCertificateContext(pcert_context: PCCERT_CONTEXT) -> BOOL;
}

pub fn enable_required_privileges() -> Result<(), WinError> {
    enable_privilege("SeRestorePrivilege")?;
    enable_privilege("SeBackupPrivilege")?;
    Ok(())
}

fn enable_privilege(name: &str) -> Result<(), WinError> {
    let mut token: HANDLE = 0;
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return Err(last_error("OpenProcessToken"));
    }

    struct TokenGuard(HANDLE);
    impl Drop for TokenGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let _guard = TokenGuard(token);

    let name_w = wide_null_from_str(name);
    let mut luid = LUID {
        low_part: 0,
        high_part: 0,
    };
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), name_w.as_ptr(), &mut luid) };
    if ok == 0 {
        return Err(last_error(format!("LookupPrivilegeValueW({name})")));
    }

    let tp = TOKEN_PRIVILEGES {
        privilege_count: 1,
        privileges: [LUID_AND_ATTRIBUTES {
            luid,
            attributes: SE_PRIVILEGE_ENABLED,
        }],
    };

    let ok = unsafe {
        AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut())
    };
    if ok == 0 {
        return Err(last_error(format!("AdjustTokenPrivileges({name})")));
    }
    let code = unsafe { GetLastError() };
    if code == ERROR_NOT_ALL_ASSIGNED {
        return Err(WinError {
            context: format!("AdjustTokenPrivileges({name}) did not assign the privilege (are you running elevated?)"),
            code,
        });
    }

    Ok(())
}

pub fn choose_unique_mount_name(base: &str) -> Result<String, WinError> {
    if !reg_key_exists(HKEY_LOCAL_MACHINE, base)? {
        return Ok(base.to_string());
    }

    let pid = unsafe { GetCurrentProcessId() };
    for i in 0u32..1000 {
        let candidate = format!("{base}_{pid}_{i}");
        if !reg_key_exists(HKEY_LOCAL_MACHINE, &candidate)? {
            return Ok(candidate);
        }
    }

    Err(WinError {
        context: format!("failed to find a free mount name based on {base}"),
        code: 0,
    })
}

fn reg_key_exists(parent: HKEY, subkey: &str) -> Result<bool, WinError> {
    let subkey_w = wide_null_from_str(subkey);
    let mut out: HKEY = 0;
    let status = unsafe { RegOpenKeyExW(parent, subkey_w.as_ptr(), 0, KEY_READ, &mut out) };
    if status == ERROR_SUCCESS {
        unsafe {
            RegCloseKey(out);
        }
        return Ok(true);
    }
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    Err(WinError {
        context: format!("RegOpenKeyExW({subkey})"),
        code: status,
    })
}

pub struct LoadedHive {
    mount_name: Vec<u16>,
    loaded: bool,
}

impl LoadedHive {
    pub fn load(hive_path: &Path, mount_name: &str) -> Result<Self, WinError> {
        let mount_name_w = wide_null_from_str(mount_name);
        let hive_path_w = wide_null_from_path(hive_path);
        let status = unsafe {
            RegLoadKeyW(
                HKEY_LOCAL_MACHINE,
                mount_name_w.as_ptr(),
                hive_path_w.as_ptr(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(WinError {
                context: format!("RegLoadKeyW({mount_name})"),
                code: status,
            });
        }
        Ok(Self {
            mount_name: mount_name_w,
            loaded: true,
        })
    }

    pub fn unload(&mut self) -> Result<(), WinError> {
        if !self.loaded {
            return Ok(());
        }
        let status = unsafe { RegUnLoadKeyW(HKEY_LOCAL_MACHINE, self.mount_name.as_ptr()) };
        if status != ERROR_SUCCESS {
            return Err(WinError {
                context: "RegUnLoadKeyW".to_string(),
                code: status,
            });
        }
        self.loaded = false;
        Ok(())
    }
}

impl Drop for LoadedHive {
    fn drop(&mut self) {
        if !self.loaded {
            return;
        }
        if let Err(err) = self.unload() {
            eprintln!("warning: failed to unload offline hive: {err}");
        }
    }
}

pub struct RegKey(HKEY);

impl RegKey {
    pub fn open(parent: HKEY, subkey: &str, writable: bool) -> Result<Self, WinError> {
        let subkey_w = wide_null_from_str(subkey);
        let mut out: HKEY = 0;
        let sam_desired = if writable { KEY_ALL_ACCESS } else { KEY_READ };
        let status = unsafe { RegOpenKeyExW(parent, subkey_w.as_ptr(), 0, sam_desired, &mut out) };
        if status != ERROR_SUCCESS {
            return Err(WinError {
                context: format!("RegOpenKeyExW({subkey})"),
                code: status,
            });
        }
        Ok(Self(out))
    }

    pub fn create(parent: HKEY, subkey: &str) -> Result<Self, WinError> {
        let subkey_w = wide_null_from_str(subkey);
        let mut out: HKEY = 0;
        let mut disposition: DWORD = 0;
        let status = unsafe {
            RegCreateKeyExW(
                parent,
                subkey_w.as_ptr(),
                0,
                std::ptr::null_mut(),
                0,
                KEY_ALL_ACCESS,
                std::ptr::null_mut(),
                &mut out,
                &mut disposition,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(WinError {
                context: format!("RegCreateKeyExW({subkey})"),
                code: status,
            });
        }
        Ok(Self(out))
    }

    pub fn raw(&self) -> HKEY {
        self.0
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

pub struct CertStore(HCERTSTORE);

impl CertStore {
    pub fn open_system_registry(store_key: HKEY, readonly: bool) -> Result<Self, WinError> {
        let flags = if readonly {
            CERT_STORE_READONLY_FLAG
        } else {
            CERT_STORE_MAXIMUM_ALLOWED_FLAG
        };
        let store = unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_REGISTRY_W,
                X509_ASN_ENCODING | PKCS_7_ASN_ENCODING,
                0,
                flags,
                store_key as *const c_void,
            )
        };
        if store.is_null() {
            return Err(last_error(
                "CertOpenStore(CERT_STORE_PROV_SYSTEM_REGISTRY_W)",
            ));
        }
        Ok(Self(store))
    }
}

impl Drop for CertStore {
    fn drop(&mut self) {
        unsafe {
            CertCloseStore(self.0, 0);
        }
    }
}

pub fn cert_add_encoded_cert(store: &CertStore, cert_der: &[u8]) -> Result<(), WinError> {
    let ok = unsafe {
        CertAddEncodedCertificateToStore(
            store.0,
            X509_ASN_ENCODING,
            cert_der.as_ptr(),
            cert_der.len() as DWORD,
            CERT_STORE_ADD_REPLACE_EXISTING,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error("CertAddEncodedCertificateToStore"));
    }
    Ok(())
}

pub fn cert_sha1_thumbprint(cert_der: &[u8]) -> Result<[u8; 20], WinError> {
    let ctx = unsafe {
        CertCreateCertificateContext(
            X509_ASN_ENCODING,
            cert_der.as_ptr(),
            cert_der.len() as DWORD,
        )
    };
    if ctx.is_null() {
        return Err(last_error("CertCreateCertificateContext"));
    }

    struct CtxGuard(PCCERT_CONTEXT);
    impl Drop for CtxGuard {
        fn drop(&mut self) {
            unsafe {
                CertFreeCertificateContext(self.0);
            }
        }
    }
    let _guard = CtxGuard(ctx);

    let mut hash = [0u8; 20];
    let mut size = hash.len() as DWORD;
    let ok = unsafe {
        CertGetCertificateContextProperty(
            ctx,
            CERT_SHA1_HASH_PROP_ID,
            hash.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if ok == 0 {
        return Err(last_error(
            "CertGetCertificateContextProperty(CERT_SHA1_HASH_PROP_ID)",
        ));
    }
    if size as usize != hash.len() {
        return Err(WinError {
            context: "unexpected CERT_SHA1_HASH_PROP_ID size".to_string(),
            code: size,
        });
    }

    Ok(hash)
}
