#pragma once

// Win32 object security helpers shared across AeroGPU UMDs.
//
// The AeroGPU UMDs use named file mappings as cross-process counters
// (e.g. GlobalHandleCounter, D3D9 ShareToken). Historically these were created
// with a NULL DACL (allow all) so any process in the session can open them.
//
// On Windows Vista+ Mandatory Integrity Control (MIC) is enforced separately
// from the DACL. A NULL DACL does not automatically grant Low Integrity access
// when the object has a higher integrity label (e.g. Medium). To make the system
// robust when a sandboxed/Low IL process needs to open these mappings, we try to
// create them with an explicit Low integrity label.
//
// This is best-effort: if SDDL conversion is unavailable (e.g. advapi32 missing)
// or fails for any reason, we fall back to the previous NULL DACL behaviour.

#if defined(_WIN32)

  #include <windows.h>

namespace aerogpu {
namespace win32 {

inline bool TryBuildLowIntegritySecurityDescriptor(PSECURITY_DESCRIPTOR* out_sd) {
  if (!out_sd) {
    return false;
  }
  *out_sd = nullptr;

  using ConvertFn = BOOL(WINAPI*)(LPCWSTR, DWORD, PSECURITY_DESCRIPTOR*, PULONG);
  static ConvertFn convert = []() -> ConvertFn {
    HMODULE advapi = GetModuleHandleW(L"advapi32.dll");
    if (!advapi) {
      advapi = LoadLibraryW(L"advapi32.dll");
    }
    if (!advapi) {
      return nullptr;
    }
    return reinterpret_cast<ConvertFn>(
        GetProcAddress(advapi, "ConvertStringSecurityDescriptorToSecurityDescriptorW"));
  }();

  if (!convert) {
    return false;
  }

  // DACL: Everyone full access.
  // SACL: Low integrity label with No-Write-Up.
  constexpr LPCWSTR kSddl = L"D:(A;;GA;;;WD)S:(ML;;NW;;;LW)";
  PSECURITY_DESCRIPTOR sd = nullptr;
  if (convert(kSddl, 1 /* SDDL_REVISION_1 */, &sd, nullptr) == FALSE || !sd) {
    if (sd) {
      LocalFree(sd);
    }
    return false;
  }

  *out_sd = sd;
  return true;
}

inline HANDLE CreateFileMappingWBestEffortLowIntegrity(
    HANDLE hFile,
    DWORD flProtect,
    DWORD dwMaximumSizeHigh,
    DWORD dwMaximumSizeLow,
    LPCWSTR lpName) {
  SECURITY_ATTRIBUTES sa{};
  sa.nLength = sizeof(sa);
  sa.bInheritHandle = FALSE;
  sa.lpSecurityDescriptor = nullptr;

  SECURITY_DESCRIPTOR null_dacl_sd{};
  const bool null_dacl_ok = (InitializeSecurityDescriptor(&null_dacl_sd, SECURITY_DESCRIPTOR_REVISION) != FALSE &&
                             SetSecurityDescriptorDacl(&null_dacl_sd, TRUE, nullptr, FALSE) != FALSE);

  PSECURITY_DESCRIPTOR sddl_sd = nullptr;
  if (TryBuildLowIntegritySecurityDescriptor(&sddl_sd)) {
    sa.lpSecurityDescriptor = sddl_sd;
    HANDLE mapping = CreateFileMappingW(hFile, &sa, flProtect, dwMaximumSizeHigh, dwMaximumSizeLow, lpName);
    LocalFree(sddl_sd);
    if (mapping) {
      return mapping;
    }
  }

  sa.lpSecurityDescriptor = null_dacl_ok ? &null_dacl_sd : nullptr;
  return CreateFileMappingW(hFile, &sa, flProtect, dwMaximumSizeHigh, dwMaximumSizeLow, lpName);
}

// Helper that provides a SECURITY_ATTRIBUTES suitable for creating named objects
// that must be accessible cross-process and across integrity levels.
//
// - Attempts to use an SDDL-based descriptor with a Low integrity label.
// - Falls back to a NULL DACL if SDDL conversion fails.
// - Always sets bInheritHandle=FALSE.
struct FileMappingSecurityAttributes final {
  SECURITY_ATTRIBUTES sa{};
  SECURITY_DESCRIPTOR null_dacl_sd{};
  PSECURITY_DESCRIPTOR sddl_sd = nullptr;
  bool has_low_integrity_label = false;

  FileMappingSecurityAttributes() {
    sa.nLength = sizeof(sa);
    sa.bInheritHandle = FALSE;
    sa.lpSecurityDescriptor = nullptr;

    if (TryBuildLowIntegritySecurityDescriptor(&sddl_sd)) {
      sa.lpSecurityDescriptor = sddl_sd;
      has_low_integrity_label = true;
      return;
    }

    if (InitializeSecurityDescriptor(&null_dacl_sd, SECURITY_DESCRIPTOR_REVISION) != FALSE &&
        SetSecurityDescriptorDacl(&null_dacl_sd, TRUE, nullptr, FALSE) != FALSE) {
      sa.lpSecurityDescriptor = &null_dacl_sd; // NULL DACL => allow all access
    } else {
      sa.lpSecurityDescriptor = nullptr; // best-effort; allow CreateFileMappingW to decide
    }
  }

  ~FileMappingSecurityAttributes() {
    if (sddl_sd) {
      LocalFree(sddl_sd);
    }
  }

  FileMappingSecurityAttributes(const FileMappingSecurityAttributes&) = delete;
  FileMappingSecurityAttributes& operator=(const FileMappingSecurityAttributes&) = delete;
};

} // namespace win32
} // namespace aerogpu

#endif // defined(_WIN32)
