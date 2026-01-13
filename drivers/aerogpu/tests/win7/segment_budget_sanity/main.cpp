#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include "..\\..\\..\\protocol\\aerogpu_win7_abi.h"
#include "..\\..\\..\\protocol\\aerogpu_umd_private.h"

#include <initguid.h>
#include <devguid.h>
#include <setupapi.h>

using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

namespace {

static const uint64_t kOneMiB = 1024ull * 1024ull;

static const uint32_t kExpectedPagingBufferPrivateDataSize = AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
static const uint32_t kExpectedPagingBufferSegmentId = 1;  // AEROGPU_SEGMENT_ID_SYSTEM

static const wchar_t* kAeroGpuHwidNeedle = L"PCI\\VEN_A3A0&DEV_0001";

struct SegmentGroupSize {
  uint64_t LocalMemorySize;
  uint64_t NonLocalMemorySize;
};

struct QuerySegmentParsed {
  bool present;
  UINT type;

  uint32_t nb_segments;
  uint32_t paging_buffer_private_data_size;
  uint32_t paging_buffer_segment_id;

  uint64_t seg0_base;
  uint64_t seg0_size;
  uint32_t seg0_flags_value;
  uint32_t seg0_group;

  QuerySegmentParsed()
      : present(false),
        type(0xFFFFFFFFu),
        nb_segments(0),
        paging_buffer_private_data_size(0),
        paging_buffer_segment_id(0),
        seg0_base(0),
        seg0_size(0),
        seg0_flags_value(0),
        seg0_group(0) {}
};

static bool IsOs64Bit() {
  // If this is a native 64-bit process OR a 32-bit process running under WOW64, the OS is x64.
  return aerogpu_test::Is64BitProcess() || aerogpu_test::IsRunningUnderWow64();
}

static uint32_t ClampNonLocalMbForOs(uint32_t mb) {
  const uint32_t min_mb = 128;
  const uint32_t max_mb = IsOs64Bit() ? 2048 : 1024;
  if (mb < min_mb) {
    return min_mb;
  }
  if (mb > max_mb) {
    return max_mb;
  }
  return mb;
}

static uint64_t ClampMaxNonLocalBytesForOs() {
  return (IsOs64Bit() ? 2048ull : 1024ull) * kOneMiB;
}

static bool ReadU32At(const unsigned char* buf, size_t buf_size, size_t off, uint32_t* out) {
  if (out) {
    *out = 0;
  }
  if (!buf || !out || off + 4 > buf_size) {
    return false;
  }
  uint32_t v = 0;
  memcpy(&v, buf + off, sizeof(v));
  *out = v;
  return true;
}

static bool ReadU64At(const unsigned char* buf, size_t buf_size, size_t off, uint64_t* out) {
  if (out) {
    *out = 0;
  }
  if (!buf || !out || off + 8 > buf_size) {
    return false;
  }
  uint64_t v = 0;
  memcpy(&v, buf + off, sizeof(v));
  *out = v;
  return true;
}

static bool ReadPtrAt(const unsigned char* buf, size_t buf_size, size_t off, size_t ptr_size, uintptr_t* out) {
  if (out) {
    *out = 0;
  }
  if (!buf || !out || (ptr_size != 4 && ptr_size != 8) || off + ptr_size > buf_size) {
    return false;
  }
  if (ptr_size == 4) {
    uint32_t v32 = 0;
    memcpy(&v32, buf + off, sizeof(v32));
    *out = (uintptr_t)v32;
    return true;
  }
  uint64_t v64 = 0;
  memcpy(&v64, buf + off, sizeof(v64));
  *out = (uintptr_t)v64;
  return true;
}

static bool ParseSegmentDescriptorAt(const unsigned char* buf,
                                    size_t buf_size,
                                    size_t desc_off,
                                    uint64_t* out_base,
                                    uint64_t* out_size,
                                    uint32_t* out_flags,
                                    uint32_t* out_group) {
  if (out_base) *out_base = 0;
  if (out_size) *out_size = 0;
  if (out_flags) *out_flags = 0;
  if (out_group) *out_group = 0;
  if (!buf) {
    return false;
  }

  uint64_t base = 0;
  if (!ReadU64At(buf, buf_size, desc_off + 0, &base)) {
    return false;
  }

  // Try a 64-bit size layout first:
  //   base(u64), size(u64), flags(u32), group(u32)
  {
    uint64_t size64 = 0;
    uint32_t flags = 0;
    uint32_t group = 0;
    if (ReadU64At(buf, buf_size, desc_off + 8, &size64) &&
        ReadU32At(buf, buf_size, desc_off + 16, &flags) &&
        ReadU32At(buf, buf_size, desc_off + 20, &group)) {
      if (size64 >= 16ull * kOneMiB && size64 <= (1ull << 50) && (size64 % kOneMiB) == 0) {
        if (out_base) *out_base = base;
        if (out_size) *out_size = size64;
        if (out_flags) *out_flags = flags;
        if (out_group) *out_group = group;
        return true;
      }
    }
  }

  // Fallback: some layouts use a 32-bit size on x86:
  //   base(u64), size(u32), flags(u32), group(u32)
  {
    uint32_t size32 = 0;
    uint32_t flags = 0;
    uint32_t group = 0;
    if (ReadU32At(buf, buf_size, desc_off + 8, &size32) &&
        ReadU32At(buf, buf_size, desc_off + 12, &flags) &&
        ReadU32At(buf, buf_size, desc_off + 16, &group)) {
      uint64_t size64 = (uint64_t)size32;
      if (size64 >= 16ull * kOneMiB && size64 <= (1ull << 32) && (size64 % kOneMiB) == 0) {
        if (out_base) *out_base = base;
        if (out_size) *out_size = size64;
        if (out_flags) *out_flags = flags;
        if (out_group) *out_group = group;
        return true;
      }
    }
  }

  return false;
}

static bool TryParseQuerySegment(const unsigned char* buf, size_t buf_size, QuerySegmentParsed* out) {
  if (!out) {
    return false;
  }
  *out = QuerySegmentParsed();
  if (!buf || buf_size < 32) {
    return false;
  }

  // We expect (based on WDDM) the first fields to be:
  //   NbSegments, PagingBufferPrivateDataSize, PagingBufferSegmentId, ...
  uint32_t nb = 0;
  uint32_t pb_priv = 0;
  uint32_t pb_seg = 0;
  if (!ReadU32At(buf, buf_size, 0, &nb) ||
      !ReadU32At(buf, buf_size, 4, &pb_priv) ||
      !ReadU32At(buf, buf_size, 8, &pb_seg)) {
    return false;
  }
  out->nb_segments = nb;
  out->paging_buffer_private_data_size = pb_priv;
  out->paging_buffer_segment_id = pb_seg;

  // Best-effort: locate the segment descriptor pointer (if present) by scanning for a pointer
  // value that points back into this output buffer.
  const uintptr_t base = (uintptr_t)buf;
  const uintptr_t end = base + buf_size;
  size_t desc_off = (size_t)-1;

  // Try pointer-sized fields first (matches the process bitness), then fall back to the other size
  // in case the thunk uses a fixed-width pointer field.
  const size_t ptr_sizes[2] = {sizeof(void*), sizeof(void*) == 8 ? (size_t)4 : (size_t)8};
  for (int ptr_pass = 0; ptr_pass < 2; ++ptr_pass) {
    const size_t ptr_size = ptr_sizes[ptr_pass];
    if (ptr_size == 8 && !aerogpu_test::Is64BitProcess()) {
      // Don't try 8-byte pointers in a 32-bit process; they cannot be valid user pointers.
      continue;
    }
    const size_t scan_limit = (buf_size < 64) ? buf_size : 64;
    for (size_t off = 0; off + ptr_size <= scan_limit; off += 4) {
      uintptr_t ptr = 0;
      if (!ReadPtrAt(buf, buf_size, off, ptr_size, &ptr)) {
        continue;
      }
      if (ptr < base || ptr >= end) {
        continue;
      }
      const size_t cand_off = (size_t)(ptr - base);
      uint64_t seg_base = 0;
      uint64_t seg_size = 0;
      uint32_t seg_flags = 0;
      uint32_t seg_group = 0;
      if (ParseSegmentDescriptorAt(buf, buf_size, cand_off, &seg_base, &seg_size, &seg_flags, &seg_group)) {
        desc_off = cand_off;
        out->seg0_base = seg_base;
        out->seg0_size = seg_size;
        out->seg0_flags_value = seg_flags;
        out->seg0_group = seg_group;
        return true;
      }
    }
  }

  // Fallback: scan the buffer for a plausible descriptor with base==0.
  for (size_t off = 0; off + 24 <= buf_size; off += 4) {
    uint64_t seg_base = 0;
    if (!ReadU64At(buf, buf_size, off, &seg_base)) {
      continue;
    }
    if (seg_base != 0) {
      continue;
    }
    uint64_t seg_size = 0;
    uint32_t seg_flags = 0;
    uint32_t seg_group = 0;
    if (ParseSegmentDescriptorAt(buf, buf_size, off, &seg_base, &seg_size, &seg_flags, &seg_group)) {
      desc_off = off;
      out->seg0_base = seg_base;
      out->seg0_size = seg_size;
      out->seg0_flags_value = seg_flags;
      out->seg0_group = seg_group;
      return true;
    }
  }

  // Header parsed, but couldn't find the descriptor array reliably.
  return true;
}

static bool MultiSzContainsCaseInsensitive(const wchar_t* multi_sz, const wchar_t* needle) {
  if (!multi_sz || !needle || !*needle) {
    return false;
  }
  for (const wchar_t* p = multi_sz; *p; p += wcslen(p) + 1) {
    if (aerogpu_test::StrIContainsW(p, needle)) {
      return true;
    }
  }
  return false;
}

static bool ReadAeroGpuNonLocalMemorySizeMbFromRegistry(uint32_t* out_mb, std::string* err) {
  if (out_mb) {
    *out_mb = 0;
  }
  if (err) {
    err->clear();
  }
  if (!out_mb) {
    if (err) {
      *err = "out_mb == NULL";
    }
    return false;
  }

  HDEVINFO devs = SetupDiGetClassDevsW(&GUID_DEVCLASS_DISPLAY, NULL, NULL, DIGCF_PRESENT);
  if (devs == INVALID_HANDLE_VALUE) {
    if (err) {
      *err = "SetupDiGetClassDevsW failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  bool found = false;
  uint32_t mb = 0;
  std::string last_err;

  SP_DEVINFO_DATA devinfo;
  ZeroMemory(&devinfo, sizeof(devinfo));
  devinfo.cbSize = sizeof(devinfo);

  for (DWORD idx = 0; SetupDiEnumDeviceInfo(devs, idx, &devinfo); ++idx) {
    wchar_t hwid[4096];
    ZeroMemory(hwid, sizeof(hwid));
    DWORD reg_type = 0;
    DWORD required = 0;
    if (!SetupDiGetDeviceRegistryPropertyW(devs,
                                           &devinfo,
                                           SPDRP_HARDWAREID,
                                           &reg_type,
                                           (PBYTE)hwid,
                                           sizeof(hwid),
                                           &required)) {
      continue;
    }
    if (reg_type != REG_MULTI_SZ) {
      continue;
    }
    if (!MultiSzContainsCaseInsensitive(hwid, kAeroGpuHwidNeedle)) {
      continue;
    }

    // Found the AeroGPU display adapter. Read HKR\Parameters\NonLocalMemorySizeMB.
    HKEY drv_key = SetupDiOpenDevRegKey(devs, &devinfo, DICS_FLAG_GLOBAL, 0, DIREG_DRV, KEY_READ);
    if (drv_key == INVALID_HANDLE_VALUE) {
      last_err = "SetupDiOpenDevRegKey failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
      continue;
    }

    HKEY params_key = NULL;
    LONG r = RegOpenKeyExW(drv_key, L"Parameters", 0, KEY_READ, &params_key);
    RegCloseKey(drv_key);
    drv_key = NULL;
    if (r != ERROR_SUCCESS) {
      // Parameters subkey may not exist if the driver isn't installed via the INF yet.
      last_err = "RegOpenKeyExW(Parameters) failed: " + aerogpu_test::Win32ErrorToString((DWORD)r);
      continue;
    }

    DWORD value_type = 0;
    DWORD value = 0;
    DWORD value_size = sizeof(value);
    r = RegQueryValueExW(params_key,
                         L"NonLocalMemorySizeMB",
                         NULL,
                         &value_type,
                         (LPBYTE)&value,
                         &value_size);
    RegCloseKey(params_key);
    params_key = NULL;

    if (r != ERROR_SUCCESS) {
      last_err = "RegQueryValueExW(NonLocalMemorySizeMB) failed: " + aerogpu_test::Win32ErrorToString((DWORD)r);
      continue;
    }
    if (value_type != REG_DWORD || value_size != sizeof(DWORD)) {
      last_err = "NonLocalMemorySizeMB has unexpected registry type/size";
      continue;
    }

    mb = (uint32_t)value;
    found = true;
    break;
  }

  const DWORD enum_err = GetLastError();
  SetupDiDestroyDeviceInfoList(devs);

  if (!found) {
    if (enum_err != ERROR_NO_MORE_ITEMS && enum_err != ERROR_SUCCESS) {
      if (err) {
        *err = "SetupDiEnumDeviceInfo failed: " + aerogpu_test::Win32ErrorToString(enum_err);
      }
      return false;
    }
    if (err) {
      *err = last_err;
    }
    return false;
  }

  *out_mb = mb;
  return true;
}

static bool VerifyAeroGpuAdapterViaEscape(const D3DKMT_FUNCS* kmt,
                                         D3DKMT_HANDLE adapter,
                                         std::string* out_err) {
  if (out_err) {
    out_err->clear();
  }
  if (!kmt || !adapter) {
    if (out_err) {
      *out_err = "VerifyAeroGpuAdapterViaEscape: invalid args";
    }
    return false;
  }

  // Prefer QUERY_DEVICE_V2 (newer KMD); fall back to legacy QUERY_DEVICE if needed.
  aerogpu_escape_query_device_v2_out q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;

  NTSTATUS st = 0;
  if (aerogpu_test::kmt::AerogpuEscapeWithTimeout(kmt, adapter, &q2, sizeof(q2), 2000, &st)) {
    if (q2.hdr.version != AEROGPU_ESCAPE_VERSION || q2.hdr.op != AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2 ||
        q2.hdr.size != sizeof(q2)) {
      if (out_err) {
        *out_err = aerogpu_test::FormatString("invalid QUERY_DEVICE_V2 header (version=%lu op=%lu size=%lu)",
                                              (unsigned long)q2.hdr.version,
                                              (unsigned long)q2.hdr.op,
                                              (unsigned long)q2.hdr.size);
      }
      return false;
    }

    const uint32_t magic = (uint32_t)q2.detected_mmio_magic;
    if (magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      if (out_err) {
        *out_err = aerogpu_test::FormatString("unexpected AeroGPU MMIO magic (0x%08lX)", (unsigned long)magic);
      }
      return false;
    }
    return true;
  }

  if (st != aerogpu_test::kmt::kStatusNotSupported && st != aerogpu_test::kmt::kStatusInvalidParameter) {
    if (out_err) {
      *out_err =
          aerogpu_test::FormatString("D3DKMTEscape(query-device-v2) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }
    return false;
  }

  aerogpu_escape_query_device_out q1;
  ZeroMemory(&q1, sizeof(q1));
  q1.hdr.version = AEROGPU_ESCAPE_VERSION;
  q1.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
  q1.hdr.size = sizeof(q1);
  q1.hdr.reserved0 = 0;

  st = 0;
  if (!aerogpu_test::kmt::AerogpuEscapeWithTimeout(kmt, adapter, &q1, sizeof(q1), 2000, &st)) {
    if (out_err) {
      *out_err =
          aerogpu_test::FormatString("D3DKMTEscape(query-device) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
    }
    return false;
  }

  if (q1.mmio_version == 0) {
    if (out_err) {
      *out_err = "QUERY_DEVICE returned mmio_version==0";
    }
    return false;
  }
  if (q1.hdr.version != AEROGPU_ESCAPE_VERSION || q1.hdr.op != AEROGPU_ESCAPE_OP_QUERY_DEVICE || q1.hdr.size != sizeof(q1)) {
    if (out_err) {
      *out_err = aerogpu_test::FormatString("invalid QUERY_DEVICE header (version=%lu op=%lu size=%lu)",
                                            (unsigned long)q1.hdr.version,
                                            (unsigned long)q1.hdr.op,
                                            (unsigned long)q1.hdr.size);
    }
    return false;
  }
  return true;
}

static bool ProbeGetSegmentGroupSizeType(const D3DKMT_FUNCS* kmt,
                                         D3DKMT_HANDLE adapter,
                                         UINT* out_type,
                                         SegmentGroupSize* out_sizes,
                                         NTSTATUS* out_last_status) {
  if (out_type) {
    *out_type = 0xFFFFFFFFu;
  }
  if (out_sizes) {
    ZeroMemory(out_sizes, sizeof(*out_sizes));
  }
  if (out_last_status) {
    *out_last_status = 0;
  }

  if (!kmt || !kmt->QueryAdapterInfo || !adapter) {
    if (out_last_status) {
      *out_last_status = aerogpu_test::kmt::kStatusInvalidParameter;
    }
    return false;
  }

  // Avoid hard-coding the WDK's numeric KMTQAITYPE_GETSEGMENTGROUPSIZE constant; probe a small
  // range of values and look for a plausible 2xU64 layout.
  SegmentGroupSize sizes;
  NTSTATUS last_status = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&sizes, sizeof(sizes));

    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::D3DKMTQueryAdapterInfoWithTimeout(
            kmt, adapter, type, &sizes, (UINT)sizeof(sizes), 2000, &st)) {
      last_status = st;
      if (st == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
        break;
      }
      continue;
    }
    last_status = st;

    const uint64_t local = sizes.LocalMemorySize;
    const uint64_t nonlocal = sizes.NonLocalMemorySize;
    const uint64_t sum = local + nonlocal;

    if (sum == 0) {
      continue;
    }
    // Heuristic: segment sizes are typically multiples of MiB and not enormous.
    if ((local % kOneMiB) != 0 || (nonlocal % kOneMiB) != 0) {
      continue;
    }
    // Avoid mis-identifying unrelated query types with small integer payloads.
    if (sum < 16ull * kOneMiB) {
      continue;
    }
    // Guard against insane values (e.g. treating a pointer as a size).
    if (local > (1ull << 50) || nonlocal > (1ull << 50)) {
      continue;
    }

    if (out_type) {
      *out_type = type;
    }
    if (out_sizes) {
      *out_sizes = sizes;
    }
    if (out_last_status) {
      *out_last_status = st;
    }
    return true;
  }

  if (out_last_status) {
    *out_last_status = last_status;
  }
  return false;
}

static bool ProbeQuerySegmentType(const D3DKMT_FUNCS* kmt,
                                  D3DKMT_HANDLE adapter,
                                  UINT* out_type,
                                  QuerySegmentParsed* out_parsed,
                                  NTSTATUS* out_last_status) {
  if (out_type) {
    *out_type = 0xFFFFFFFFu;
  }
  if (out_parsed) {
    *out_parsed = QuerySegmentParsed();
  }
  if (out_last_status) {
    *out_last_status = 0;
  }

  if (!kmt || !kmt->QueryAdapterInfo || !adapter) {
    if (out_last_status) {
      *out_last_status = aerogpu_test::kmt::kStatusInvalidParameter;
    }
    return false;
  }

  // Best-effort probe: avoid hard-coding KMTQAITYPE_QUERYSEGMENT.
  std::vector<unsigned char> buf;
  buf.resize(1024);

  NTSTATUS last_status = 0;
  for (UINT type = 0; type < 256; ++type) {
    memset(&buf[0], 0, buf.size());
    NTSTATUS st = 0;
    if (!aerogpu_test::kmt::D3DKMTQueryAdapterInfoWithTimeout(
            kmt, adapter, type, &buf[0], (UINT)buf.size(), 2000, &st)) {
      last_status = st;
      if (st == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
        break;
      }
      continue;
    }
    last_status = st;

    QuerySegmentParsed parsed;
    if (!TryParseQuerySegment(&buf[0], buf.size(), &parsed)) {
      continue;
    }

    // Heuristic: AeroGPU's QUERYSEGMENT reports a single segment + known paging buffer fields.
    if (parsed.nb_segments != 1) {
      continue;
    }
    if (parsed.paging_buffer_private_data_size != kExpectedPagingBufferPrivateDataSize) {
      continue;
    }
    if (parsed.paging_buffer_segment_id != kExpectedPagingBufferSegmentId) {
      continue;
    }
    if (parsed.seg0_size == 0 || (parsed.seg0_size % kOneMiB) != 0) {
      // We require a parsable segment0 size to treat this as a match.
      continue;
    }

    if (out_type) {
      *out_type = type;
    }
    if (out_parsed) {
      *out_parsed = parsed;
      out_parsed->present = true;
      out_parsed->type = type;
    }
    if (out_last_status) {
      *out_last_status = st;
    }
    return true;
  }

  if (out_last_status) {
    *out_last_status = last_status;
  }
  return false;
}

static int RunSegmentBudgetSanity(int argc, char** argv) {
  const char* kTestName = "segment_budget_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--json[=PATH]] [--allow-remote] [--strict-default] [--min-nonlocal-mb=N]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Queries WDDM segment budget via D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE) and validates that the non-local "
        "segment size is sane. Also logs best-effort QUERYSEGMENT details (segment descriptor + paging buffer fields) "
        "when available.\n"
        "For AeroGPU, this budget is controlled by the registry value HKR\\Parameters\\NonLocalMemorySizeMB "
        "(default 512; clamped 128..1024 on x86, 128..2048 on x64). When the AeroGPU device registry key can be "
        "located, the test also reads NonLocalMemorySizeMB and verifies the KMD-reported budget matches it.");
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");
  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
  }

  uint32_t min_nonlocal_mb = 128;
  const bool strict_default = aerogpu_test::HasArg(argc, argv, "--strict-default");
  if (strict_default) {
    min_nonlocal_mb = 512;
  }
  std::string min_mb_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--min-nonlocal-mb", &min_mb_str)) {
    std::string err;
    uint32_t v = 0;
    if (!aerogpu_test::ParseUint32(min_mb_str, &v, &err)) {
      return reporter.Fail("invalid --min-nonlocal-mb: %s", err.c_str());
    }
    if (v < 128) {
      return reporter.Fail("--min-nonlocal-mb must be >= 128 (got %lu)", (unsigned long)v);
    }
    min_nonlocal_mb = v;
  }

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return reporter.Fail("%s", kmt_err.c_str());
  }
  if (!kmt.QueryAdapterInfo) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("D3DKMTQueryAdapterInfo not available (missing gdi32 export)");
  }

  D3DKMT_HANDLE adapter = 0;
  std::string open_err;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &open_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", open_err.c_str());
  }

  // Avoid false PASS when AeroGPU isn't the active adapter: confirm we can talk to the AeroGPU KMD via escape.
  std::string verify_err;
  if (!VerifyAeroGpuAdapterViaEscape(&kmt, adapter, &verify_err)) {
    aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    return reporter.Fail("%s", verify_err.c_str());
  }

  UINT seg_group_type = 0xFFFFFFFFu;
  SegmentGroupSize sizes;
  ZeroMemory(&sizes, sizeof(sizes));
  NTSTATUS last_status_group = 0;

  const bool have_sizes = ProbeGetSegmentGroupSizeType(&kmt, adapter, &seg_group_type, &sizes, &last_status_group);

  UINT query_segment_type = 0xFFFFFFFFu;
  QuerySegmentParsed query_segment;
  NTSTATUS last_status_query = 0;
  const bool have_query_segment =
      ProbeQuerySegmentType(&kmt, adapter, &query_segment_type, &query_segment, &last_status_query);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!have_sizes || seg_group_type == 0xFFFFFFFFu) {
    if (last_status_group == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
      return reporter.Fail("D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE) timed out");
    }
    return reporter.Fail("failed to query GETSEGMENTGROUPSIZE (probe last NTSTATUS=0x%08lX)",
                         (unsigned long)last_status_group);
  }

  aerogpu_test::PrintfStdout(
      "INFO: %s: GETSEGMENTGROUPSIZE type=%lu local=%I64u MiB nonlocal=%I64u MiB (local=%I64u bytes nonlocal=%I64u bytes)",
      kTestName,
      (unsigned long)seg_group_type,
      (unsigned long long)(sizes.LocalMemorySize / kOneMiB),
      (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB),
      (unsigned long long)sizes.LocalMemorySize,
      (unsigned long long)sizes.NonLocalMemorySize);

  if (sizes.LocalMemorySize != 0) {
    aerogpu_test::PrintfStdout("WARN: %s: LocalMemorySize is non-zero (%I64u MiB). AeroGPU is expected to be system-memory-only (LocalMemorySize=0).",
                               kTestName,
                               (unsigned long long)(sizes.LocalMemorySize / kOneMiB));
  }

  if (have_query_segment && query_segment.present && query_segment_type != 0xFFFFFFFFu) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: QUERYSEGMENT type=%lu nbSegments=%lu pagingPrivSize=%lu pagingSegId=%lu "
        "seg0_base=0x%I64X seg0_size=%I64u MiB (flags=0x%08lX group=%lu)",
        kTestName,
        (unsigned long)query_segment_type,
        (unsigned long)query_segment.nb_segments,
        (unsigned long)query_segment.paging_buffer_private_data_size,
        (unsigned long)query_segment.paging_buffer_segment_id,
        (unsigned long long)query_segment.seg0_base,
        (unsigned long long)(query_segment.seg0_size / kOneMiB),
        (unsigned long)query_segment.seg0_flags_value,
        (unsigned long)query_segment.seg0_group);

    if (query_segment.seg0_size != 0 && query_segment.seg0_size != sizes.NonLocalMemorySize) {
      aerogpu_test::PrintfStdout(
          "WARN: %s: QUERYSEGMENT segment0 size (%I64u MiB) does not match GETSEGMENTGROUPSIZE NonLocalMemorySize (%I64u MiB). "
          "This may indicate inconsistent budget reporting.",
          kTestName,
          (unsigned long long)(query_segment.seg0_size / kOneMiB),
          (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB));
    }
  } else if (last_status_query == (NTSTATUS)0xC0000102L /* STATUS_TIMEOUT */) {
    aerogpu_test::PrintfStdout("INFO: %s: QUERYSEGMENT probe timed out; skipping", kTestName);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: QUERYSEGMENT not available (probe last NTSTATUS=0x%08lX); skipping",
                               kTestName,
                               (unsigned long)last_status_query);
  }

  // Registry override cross-check (best-effort).
  //
  // If we can locate the AeroGPU display adapter by HWID and read HKR\Parameters\NonLocalMemorySizeMB, verify the
  // reported segment budget matches the clamped registry value. This directly validates that registry overrides take
  // effect after reboot/device restart.
  uint32_t reg_mb = 0;
  std::string reg_err;
  if (ReadAeroGpuNonLocalMemorySizeMbFromRegistry(&reg_mb, &reg_err)) {
    const uint32_t reg_mb_clamped = ClampNonLocalMbForOs(reg_mb);
    const uint64_t expected_bytes = (uint64_t)reg_mb_clamped * kOneMiB;
    aerogpu_test::PrintfStdout(
        "INFO: %s: registry NonLocalMemorySizeMB=%lu (clamped=%lu) => expected=%I64u MiB",
        kTestName,
        (unsigned long)reg_mb,
        (unsigned long)reg_mb_clamped,
        (unsigned long long)(expected_bytes / kOneMiB));

    if (sizes.NonLocalMemorySize != expected_bytes) {
      return reporter.Fail(
          "NonLocalMemorySize mismatch: GETSEGMENTGROUPSIZE reports %I64u MiB, but HKR\\\\Parameters\\\\NonLocalMemorySizeMB=%lu (clamped=%lu) implies %I64u MiB. "
          "Reboot the guest (or disable/enable the AeroGPU device) after changing the registry value.",
          (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB),
          (unsigned long)reg_mb,
          (unsigned long)reg_mb_clamped,
          (unsigned long long)(expected_bytes / kOneMiB));
    }
  } else if (!reg_err.empty()) {
    aerogpu_test::PrintfStdout("INFO: %s: registry NonLocalMemorySizeMB not available: %s", kTestName, reg_err.c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: registry NonLocalMemorySizeMB not available; skipping registry cross-check", kTestName);
  }

  if (sizes.NonLocalMemorySize == 0) {
    return reporter.Fail("NonLocalMemorySize==0 (expected a nonzero system-memory-backed segment budget)");
  }

  const uint64_t min_nonlocal_bytes = (uint64_t)min_nonlocal_mb * kOneMiB;
  if (sizes.NonLocalMemorySize < min_nonlocal_bytes) {
    return reporter.Fail("NonLocalMemorySize too small: %I64u MiB < %lu MiB (use HKR\\\\Parameters\\\\NonLocalMemorySizeMB)",
                         (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB),
                         (unsigned long)min_nonlocal_mb);
  }

  // Default budget is 512MiB. Values below that can be intentional, but often lead to allocation failures under
  // real workloads. Always warn so the user notices.
  if (sizes.NonLocalMemorySize < 512ull * kOneMiB) {
    aerogpu_test::PrintfStdout(
        "WARN: %s: NonLocalMemorySize is below the default 512 MiB (%I64u MiB). "
        "D3D9/D3D11 workloads may fail allocations. Set HKR\\\\Parameters\\\\NonLocalMemorySizeMB to increase it "
        "(or pass --strict-default/--min-nonlocal-mb to enforce a minimum).",
        kTestName,
        (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB));
    if (strict_default && min_nonlocal_mb == 512) {
      // This path should already be caught by the min_nonlocal_mb check above, but keep the logic explicit.
      return reporter.Fail("NonLocalMemorySize below 512 MiB and --strict-default was supplied");
    }
  }

  const uint64_t max_expected = ClampMaxNonLocalBytesForOs();
  if (sizes.NonLocalMemorySize > max_expected) {
    aerogpu_test::PrintfStdout(
        "INFO: %s: NonLocalMemorySize exceeds expected clamp for this OS (%s, max %I64u MiB): %I64u MiB. "
        "This may indicate the KMD clamp changed or is not being applied.",
        kTestName,
        IsOs64Bit() ? "x64" : "x86",
        (unsigned long long)(max_expected / kOneMiB),
        (unsigned long long)(sizes.NonLocalMemorySize / kOneMiB));
  }

  return reporter.Pass();
}

}  // namespace

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSegmentBudgetSanity(argc, argv);
}
