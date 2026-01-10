//! Windows 7 focused BCD well-known object GUIDs and element type IDs.
//!
//! These values are required for deterministic, offline BCD patching. They are stable across
//! Windows 7 installs and are the same values shown by `bcdedit /enum all` and/or under
//! `Objects\\{GUID}` when loading the BCD hive with `reg.exe load`.

// --- Well-known object identifiers (GUIDs) ---
//
// The BCD store uses GUIDs as registry keys under `Objects`. `bcdedit` exposes
// some objects via friendly aliases like `{bootmgr}`; the underlying GUID is
// what we match on in offline patching.
//
// Stored as lowercase without braces; matching must be case-insensitive and
// tolerant of optional `{}` in the hive.

/// `{bootmgr}` – Windows Boot Manager object GUID.
pub const GUID_BOOTMGR: &str = "9dea862c-5cdd-4e70-acc1-f32b344d4795";

/// `{globalsettings}` – global library settings inherited by many objects.
pub const GUID_GLOBAL_SETTINGS: &str = "7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e";

/// `{bootloadersettings}` – template object inherited by Windows Boot Loader entries.
pub const GUID_BOOTLOADER_SETTINGS: &str = "6efb52bf-1766-41db-a6b3-0ee5eff72bd7";

/// `{resumeloadersettings}` – template object inherited by Windows Resume Loader entries.
pub const GUID_RESUMELOADER_SETTINGS: &str = "1afa9c49-16ab-4a5c-901b-212802da9460";

/// `{memdiag}` – Windows Memory Diagnostic entry (present on most Win7 installs).
pub const GUID_MEMDIAG: &str = "b2721d73-1db4-4c62-bf78-c548a880142d";

/// `{ntldr}` – legacy NTLDR entry (only present if explicitly created).
pub const GUID_NTLDR: &str = "466f5a88-0af2-4f76-9038-095b170dc21c";

/// `{bootmgr}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_BOOTMGR: &str = "{9dea862c-5cdd-4e70-acc1-f32b344d4795}";

/// `{globalsettings}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_GLOBALSETTINGS: &str = "{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}";

/// `{bootloadersettings}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_BOOTLOADERSETTINGS: &str = "{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}";

/// `{resumeloadersettings}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_RESUMELOADERSETTINGS: &str = "{1afa9c49-16ab-4a5c-901b-212802da9460}";

/// `{memdiag}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_MEMDIAG: &str = "{b2721d73-1db4-4c62-bf78-c548a880142d}";

/// `{ntldr}` object key name as stored under `Objects` in the BCD hive.
pub const OBJ_NTLDR: &str = "{466f5a88-0af2-4f76-9038-095b170dc21c}";

// --- Element type IDs ---
//
// The BCD registry hive stores element subkeys under `Objects\\{GUID}\\Elements`
// named as 8-digit hex values, e.g. `16000048`. These correspond to
// `BCD_ELEMENT_TYPE` values in the Windows SDK/WDK `bcd.h`.

/// `nointegritychecks` – Disable integrity checks (library boolean).
pub const ELEM_DISABLE_INTEGRITY_CHECKS: u32 = 0x1600_0048;

/// `testsigning` – Allow prerelease signatures (library boolean).
pub const ELEM_ALLOW_PRERELEASE_SIGNATURES: u32 = 0x1600_0049;

/// `applicationpath` – Application path for loader objects (library string).
pub const ELEM_APPLICATION_PATH: u32 = 0x1200_0002;

/// `{bootmgr} default` – Default boot entry (bootmgr object element).
pub const ELEM_BOOTMGR_DEFAULT_OBJECT: u32 = 0x2300_0003;

/// `{bootmgr} displayorder` – Display order list of boot entries (bootmgr object list element).
pub const ELEM_BOOTMGR_DISPLAY_ORDER: u32 = 0x2400_0001;
