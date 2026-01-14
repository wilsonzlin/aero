#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static const NTSTATUS kStatusAccessDenied = (NTSTATUS)0xC0000022L;
static const NTSTATUS kStatusInvalidHandle = (NTSTATUS)0xC0000008L;
static const NTSTATUS kStatusTimeout = (NTSTATUS)0xC0000102L;

static int RunDbgctlEscapeSecuritySanity(int argc, char** argv) {
  const char* kTestName = "dbgctl_escape_security_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout("Usage: %s.exe [--json[=PATH]]", kTestName);
    aerogpu_test::PrintfStdout("");
    aerogpu_test::PrintfStdout("Negative coverage for dbgctl escapes (READ_GPA / MAP_SHARED_HANDLE).");
    aerogpu_test::PrintfStdout("These checks ensure debug tooling escapes do not regress into");
    aerogpu_test::PrintfStdout("arbitrary memory disclosure or kernel-object pinning primitives.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return reporter.Fail("%s", kmt_err.c_str());
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", open_err.c_str());
  }

  bool any_checked = false;
  bool any_supported = false;

  // ---- READ_GPA negative coverage -----------------------------------------
  {
    // Pick a clearly invalid guest physical address far beyond any plausible Win7 guest RAM
    // size, but avoid overflow in gpa+size calculations.
    const unsigned long long kInvalidGpa = 0x8000000000000000ull;
    const uint32_t kReqBytes = 16;

    aerogpu_escape_read_gpa_inout io;
    ZeroMemory(&io, sizeof(io));
    io.hdr.version = AEROGPU_ESCAPE_VERSION;
    io.hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
    io.hdr.size = sizeof(io);
    io.hdr.reserved0 = 0;
    io.gpa = (uint64_t)kInvalidGpa;
    io.size_bytes = (uint32_t)kReqBytes;
    io.reserved0 = 0;
    // Fill with sentinels so we can detect if the driver unexpectedly returns success without
    // initializing fields.
    io.status = 0xDEADBEEFu;
    io.bytes_copied = 0xDEADBEEFu;
    memset(io.data, 0xCC, sizeof(io.data));

    NTSTATUS st = 0;
    const bool ok = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &io, sizeof(io), 2000, &st);
    if (!ok) {
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: READ_GPA escape not supported; skipping READ_GPA coverage", kTestName);
      } else if (st == kStatusTimeout) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("D3DKMTEscape(READ_GPA) timed out");
      } else {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("D3DKMTEscape(READ_GPA) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
      }
    } else {
      any_checked = true;

      const NTSTATUS op_st = (NTSTATUS)(uint32_t)io.status;
      const uint32_t bytes = (uint32_t)io.bytes_copied;

      // If the driver returns STATUS_NOT_SUPPORTED in-band, treat it as a gated/disabled path.
      if (op_st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: READ_GPA gated off (status=STATUS_NOT_SUPPORTED); skipping", kTestName);
      } else {
        any_supported = true;

        aerogpu_test::PrintfStdout("INFO: %s: READ_GPA invalid gpa=0x%I64X size=%lu => status=0x%08lX bytes_copied=%lu",
                                   kTestName,
                                   (unsigned long long)kInvalidGpa,
                                   (unsigned long)kReqBytes,
                                   (unsigned long)op_st,
                                   (unsigned long)bytes);

        if (bytes != 0) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("READ_GPA invalid address unexpectedly copied %lu byte(s)", (unsigned long)bytes);
        }
        if (op_st == 0) {
          aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
          aerogpu_test::kmt::UnloadD3DKMT(&kmt);
          return reporter.Fail("READ_GPA invalid address unexpectedly returned STATUS_SUCCESS");
        }
        // Prefer an explicit deny code; keep the check permissive (any non-success) as long as no
        // bytes were copied. This prevents regressions into memory disclosure primitives even if
        // the exact failure code changes.
        if (op_st != kStatusAccessDenied && op_st != aerogpu_test::kmt::kStatusInvalidParameter) {
          aerogpu_test::PrintfStdout("INFO: %s: READ_GPA denied with unexpected status (still OK): 0x%08lX",
                                     kTestName,
                                     (unsigned long)op_st);
        }
      }
    }
  }

  // ---- MAP_SHARED_HANDLE negative coverage --------------------------------
  {
    struct InvalidCase {
      const char* label;
      unsigned long long handle_value;
    };
    const InvalidCase cases[] = {
        {"0", 0ull},
        // INVALID_HANDLE_VALUE (cast to an unsigned integer of pointer size).
        {"INVALID_HANDLE_VALUE", (unsigned long long)(uintptr_t)INVALID_HANDLE_VALUE},
    };

    bool map_supported = true;
    for (size_t i = 0; i < ARRAYSIZE(cases); ++i) {
      aerogpu_escape_map_shared_handle_inout io;
      ZeroMemory(&io, sizeof(io));
      io.hdr.version = AEROGPU_ESCAPE_VERSION;
      io.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
      io.hdr.size = sizeof(io);
      io.hdr.reserved0 = 0;
      io.shared_handle = (uint64_t)cases[i].handle_value;
      io.debug_token = 0xDEADBEEFu;
      io.reserved0 = 0;

      NTSTATUS st = 0;
      const bool ok = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &io, sizeof(io), 2000, &st);
      if (st == aerogpu_test::kmt::kStatusNotSupported) {
        aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE escape not supported; skipping MAP_SHARED_HANDLE coverage",
                                   kTestName);
        map_supported = false;
        break;
      }
      if (st == kStatusTimeout) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("D3DKMTEscape(MAP_SHARED_HANDLE %s) timed out", cases[i].label);
      }

      // For invalid handles, *success* is the unsafe outcome.
      if (ok) {
        aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
        aerogpu_test::kmt::UnloadD3DKMT(&kmt);
        return reporter.Fail("MAP_SHARED_HANDLE(%s) unexpectedly succeeded (debug_token=%lu)",
                             cases[i].label,
                             (unsigned long)io.debug_token);
      }

      any_checked = true;
      any_supported = true;

      aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE(%s) failed as expected (NTSTATUS=0x%08lX)",
                                 kTestName,
                                 cases[i].label,
                                 (unsigned long)st);

      if (st != aerogpu_test::kmt::kStatusInvalidParameter && st != kStatusInvalidHandle &&
          st != kStatusAccessDenied) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: MAP_SHARED_HANDLE(%s) returned unexpected failure (still OK): 0x%08lX",
            kTestName,
            cases[i].label,
            (unsigned long)st);
      }
    }

    // Optional: if MAP_SHARED_HANDLE works for a valid handle, ensure the token is stable.
    if (map_supported) {
      HANDLE section = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, 4096, NULL);
      if (!section) {
        aerogpu_test::PrintfStdout("INFO: %s: CreateFileMapping failed; skipping MAP_SHARED_HANDLE stability check: %s",
                                   kTestName,
                                   aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      } else {
        aerogpu_escape_map_shared_handle_inout io1;
        ZeroMemory(&io1, sizeof(io1));
        io1.hdr.version = AEROGPU_ESCAPE_VERSION;
        io1.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
        io1.hdr.size = sizeof(io1);
        io1.hdr.reserved0 = 0;
        io1.shared_handle = (uint64_t)(uintptr_t)section;
        io1.debug_token = 0;
        io1.reserved0 = 0;

        NTSTATUS st1 = 0;
        const bool ok1 = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &io1, sizeof(io1), 2000, &st1);
        if (st1 == aerogpu_test::kmt::kStatusNotSupported) {
          aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE not supported; skipping stability check", kTestName);
        } else if (!ok1) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: MAP_SHARED_HANDLE(valid section) failed (NTSTATUS=0x%08lX); skipping stability check",
              kTestName,
              (unsigned long)st1);
        } else if (io1.debug_token == 0) {
          aerogpu_test::PrintfStdout(
              "INFO: %s: MAP_SHARED_HANDLE(valid section) returned debug_token=0; skipping stability check",
              kTestName);
        } else {
          aerogpu_escape_map_shared_handle_inout io2 = io1;
          io2.debug_token = 0;

          NTSTATUS st2 = 0;
          const bool ok2 = aerogpu_test::kmt::AerogpuEscapeWithTimeout(&kmt, adapter, &io2, sizeof(io2), 2000, &st2);
          if (!ok2) {
            aerogpu_test::PrintfStdout(
                "INFO: %s: MAP_SHARED_HANDLE second call failed (NTSTATUS=0x%08lX); skipping stability check",
                kTestName,
                (unsigned long)st2);
          } else if (io2.debug_token != io1.debug_token) {
            CloseHandle(section);
            aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
            aerogpu_test::kmt::UnloadD3DKMT(&kmt);
            return reporter.Fail("MAP_SHARED_HANDLE returned unstable debug_token (%lu -> %lu)",
                                 (unsigned long)io1.debug_token,
                                 (unsigned long)io2.debug_token);
          } else {
            aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE debug_token stable (%lu)",
                                       kTestName,
                                       (unsigned long)io1.debug_token);
          }
        }

        CloseHandle(section);
      }
    }
  }

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!any_checked) {
    aerogpu_test::PrintfStdout("INFO: %s: dbgctl escapes not supported; skipping", kTestName);
    reporter.SetSkipped("not_supported");
    return reporter.Pass();
  }

  if (!any_supported) {
    aerogpu_test::PrintfStdout("INFO: %s: dbgctl escapes gated off; skipping", kTestName);
    reporter.SetSkipped("gated_off");
    return reporter.Pass();
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunDbgctlEscapeSecuritySanity(argc, argv);
}

