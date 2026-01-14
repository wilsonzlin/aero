#ifndef UNICODE
#define UNICODE
#endif

#ifndef _UNICODE
#define _UNICODE
#endif

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#include <errno.h>
#include <locale.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string>
#include <string.h>
#include <vector>
#include <wchar.h>

#include "aerogpu_pci.h"
#include "aerogpu_dbgctl_escape.h"
#include "aerogpu_cmd.h"
#include "aerogpu_feature_decode.h"
#include "aerogpu_umd_private.h"
#include "aerogpu_fence_watch_math.h"

typedef LONG NTSTATUS;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#endif

#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif

#ifndef STATUS_TIMEOUT
#define STATUS_TIMEOUT ((NTSTATUS)0xC0000102L)
#endif

#ifndef STATUS_INSUFFICIENT_RESOURCES
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#endif

#ifndef STATUS_BUFFER_TOO_SMALL
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)
#endif

#ifndef STATUS_ACCESS_DENIED
#define STATUS_ACCESS_DENIED ((NTSTATUS)0xC0000022L)
#endif

#ifndef STATUS_PARTIAL_COPY
// Warning status (still non-success for NT_SUCCESS).
#define STATUS_PARTIAL_COPY ((NTSTATUS)0x8000000DL)
#endif

typedef UINT D3DKMT_HANDLE;

static const uint32_t kAerogpuIrqFence = (1u << 0);
static const uint32_t kAerogpuIrqScanoutVblank = (1u << 1);
static const uint32_t kAerogpuIrqError = (1u << 31);

static bool g_json_output = false;
static bool g_json_pretty = false;
static const wchar_t *g_json_path = NULL;

static std::string WideToUtf8(const wchar_t *s) {
  if (!s) {
    return std::string();
  }
  const int bytes =
      WideCharToMultiByte(CP_UTF8, 0, s, -1, NULL, 0, NULL, NULL);
  if (bytes <= 0) {
    return std::string();
  }
  std::string out;
  out.resize((size_t)bytes);
  WideCharToMultiByte(CP_UTF8, 0, s, -1, &out[0], bytes, NULL, NULL);
  if (!out.empty() && out[out.size() - 1] == '\0') {
    out.resize(out.size() - 1);
  }
  return out;
}

static std::string WideToUtf8(const std::wstring &s) {
  return WideToUtf8(s.c_str());
}

static std::string HexU32(uint32_t v) {
  char buf[32];
  sprintf_s(buf, sizeof(buf), "0x%08lx", (unsigned long)v);
  return std::string(buf);
}

static std::string HexU64(uint64_t v) {
  char buf[32];
  sprintf_s(buf, sizeof(buf), "0x%016I64x", (unsigned long long)v);
  return std::string(buf);
}

static std::string DecU64(uint64_t v) {
  char buf[64];
  sprintf_s(buf, sizeof(buf), "%I64u", (unsigned long long)v);
  return std::string(buf);
}

static std::string DecI64(int64_t v) {
  char buf[64];
  sprintf_s(buf, sizeof(buf), "%I64d", (long long)v);
  return std::string(buf);
}

static std::string BytesToHex(const void *data, size_t len, bool withPrefix = true) {
  const uint8_t *p = (const uint8_t *)data;
  std::string out;
  const size_t prefixLen = withPrefix ? 2 : 0;
  if (len > ((size_t)-1 - prefixLen) / 2) {
    // Overflow; return a best-effort prefix-only string.
    return withPrefix ? std::string("0x") : std::string();
  }
  out.reserve(prefixLen + len * 2);
  if (withPrefix) {
    out.push_back('0');
    out.push_back('x');
  }
  for (size_t i = 0; i < len; ++i) {
    char b[3];
    sprintf_s(b, sizeof(b), "%02x", (unsigned)p[i]);
    out.push_back(b[0]);
    out.push_back(b[1]);
  }
  return out;
}

static std::string Win32ErrorToString(DWORD win32) {
  wchar_t msg[512];
  DWORD chars =
      FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                     NULL, win32, 0, msg,
                     (DWORD)(sizeof(msg) / sizeof(msg[0])), NULL);
  if (chars == 0) {
    return std::string();
  }
  while (chars > 0 && (msg[chars - 1] == L'\r' || msg[chars - 1] == L'\n')) {
    msg[--chars] = 0;
  }
  return WideToUtf8(msg);
}

class JsonWriter {
public:
  explicit JsonWriter(std::string *out, bool pretty = g_json_pretty) : out_(out), pretty_(pretty) {}

  void BeginObject() {
    PrepareValue();
    out_->push_back('{');
    Ctx c;
    c.type = CTX_OBJECT;
    c.first = true;
    c.expecting_value = false;
    stack_.push_back(c);
  }

  void EndObject() {
    if (stack_.empty()) {
      return;
    }
    const Ctx c = stack_.back();
    stack_.pop_back();
    if (pretty_ && !c.first) {
      out_->push_back('\n');
      WriteIndent(stack_.size());
    }
    out_->push_back('}');
  }

  void BeginArray() {
    PrepareValue();
    out_->push_back('[');
    Ctx c;
    c.type = CTX_ARRAY;
    c.first = true;
    c.expecting_value = false;
    stack_.push_back(c);
  }

  void EndArray() {
    if (stack_.empty()) {
      return;
    }
    const Ctx c = stack_.back();
    stack_.pop_back();
    if (pretty_ && !c.first) {
      out_->push_back('\n');
      WriteIndent(stack_.size());
    }
    out_->push_back(']');
  }

  void Key(const char *k) {
    if (stack_.empty()) {
      return;
    }
    Ctx &c = stack_.back();
    if (c.type != CTX_OBJECT) {
      return;
    }
    if (c.expecting_value) {
      // Missing value for previous key; keep output valid by inserting null.
      Null();
    }
    if (!c.first) {
      out_->push_back(',');
    }
    c.first = false;
    if (pretty_) {
      out_->push_back('\n');
      WriteIndent(stack_.size());
    }
    WriteString(k);
    if (pretty_) {
      out_->append(": ");
    } else {
      out_->push_back(':');
    }
    c.expecting_value = true;
  }

  void String(const char *s) {
    PrepareValue();
    WriteString(s ? s : "");
  }

  void String(const std::string &s) { String(s.c_str()); }

  void Bool(bool v) {
    PrepareValue();
    if (v) {
      out_->append("true");
    } else {
      out_->append("false");
    }
  }

  void Null() {
    PrepareValue();
    out_->append("null");
  }

  void Uint32(uint32_t v) {
    PrepareValue();
    char buf[32];
    sprintf_s(buf, sizeof(buf), "%lu", (unsigned long)v);
    out_->append(buf);
  }

  void Int32(int32_t v) {
    PrepareValue();
    char buf[32];
    sprintf_s(buf, sizeof(buf), "%ld", (long)v);
    out_->append(buf);
  }

  void Double(double v) {
    PrepareValue();
    // JSON numbers require '.' decimal separator regardless of process locale.
    static _locale_t c_locale = _create_locale(LC_NUMERIC, "C");
    char buf[64];
    _sprintf_s_l(buf, sizeof(buf), "%.6f", c_locale, v);
    out_->append(buf);
  }

private:
  enum CtxType { CTX_OBJECT = 0, CTX_ARRAY = 1 };
  struct Ctx {
    CtxType type;
    bool first;
    bool expecting_value;
  };

  void WriteIndent(size_t depth) {
    if (!out_) {
      return;
    }
    const size_t spaces = depth * 2;
    out_->append(spaces, ' ');
  }

  void PrepareValue() {
    if (!out_) {
      return;
    }
    if (stack_.empty()) {
      return;
    }
    Ctx &c = stack_.back();
    if (c.type == CTX_ARRAY) {
      if (!c.first) {
        out_->push_back(',');
      }
      if (pretty_) {
        out_->push_back('\n');
        WriteIndent(stack_.size());
      }
      c.first = false;
      return;
    }
    // Object: value must come after Key().
    if (c.type == CTX_OBJECT) {
      if (!c.expecting_value) {
        // Misuse; keep output valid by emitting an implicit keyless null.
        // (Should not happen in normal usage.)
      } else {
        c.expecting_value = false;
      }
    }
  }

  void WriteString(const char *s) {
    out_->push_back('"');
    for (const unsigned char *p = (const unsigned char *)s; p && *p; ++p) {
      const unsigned char c = *p;
      switch (c) {
      case '"':
        out_->append("\\\"");
        break;
      case '\\':
        out_->append("\\\\");
        break;
      case '\b':
        out_->append("\\b");
        break;
      case '\f':
        out_->append("\\f");
        break;
      case '\n':
        out_->append("\\n");
        break;
      case '\r':
        out_->append("\\r");
        break;
      case '\t':
        out_->append("\\t");
        break;
      default:
        if (c < 0x20) {
          char buf[8];
          sprintf_s(buf, sizeof(buf), "\\u%04x", (unsigned int)c);
          out_->append(buf);
        } else {
          out_->push_back((char)c);
        }
        break;
      }
    }
    out_->push_back('"');
  }

  std::string *out_;
  bool pretty_;
  std::vector<Ctx> stack_;
};

static const char *AerogpuFormatName(uint32_t fmt) {
  switch (fmt) {
  case AEROGPU_FORMAT_INVALID:
    return "Invalid";
  case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    return "B8G8R8A8Unorm";
  case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    return "B8G8R8X8Unorm";
  case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    return "R8G8B8A8Unorm";
  case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    return "R8G8B8X8Unorm";
  case AEROGPU_FORMAT_B5G6R5_UNORM:
    return "B5G6R5Unorm";
  case AEROGPU_FORMAT_B5G5R5A1_UNORM:
    return "B5G5R5A1Unorm";
  case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
    return "B8G8R8A8UnormSrgb";
  case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
    return "B8G8R8X8UnormSrgb";
  case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
    return "R8G8B8A8UnormSrgb";
  case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
    return "R8G8B8X8UnormSrgb";
  case AEROGPU_FORMAT_D24_UNORM_S8_UINT:
    return "D24UnormS8Uint";
  case AEROGPU_FORMAT_D32_FLOAT:
    return "D32Float";
  case AEROGPU_FORMAT_BC1_RGBA_UNORM:
    return "BC1RgbaUnorm";
  case AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB:
    return "BC1RgbaUnormSrgb";
  case AEROGPU_FORMAT_BC2_RGBA_UNORM:
    return "BC2RgbaUnorm";
  case AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB:
    return "BC2RgbaUnormSrgb";
  case AEROGPU_FORMAT_BC3_RGBA_UNORM:
    return "BC3RgbaUnorm";
  case AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB:
    return "BC3RgbaUnormSrgb";
  case AEROGPU_FORMAT_BC7_RGBA_UNORM:
    return "BC7RgbaUnorm";
  case AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB:
    return "BC7RgbaUnormSrgb";
  default:
    break;
  }

  // Avoid returning a pointer to a single static buffer; dbgctl may call this
  // helper multiple times in a single print statement.
  static __declspec(thread) char buf[4][32];
  static __declspec(thread) uint32_t buf_index = 0;
  char *out = buf[buf_index++ & 3u];
  sprintf_s(out, sizeof(buf[0]), "unknown(%lu)", (unsigned long)fmt);
  return out;
}

static const wchar_t *AerogpuErrorCodeName(uint32_t code) {
  switch (code) {
  case AEROGPU_ERROR_NONE:
    return L"NONE";
  case AEROGPU_ERROR_CMD_DECODE:
    return L"CMD_DECODE";
  case AEROGPU_ERROR_OOB:
    return L"OOB";
  case AEROGPU_ERROR_BACKEND:
    return L"BACKEND";
  case AEROGPU_ERROR_INTERNAL:
    return L"INTERNAL";
  default:
    break;
  }
  return L"UNKNOWN";
}

typedef struct D3DKMT_OPENADAPTERFROMHDC {
  HDC hDc;
  D3DKMT_HANDLE hAdapter;
  LUID AdapterLuid;
  UINT VidPnSourceId;
} D3DKMT_OPENADAPTERFROMHDC;

typedef struct D3DKMT_CLOSEADAPTER {
  D3DKMT_HANDLE hAdapter;
} D3DKMT_CLOSEADAPTER;

typedef struct D3DKMT_WAITFORVERTICALBLANKEVENT {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  UINT VidPnSourceId;
} D3DKMT_WAITFORVERTICALBLANKEVENT;

typedef struct D3DKMT_GETSCANLINE {
  D3DKMT_HANDLE hAdapter;
  UINT VidPnSourceId;
  BOOL InVerticalBlank;
  UINT ScanLine;
} D3DKMT_GETSCANLINE;

typedef struct D3DKMT_QUERYADAPTERINFO {
  D3DKMT_HANDLE hAdapter;
  UINT Type; // KMTQUERYADAPTERINFOTYPE
  VOID *pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_QUERYADAPTERINFO;

// Minimal Win7-era WDDM segment query structs (from d3dkmddi/d3dkmthk).
// dbgctl intentionally avoids pulling in WDK headers; keep definitions local.
typedef struct DXGK_SEGMENTFLAGS {
  union {
    struct {
      UINT Aperture : 1;
      UINT CpuVisible : 1;
      UINT CacheCoherent : 1;
      UINT UseBanking : 1;
      UINT Reserved : 28;
    };
    UINT Value;
  };
} DXGK_SEGMENTFLAGS;

typedef enum DXGK_MEMORY_SEGMENT_GROUP {
  DXGK_MEMORY_SEGMENT_GROUP_LOCAL = 0,
  DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL = 1,
} DXGK_MEMORY_SEGMENT_GROUP;

typedef struct DXGK_SEGMENTDESCRIPTOR {
  LARGE_INTEGER BaseAddress; // PHYSICAL_ADDRESS
  ULONGLONG Size;
  DXGK_SEGMENTFLAGS Flags;
  UINT MemorySegmentGroup; // DXGK_MEMORY_SEGMENT_GROUP
} DXGK_SEGMENTDESCRIPTOR;

typedef struct DXGK_QUERYSEGMENTOUT {
  UINT NbSegments;
  UINT PagingBufferPrivateDataSize;
  UINT PagingBufferSegmentId;
  SIZE_T PagingBufferSize;
  DXGK_SEGMENTDESCRIPTOR pSegmentDescriptor[1]; // variable-length
} DXGK_QUERYSEGMENTOUT;

typedef struct DXGK_SEGMENTGROUPSIZE {
  ULONGLONG LocalMemorySize;
  ULONGLONG NonLocalMemorySize;
} DXGK_SEGMENTGROUPSIZE;

typedef enum D3DKMT_ESCAPETYPE {
  D3DKMT_ESCAPE_DRIVERPRIVATE = 0,
} D3DKMT_ESCAPETYPE;

typedef struct D3DKMT_ESCAPEFLAGS {
  union {
    struct {
      UINT HardwareAccess : 1;
      UINT Reserved : 31;
    };
    UINT Value;
  };
} D3DKMT_ESCAPEFLAGS;

typedef struct D3DKMT_ESCAPE {
  D3DKMT_HANDLE hAdapter;
  D3DKMT_HANDLE hDevice;
  D3DKMT_HANDLE hContext;
  D3DKMT_ESCAPETYPE Type;
  D3DKMT_ESCAPEFLAGS Flags;
  VOID *pPrivateDriverData;
  UINT PrivateDriverDataSize;
} D3DKMT_ESCAPE;

typedef NTSTATUS(WINAPI *PFND3DKMTOpenAdapterFromHdc)(D3DKMT_OPENADAPTERFROMHDC *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTCloseAdapter)(D3DKMT_CLOSEADAPTER *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTEscape)(D3DKMT_ESCAPE *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTWaitForVerticalBlankEvent)(D3DKMT_WAITFORVERTICALBLANKEVENT *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTGetScanLine)(D3DKMT_GETSCANLINE *pData);
typedef NTSTATUS(WINAPI *PFND3DKMTQueryAdapterInfo)(D3DKMT_QUERYADAPTERINFO *pData);
typedef ULONG(WINAPI *PFNRtlNtStatusToDosError)(NTSTATUS Status);

typedef struct D3DKMT_FUNCS {
  HMODULE gdi32;
  PFND3DKMTOpenAdapterFromHdc OpenAdapterFromHdc;
  PFND3DKMTCloseAdapter CloseAdapter;
  PFND3DKMTEscape Escape;
  PFND3DKMTWaitForVerticalBlankEvent WaitForVerticalBlankEvent;
  PFND3DKMTGetScanLine GetScanLine;
  PFND3DKMTQueryAdapterInfo QueryAdapterInfo;
  PFNRtlNtStatusToDosError RtlNtStatusToDosError;
} D3DKMT_FUNCS;

static uint32_t g_escape_timeout_ms = 0;
static volatile LONG g_skip_close_adapter = 0;

#ifndef AEROGPU_ESCAPE_OP_READ_GPA
// Expected to be provided by the KMD companion change. Keep a local fallback so this tool
// continues to build against older protocol headers.
#define AEROGPU_ESCAPE_OP_READ_GPA 13u
#endif
#pragma pack(push, 1)
typedef struct bmp_file_header {
  uint16_t bfType;      /* "BM" */
  uint32_t bfSize;      /* total file size */
  uint16_t bfReserved1; /* 0 */
  uint16_t bfReserved2; /* 0 */
  uint32_t bfOffBits;   /* offset to pixel data */
} bmp_file_header;

typedef struct bmp_info_header {
  uint32_t biSize;          /* 40 */
  int32_t biWidth;
  int32_t biHeight;         /* positive = bottom-up */
  uint16_t biPlanes;        /* 1 */
  uint16_t biBitCount;      /* 32 */
  uint32_t biCompression;   /* BI_RGB (0) */
  uint32_t biSizeImage;     /* raw image size (may be 0 for BI_RGB but we fill it) */
  int32_t biXPelsPerMeter;
  int32_t biYPelsPerMeter;
  uint32_t biClrUsed;
  uint32_t biClrImportant;
} bmp_info_header;
#pragma pack(pop)

static bool MulU64(uint64_t a, uint64_t b, uint64_t *out) {
  if (!out) {
    return false;
  }
  if (a == 0 || b == 0) {
    *out = 0;
    return true;
  }
  const uint64_t kU64Max = ~(uint64_t)0;
  if (a > (kU64Max / b)) {
    return false;
  }
  *out = a * b;
  return true;
}

static bool AddU64(uint64_t a, uint64_t b, uint64_t *out) {
  if (!out) {
    return false;
  }
  const uint64_t kU64Max = ~(uint64_t)0;
  if (a > (kU64Max - b)) {
    return false;
  }
  *out = a + b;
  return true;
}

static const uint32_t kPngCrc32Table[256] = {
    0x00000000u, 0x77073096u, 0xee0e612cu, 0x990951bau, 0x076dc419u, 0x706af48fu, 0xe963a535u, 0x9e6495a3u,
    0x0edb8832u, 0x79dcb8a4u, 0xe0d5e91eu, 0x97d2d988u, 0x09b64c2bu, 0x7eb17cbdu, 0xe7b82d07u, 0x90bf1d91u,
    0x1db71064u, 0x6ab020f2u, 0xf3b97148u, 0x84be41deu, 0x1adad47du, 0x6ddde4ebu, 0xf4d4b551u, 0x83d385c7u,
    0x136c9856u, 0x646ba8c0u, 0xfd62f97au, 0x8a65c9ecu, 0x14015c4fu, 0x63066cd9u, 0xfa0f3d63u, 0x8d080df5u,
    0x3b6e20c8u, 0x4c69105eu, 0xd56041e4u, 0xa2677172u, 0x3c03e4d1u, 0x4b04d447u, 0xd20d85fdu, 0xa50ab56bu,
    0x35b5a8fau, 0x42b2986cu, 0xdbbbc9d6u, 0xacbcf940u, 0x32d86ce3u, 0x45df5c75u, 0xdcd60dcfu, 0xabd13d59u,
    0x26d930acu, 0x51de003au, 0xc8d75180u, 0xbfd06116u, 0x21b4f4b5u, 0x56b3c423u, 0xcfba9599u, 0xb8bda50fu,
    0x2802b89eu, 0x5f058808u, 0xc60cd9b2u, 0xb10be924u, 0x2f6f7c87u, 0x58684c11u, 0xc1611dabu, 0xb6662d3du,
    0x76dc4190u, 0x01db7106u, 0x98d220bcu, 0xefd5102au, 0x71b18589u, 0x06b6b51fu, 0x9fbfe4a5u, 0xe8b8d433u,
    0x7807c9a2u, 0x0f00f934u, 0x9609a88eu, 0xe10e9818u, 0x7f6a0dbbu, 0x086d3d2du, 0x91646c97u, 0xe6635c01u,
    0x6b6b51f4u, 0x1c6c6162u, 0x856530d8u, 0xf262004eu, 0x6c0695edu, 0x1b01a57bu, 0x8208f4c1u, 0xf50fc457u,
    0x65b0d9c6u, 0x12b7e950u, 0x8bbeb8eau, 0xfcb9887cu, 0x62dd1ddfu, 0x15da2d49u, 0x8cd37cf3u, 0xfbd44c65u,
    0x4db26158u, 0x3ab551ceu, 0xa3bc0074u, 0xd4bb30e2u, 0x4adfa541u, 0x3dd895d7u, 0xa4d1c46du, 0xd3d6f4fbu,
    0x4369e96au, 0x346ed9fcu, 0xad678846u, 0xda60b8d0u, 0x44042d73u, 0x33031de5u, 0xaa0a4c5fu, 0xdd0d7cc9u,
    0x5005713cu, 0x270241aau, 0xbe0b1010u, 0xc90c2086u, 0x5768b525u, 0x206f85b3u, 0xb966d409u, 0xce61e49fu,
    0x5edef90eu, 0x29d9c998u, 0xb0d09822u, 0xc7d7a8b4u, 0x59b33d17u, 0x2eb40d81u, 0xb7bd5c3bu, 0xc0ba6cadu,
    0xedb88320u, 0x9abfb3b6u, 0x03b6e20cu, 0x74b1d29au, 0xead54739u, 0x9dd277afu, 0x04db2615u, 0x73dc1683u,
    0xe3630b12u, 0x94643b84u, 0x0d6d6a3eu, 0x7a6a5aa8u, 0xe40ecf0bu, 0x9309ff9du, 0x0a00ae27u, 0x7d079eb1u,
    0xf00f9344u, 0x8708a3d2u, 0x1e01f268u, 0x6906c2feu, 0xf762575du, 0x806567cbu, 0x196c3671u, 0x6e6b06e7u,
    0xfed41b76u, 0x89d32be0u, 0x10da7a5au, 0x67dd4accu, 0xf9b9df6fu, 0x8ebeeff9u, 0x17b7be43u, 0x60b08ed5u,
    0xd6d6a3e8u, 0xa1d1937eu, 0x38d8c2c4u, 0x4fdff252u, 0xd1bb67f1u, 0xa6bc5767u, 0x3fb506ddu, 0x48b2364bu,
    0xd80d2bdau, 0xaf0a1b4cu, 0x36034af6u, 0x41047a60u, 0xdf60efc3u, 0xa867df55u, 0x316e8eefu, 0x4669be79u,
    0xcb61b38cu, 0xbc66831au, 0x256fd2a0u, 0x5268e236u, 0xcc0c7795u, 0xbb0b4703u, 0x220216b9u, 0x5505262fu,
    0xc5ba3bbeu, 0xb2bd0b28u, 0x2bb45a92u, 0x5cb36a04u, 0xc2d7ffa7u, 0xb5d0cf31u, 0x2cd99e8bu, 0x5bdeae1du,
    0x9b64c2b0u, 0xec63f226u, 0x756aa39cu, 0x026d930au, 0x9c0906a9u, 0xeb0e363fu, 0x72076785u, 0x05005713u,
    0x95bf4a82u, 0xe2b87a14u, 0x7bb12baeu, 0x0cb61b38u, 0x92d28e9bu, 0xe5d5be0du, 0x7cdcefb7u, 0x0bdbdf21u,
    0x86d3d2d4u, 0xf1d4e242u, 0x68ddb3f8u, 0x1fda836eu, 0x81be16cdu, 0xf6b9265bu, 0x6fb077e1u, 0x18b74777u,
    0x88085ae6u, 0xff0f6a70u, 0x66063bcau, 0x11010b5cu, 0x8f659effu, 0xf862ae69u, 0x616bffd3u, 0x166ccf45u,
    0xa00ae278u, 0xd70dd2eeu, 0x4e048354u, 0x3903b3c2u, 0xa7672661u, 0xd06016f7u, 0x4969474du, 0x3e6e77dbu,
    0xaed16a4au, 0xd9d65adcu, 0x40df0b66u, 0x37d83bf0u, 0xa9bcae53u, 0xdebb9ec5u, 0x47b2cf7fu, 0x30b5ffe9u,
    0xbdbdf21cu, 0xcabac28au, 0x53b39330u, 0x24b4a3a6u, 0xbad03605u, 0xcdd70693u, 0x54de5729u, 0x23d967bfu,
    0xb3667a2eu, 0xc4614ab8u, 0x5d681b02u, 0x2a6f2b94u, 0xb40bbe37u, 0xc30c8ea1u, 0x5a05df1bu, 0x2d02ef8du,
};

static uint32_t PngCrc32Update(uint32_t crc, const void *data, size_t len) {
  const uint8_t *p = (const uint8_t *)data;
  for (size_t i = 0; i < len; ++i) {
    crc = kPngCrc32Table[(crc ^ (uint32_t)p[i]) & 0xFFu] ^ (crc >> 8);
  }
  return crc;
}

static uint32_t PngAdler32Update(uint32_t adler, const void *data, size_t len) {
  // zlib Adler32 (RFC 1950).
  static const uint32_t kBase = 65521u;
  static const size_t kNmax = 5552u;

  uint32_t s1 = adler & 0xFFFFu;
  uint32_t s2 = (adler >> 16) & 0xFFFFu;

  const uint8_t *buf = (const uint8_t *)data;
  while (len != 0) {
    size_t k = (len < kNmax) ? len : kNmax;
    len -= k;
    while (k--) {
      s1 += *buf++;
      s2 += s1;
    }
    s1 %= kBase;
    s2 %= kBase;
  }

  return (s2 << 16) | s1;
}

static bool WriteU32Be(FILE *fp, uint32_t v) {
  uint8_t b[4];
  b[0] = (uint8_t)((v >> 24) & 0xFFu);
  b[1] = (uint8_t)((v >> 16) & 0xFFu);
  b[2] = (uint8_t)((v >> 8) & 0xFFu);
  b[3] = (uint8_t)(v & 0xFFu);
  return fwrite(b, 1, sizeof(b), fp) == sizeof(b);
}

static bool WritePngChunk(FILE *fp, const char type[4], const void *data, uint32_t len) {
  if (!fp || !type) {
    return false;
  }
  if (!WriteU32Be(fp, len)) {
    return false;
  }
  if (fwrite(type, 1, 4, fp) != 4) {
    return false;
  }
  if (len != 0) {
    if (!data) {
      return false;
    }
    if (fwrite(data, 1, len, fp) != len) {
      return false;
    }
  }

  uint32_t crc = 0xFFFFFFFFu;
  crc = PngCrc32Update(crc, type, 4);
  if (len != 0) {
    crc = PngCrc32Update(crc, data, (size_t)len);
  }
  crc ^= 0xFFFFFFFFu;
  return WriteU32Be(fp, crc);
}

static bool WritePngChunkHeader(FILE *fp, const char type[4], uint32_t len, uint32_t *crcOut) {
  if (!fp || !type || !crcOut) {
    return false;
  }
  if (!WriteU32Be(fp, len)) {
    return false;
  }
  if (fwrite(type, 1, 4, fp) != 4) {
    return false;
  }
  *crcOut = PngCrc32Update(0xFFFFFFFFu, type, 4);
  return true;
}

static bool WritePngChunkCrc(FILE *fp, uint32_t crc) {
  if (!fp) {
    return false;
  }
  crc ^= 0xFFFFFFFFu;
  return WriteU32Be(fp, crc);
}

static const uint64_t kDumpLastCmdDefaultMaxBytes = 1024ull * 1024ull;      // 1 MiB
static const uint64_t kDumpLastCmdHardMaxBytes = 64ull * 1024ull * 1024ull; // 64 MiB

static void PrintUsage() {
  fwprintf(stderr,
           L"Usage:\n"
           L"  aerogpu_dbgctl [--display \\\\.\\DISPLAY1] [--ring-id N] [--timeout-ms N] [--json[=PATH]] [--pretty]\n"
           L"               [--vblank-samples N] [--vblank-interval-ms N]\n"
           L"               [--samples N] [--interval-ms N]\n"
           L"               [--size N] [--out FILE] [--cmd-out FILE] [--alloc-out FILE] [--count N] [--force]\n"
           L"               <command>\n"
           L"\n"
           L"Global output options:\n"
           L"  --json[=PATH]  Output machine-readable JSON (schema_version=1). If PATH is provided, write JSON there.\n"
           L"  --pretty       Pretty-print JSON (implies --json).\n"
           L"\n"
           L"Commands:\n"
           L"  --list-displays\n"
           L"  --status  (alias: --query-version)\n"
           L"  --query-version  (alias: --query-device)\n"
           L"  --query-umd-private\n"
           L"  --query-segments\n"
           L"  --query-fence\n"
           L"  --watch-fence  (requires: --samples N --interval-ms M)\n"
           L"  --query-perf  (alias: --perf)\n"
           L"  --query-scanout\n"
           L"  --dump-scanout-bmp PATH\n"
           L"  --dump-scanout-png PATH\n"
           L"  --query-cursor  (alias: --dump-cursor)\n"
           L"  --dump-cursor-bmp PATH\n"
           L"  --dump-cursor-png PATH\n"
           L"  --dump-ring\n"
           L"  --dump-last-submit (alias: --dump-last-cmd) [--index-from-tail K] [--count N]\n"
           L"      --cmd-out <path> [--alloc-out <path>] [--force]\n"
           L"  --watch-ring  (requires: --samples N --interval-ms M)\n"
           L"  --dump-createalloc  (DxgkDdiCreateAllocation trace)\n"
           L"      [--csv <path>]  (write CreateAllocation trace as CSV)\n"
           L"  --dump-vblank  (alias: --query-vblank)\n"
           L"  --wait-vblank  (D3DKMTWaitForVerticalBlankEvent)\n"
           L"  --query-scanline  (D3DKMTGetScanLine)\n"
           L"  --map-shared-handle HANDLE\n"
           L"  --read-gpa GPA --size N [--out FILE] [--force]\n"
           L"  --read-gpa GPA N [--out FILE] [--force]\n"
           L"  --selftest\n");
}

static void PrintNtStatus(const wchar_t *prefix, const D3DKMT_FUNCS *f, NTSTATUS st) {
  DWORD win32 = 0;
  if (f->RtlNtStatusToDosError) {
    win32 = f->RtlNtStatusToDosError(st);
  }

  if (win32 != 0) {
    wchar_t msg[512];
    DWORD chars = FormatMessageW(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, NULL, win32, 0,
                                 msg, (DWORD)(sizeof(msg) / sizeof(msg[0])), NULL);
    if (chars != 0) {
      while (chars > 0 && (msg[chars - 1] == L'\r' || msg[chars - 1] == L'\n')) {
        msg[--chars] = 0;
      }
      fwprintf(stderr, L"%s: NTSTATUS=0x%08lx (Win32=%lu: %s)\n", prefix, (unsigned long)st,
               (unsigned long)win32, msg);
      return;
    }
  }

  fwprintf(stderr, L"%s: NTSTATUS=0x%08lx\n", prefix, (unsigned long)st);
}

static DWORD NtStatusToWin32(const D3DKMT_FUNCS *f, NTSTATUS st) {
  if (!f || !f->RtlNtStatusToDosError) {
    return 0;
  }
  return f->RtlNtStatusToDosError(st);
}

static void JsonWriteNtStatusError(JsonWriter &w, const D3DKMT_FUNCS *f, NTSTATUS st) {
  w.BeginObject();
  w.Key("ntstatus");
  w.String(HexU32((uint32_t)st));
  const DWORD win32 = NtStatusToWin32(f, st);
  if (win32 != 0) {
    w.Key("win32");
    w.Uint32((uint32_t)win32);
    w.Key("win32_hex");
    w.String(HexU32((uint32_t)win32));
    const std::string msg = Win32ErrorToString(win32);
    if (!msg.empty()) {
      w.Key("win32_message");
      w.String(msg);
    }
  }
  w.EndObject();
}

static void JsonWriteTopLevelError(std::string *out, const char *command, const D3DKMT_FUNCS *f, const char *message,
                                  NTSTATUS st) {
  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String(command ? command : "");
  w.Key("ok");
  w.Bool(false);
  w.Key("error");
  w.BeginObject();
  w.Key("message");
  w.String(message ? message : "");
  w.Key("status");
  JsonWriteNtStatusError(w, f, st);
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
}

static bool WriteStringToFileUtf8(const wchar_t *path, const std::string &data) {
  if (!path) {
    return false;
  }
  FILE *fp = _wfopen(path, L"wb");
  if (!fp) {
    return false;
  }
  const size_t n = fwrite(data.data(), 1, data.size(), fp);
  fclose(fp);
  return n == data.size();
}

static int WriteJsonToDestination(const std::string &json) {
  if (g_json_path) {
    if (WriteStringToFileUtf8(g_json_path, json)) {
      return 0;
    }
    const int err = errno;
    fwprintf(stderr, L"Failed to write JSON to %s (errno=%d)\n", g_json_path, err);
    // Best-effort fallback to stdout so the caller still gets a parseable payload.
    fwrite(json.data(), 1, json.size(), stdout);
    return 2;
  }

  fwrite(json.data(), 1, json.size(), stdout);
  return 0;
}

static void HexDumpBytes(const void *data, uint32_t len, uint64_t base) {
  const uint8_t *p = (const uint8_t *)data;
  const uint32_t kBytesPerLine = 16;

  for (uint32_t i = 0; i < len; i += kBytesPerLine) {
    const uint32_t lineLen = (len - i < kBytesPerLine) ? (len - i) : kBytesPerLine;
    wprintf(L"%016I64x: ", (unsigned long long)(base + (uint64_t)i));
    for (uint32_t j = 0; j < kBytesPerLine; ++j) {
      if (j < lineLen) {
        wprintf(L"%02x ", (unsigned)p[i + j]);
      } else {
        wprintf(L"   ");
      }
    }
    wprintf(L"|");
    for (uint32_t j = 0; j < lineLen; ++j) {
      const uint8_t c = p[i + j];
      const wchar_t wc = (c >= 32 && c <= 126) ? (wchar_t)c : L'.';
      wprintf(L"%c", wc);
    }
    wprintf(L"|\n");
  }
}

static void BestEffortDeleteOutputFile(const wchar_t *path) {
  if (!path || !path[0]) {
    return;
  }
  if (DeleteFileW(path)) {
    return;
  }
  const DWORD err = GetLastError();
  if (err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND) {
    return;
  }
  // If the file is read-only, try clearing the attribute and deleting again.
  const DWORD attrs = GetFileAttributesW(path);
  if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_READONLY) != 0) {
    SetFileAttributesW(path, attrs & ~FILE_ATTRIBUTE_READONLY);
    DeleteFileW(path);
  }
}

static bool WriteBinaryFile(const wchar_t *path, const void *data, uint32_t len) {
  if (!path) {
    return false;
  }

  HANDLE h =
      CreateFileW(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    fwprintf(stderr, L"Failed to open output file %s (GetLastError=%lu)\n", path, (unsigned long)GetLastError());
    return false;
  }

  DWORD written = 0;
  const BOOL ok = WriteFile(h, data, (DWORD)len, &written, NULL);
  const DWORD lastErr = GetLastError();
  CloseHandle(h);

  if (!ok || written != len) {
    fwprintf(stderr,
             L"Failed to write output file %s (written=%lu/%lu, GetLastError=%lu)\n",
             path,
             (unsigned long)written,
             (unsigned long)len,
             (unsigned long)lastErr);
    BestEffortDeleteOutputFile(path);
    return false;
  }

  return true;
}

static bool LoadD3DKMT(D3DKMT_FUNCS *out) {
  ZeroMemory(out, sizeof(*out));
  out->gdi32 = LoadLibraryW(L"gdi32.dll");
  if (!out->gdi32) {
    fwprintf(stderr, L"Failed to load gdi32.dll\n");
    return false;
  }

  out->OpenAdapterFromHdc =
      (PFND3DKMTOpenAdapterFromHdc)GetProcAddress(out->gdi32, "D3DKMTOpenAdapterFromHdc");
  out->CloseAdapter = (PFND3DKMTCloseAdapter)GetProcAddress(out->gdi32, "D3DKMTCloseAdapter");
  out->Escape = (PFND3DKMTEscape)GetProcAddress(out->gdi32, "D3DKMTEscape");
  out->WaitForVerticalBlankEvent =
      (PFND3DKMTWaitForVerticalBlankEvent)GetProcAddress(out->gdi32, "D3DKMTWaitForVerticalBlankEvent");
  out->GetScanLine = (PFND3DKMTGetScanLine)GetProcAddress(out->gdi32, "D3DKMTGetScanLine");
  out->QueryAdapterInfo = (PFND3DKMTQueryAdapterInfo)GetProcAddress(out->gdi32, "D3DKMTQueryAdapterInfo");

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (ntdll) {
    out->RtlNtStatusToDosError = (PFNRtlNtStatusToDosError)GetProcAddress(ntdll, "RtlNtStatusToDosError");
  }

  if (!out->OpenAdapterFromHdc || !out->CloseAdapter || !out->Escape) {
    fwprintf(stderr,
             L"Required D3DKMT* exports not found in gdi32.dll.\n"
             L"This tool requires Windows Vista+ (WDDM).\n");
    return false;
  }

  return true;
}

static bool GetPrimaryDisplayName(wchar_t out[CCHDEVICENAME]) {
  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
  }

  wcsncpy(out, L"\\\\.\\DISPLAY1", CCHDEVICENAME - 1);
  out[CCHDEVICENAME - 1] = 0;
  return true;
}

static int ListDisplays() {
  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  wprintf(L"Display devices:\n");
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    const bool primary = (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
    const bool active = (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0;
    wprintf(L"  [%lu] %s%s%s\n",
            (unsigned long)i,
            dd.DeviceName,
            primary ? L" (primary)" : L"",
            active ? L" (active)" : L"");
    wprintf(L"       %s\n", dd.DeviceString);

    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  return 0;
}

static int ListDisplaysJson(std::string *out) {
  if (!out) {
    return 1;
  }
  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("list-displays");
  w.Key("ok");
  w.Bool(true);
  w.Key("displays");
  w.BeginArray();

  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(NULL, i, &dd, 0); ++i) {
    const bool primary = (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
    const bool active = (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0;

    w.BeginObject();
    w.Key("index");
    w.Uint32((uint32_t)i);
    w.Key("device_name");
    w.String(WideToUtf8(dd.DeviceName));
    w.Key("device_string");
    w.String(WideToUtf8(dd.DeviceString));
    w.Key("primary");
    w.Bool(primary);
    w.Key("active");
    w.Bool(active);
    w.EndObject();

    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  w.EndArray();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

typedef struct EscapeThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  UINT flags_value;
  void *buf;
  UINT bufSize;
  NTSTATUS status;
  HANDLE done_event;
} EscapeThreadCtx;

static DWORD WINAPI EscapeThreadProc(LPVOID param) {
  EscapeThreadCtx *ctx = (EscapeThreadCtx *)param;
  if (!ctx || !ctx->f || !ctx->f->Escape || !ctx->buf || ctx->bufSize == 0) {
    if (ctx) {
      ctx->status = STATUS_INVALID_PARAMETER;
    }
    return 0;
  }

  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = ctx->hAdapter;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = ctx->flags_value;
  e.pPrivateDriverData = ctx->buf;
  e.PrivateDriverDataSize = ctx->bufSize;
  ctx->status = ctx->f->Escape(&e);

  if (ctx->done_event) {
    SetEvent(ctx->done_event);
  }
  return 0;
}

static NTSTATUS SendAerogpuEscapeEx(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, void *buf, UINT bufSize,
                                    UINT flagsValue) {
  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = hAdapter;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = flagsValue;
  e.pPrivateDriverData = buf;
  e.PrivateDriverDataSize = bufSize;
  if (g_escape_timeout_ms == 0) {
    return f->Escape(&e);
  }

  // Like the vblank wait helper, run escapes on a worker thread so a buggy kernel driver cannot
  // hang the dbgctl process forever. If the call times out, leak the context (the thread may be
  // blocked inside the kernel thunk) and set a global so we avoid calling D3DKMTCloseAdapter.
  EscapeThreadCtx *ctx = (EscapeThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*ctx));
  if (!ctx) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  void *bufCopy = HeapAlloc(GetProcessHeap(), 0, bufSize);
  if (!bufCopy) {
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  memcpy(bufCopy, buf, bufSize);

  ctx->f = f;
  ctx->hAdapter = hAdapter;
  ctx->flags_value = flagsValue;
  ctx->buf = bufCopy;
  ctx->bufSize = bufSize;
  ctx->status = 0;
  ctx->done_event = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (!ctx->done_event) {
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  HANDLE thread = CreateThread(NULL, 0, EscapeThreadProc, ctx, 0, NULL);
  if (!thread) {
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  DWORD w = WaitForSingleObject(ctx->done_event, g_escape_timeout_ms);
  if (w == WAIT_OBJECT_0) {
    // Thread completed; safe to copy results back and clean up.
    const NTSTATUS st = ctx->status;
    if (NT_SUCCESS(st)) {
      memcpy(buf, ctx->buf, bufSize);
    }
    CloseHandle(thread);
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, ctx->buf);
    HeapFree(GetProcessHeap(), 0, ctx);
    return st;
  }

  // Timeout or failure; avoid deadlock-prone cleanup.
  CloseHandle(thread);
  InterlockedExchange(&g_skip_close_adapter, 1);
  return (w == WAIT_TIMEOUT) ? STATUS_TIMEOUT : STATUS_INVALID_PARAMETER;
}

static NTSTATUS SendAerogpuEscapeDirect(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, void *buf, UINT bufSize) {
  if (!f || !f->Escape || !hAdapter || !buf || bufSize == 0) {
    return STATUS_INVALID_PARAMETER;
  }
  D3DKMT_ESCAPE e;
  ZeroMemory(&e, sizeof(e));
  e.hAdapter = hAdapter;
  e.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  e.Flags.Value = 0;
  e.pPrivateDriverData = buf;
  e.PrivateDriverDataSize = bufSize;
  return f->Escape(&e);
}

static NTSTATUS SendAerogpuEscape(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, void *buf, UINT bufSize) {
  return SendAerogpuEscapeEx(f, hAdapter, buf, bufSize, 0);
}

static uint32_t MinU32(uint32_t a, uint32_t b) { return (a < b) ? a : b; }

static bool CreateEmptyFile(const wchar_t *path) {
  if (!path || path[0] == 0) {
    return false;
  }
  FILE *fp = _wfopen(path, L"wb");
  if (!fp) {
    fwprintf(stderr, L"Failed to open output file: %s (errno=%d)\n", path, errno);
    return false;
  }
  fclose(fp);
  return true;
}

static bool DumpGpaToFile(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint64_t gpa, uint32_t sizeBytes,
                          const wchar_t *path) {
  if (!path || path[0] == 0) {
    return false;
  }
  if (sizeBytes == 0) {
    return CreateEmptyFile(path);
  }

  FILE *fp = _wfopen(path, L"wb");
  if (!fp) {
    fwprintf(stderr, L"Failed to open output file: %s (errno=%d)\n", path, errno);
    return false;
  }

  bool ok = false;
  uint32_t done = 0;
  while (done < sizeBytes) {
    const uint32_t chunk = MinU32(sizeBytes - done, (uint32_t)AEROGPU_DBGCTL_READ_GPA_MAX_BYTES);
    const uint64_t cur = gpa + (uint64_t)done;
    if (cur < gpa) {
      fwprintf(stderr, L"dump-gpa: address overflow\n");
      goto cleanup;
    }

    aerogpu_escape_read_gpa_inout io;
    ZeroMemory(&io, sizeof(io));
    io.hdr.version = AEROGPU_ESCAPE_VERSION;
    io.hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
    io.hdr.size = sizeof(io);
    io.hdr.reserved0 = 0;
    io.gpa = (aerogpu_escape_u64)cur;
    io.size_bytes = (aerogpu_escape_u32)chunk;
    io.reserved0 = 0;
    io.status = (aerogpu_escape_u32)STATUS_INVALID_PARAMETER;
    io.bytes_copied = 0;

    NTSTATUS st = SendAerogpuEscapeDirect(f, hAdapter, &io, sizeof(io));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(read-gpa) failed", f, st);
      goto cleanup;
    }

    const NTSTATUS op = (NTSTATUS)io.status;
    uint32_t copied = io.bytes_copied;
    if (copied > chunk) {
      copied = chunk;
    }
    if (!NT_SUCCESS(op)) {
      PrintNtStatus(L"read-gpa operation failed", f, op);
      goto cleanup;
    }
    if (copied != chunk) {
      fwprintf(stderr,
               L"read-gpa short read: gpa=0x%I64x requested=%lu got=%lu\n",
               (unsigned long long)cur,
               (unsigned long)chunk,
               (unsigned long)copied);
      goto cleanup;
    }

    if (copied != 0 && fwrite(io.data, 1, copied, fp) != copied) {
      fwprintf(stderr, L"Failed to write output file: %s (errno=%d)\n", path, errno);
      goto cleanup;
    }

    done += chunk;
  }

  ok = true;

cleanup:
  if (fclose(fp) != 0 && ok) {
    fwprintf(stderr, L"Failed to close output file: %s (errno=%d)\n", path, errno);
    ok = false;
  }
  if (!ok) {
    BestEffortDeleteOutputFile(path);
  }
  return ok;
}

typedef struct QueryAdapterInfoThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  UINT type;
  void *buf;
  UINT bufSize;
  NTSTATUS status;
  HANDLE done_event;
} QueryAdapterInfoThreadCtx;

static DWORD WINAPI QueryAdapterInfoThreadProc(LPVOID param) {
  QueryAdapterInfoThreadCtx *ctx = (QueryAdapterInfoThreadCtx *)param;
  if (!ctx || !ctx->f || !ctx->f->QueryAdapterInfo || !ctx->buf || ctx->bufSize == 0) {
    if (ctx) {
      ctx->status = STATUS_INVALID_PARAMETER;
      if (ctx->done_event) {
        SetEvent(ctx->done_event);
      }
    }
    return 0;
  }

  D3DKMT_QUERYADAPTERINFO q;
  ZeroMemory(&q, sizeof(q));
  q.hAdapter = ctx->hAdapter;
  q.Type = ctx->type;
  q.pPrivateDriverData = ctx->buf;
  q.PrivateDriverDataSize = ctx->bufSize;

  ctx->status = ctx->f->QueryAdapterInfo(&q);

  if (ctx->done_event) {
    SetEvent(ctx->done_event);
  }
  return 0;
}

static NTSTATUS QueryAdapterInfoWithTimeout(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, UINT type, void *buf,
                                            UINT bufSize) {
  if (!f || !f->QueryAdapterInfo || !hAdapter || !buf || bufSize == 0) {
    return STATUS_INVALID_PARAMETER;
  }

  D3DKMT_QUERYADAPTERINFO q;
  ZeroMemory(&q, sizeof(q));
  q.hAdapter = hAdapter;
  q.Type = type;
  q.pPrivateDriverData = buf;
  q.PrivateDriverDataSize = bufSize;

  if (g_escape_timeout_ms == 0) {
    return f->QueryAdapterInfo(&q);
  }

  // Run QueryAdapterInfo on a worker thread so a buggy kernel driver cannot hang dbgctl forever. If the call times out,
  // leak the context (the thread may be blocked inside the kernel thunk) and set a global so we avoid calling
  // D3DKMTCloseAdapter.
  QueryAdapterInfoThreadCtx *ctx =
      (QueryAdapterInfoThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*ctx));
  if (!ctx) {
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  void *bufCopy = HeapAlloc(GetProcessHeap(), 0, bufSize);
  if (!bufCopy) {
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }
  memcpy(bufCopy, buf, bufSize);

  ctx->f = f;
  ctx->hAdapter = hAdapter;
  ctx->type = type;
  ctx->buf = bufCopy;
  ctx->bufSize = bufSize;
  ctx->status = 0;
  ctx->done_event = CreateEventW(NULL, TRUE, FALSE, NULL);
  if (!ctx->done_event) {
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  HANDLE thread = CreateThread(NULL, 0, QueryAdapterInfoThreadProc, ctx, 0, NULL);
  if (!thread) {
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, bufCopy);
    HeapFree(GetProcessHeap(), 0, ctx);
    return STATUS_INSUFFICIENT_RESOURCES;
  }

  DWORD w = WaitForSingleObject(ctx->done_event, g_escape_timeout_ms);
  if (w == WAIT_OBJECT_0) {
    const NTSTATUS st = ctx->status;
    if (NT_SUCCESS(st)) {
      memcpy(buf, ctx->buf, bufSize);
    }
    CloseHandle(thread);
    CloseHandle(ctx->done_event);
    HeapFree(GetProcessHeap(), 0, ctx->buf);
    HeapFree(GetProcessHeap(), 0, ctx);
    return st;
  }

  CloseHandle(thread);
  InterlockedExchange(&g_skip_close_adapter, 1);
  return (w == WAIT_TIMEOUT) ? STATUS_TIMEOUT : STATUS_INVALID_PARAMETER;
}

static const wchar_t *SelftestErrorToString(uint32_t code) {
  switch (code) {
  case AEROGPU_DBGCTL_SELFTEST_OK:
    return L"OK";
  case AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE:
    return L"INVALID_STATE";
  case AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY:
    return L"RING_NOT_READY";
  case AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY:
    return L"GPU_BUSY";
  case AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES:
    return L"NO_RESOURCES";
  case AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT:
    return L"TIMEOUT";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE:
    return L"VBLANK_REGS_OUT_OF_RANGE";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK:
    return L"VBLANK_SEQ_STUCK";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE:
    return L"VBLANK_IRQ_REGS_OUT_OF_RANGE";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED:
    return L"VBLANK_IRQ_NOT_LATCHED";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED:
    return L"VBLANK_IRQ_NOT_CLEARED";
  case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE:
    return L"CURSOR_REGS_OUT_OF_RANGE";
  case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH:
    return L"CURSOR_RW_MISMATCH";
  case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED:
    return L"VBLANK_IRQ_NOT_DELIVERED";
  case AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED:
    return L"TIME_BUDGET_EXHAUSTED";
  default:
    return L"UNKNOWN";
  }
}

static const wchar_t *DxgkMemorySegmentGroupToString(UINT group) {
  switch (group) {
  case DXGK_MEMORY_SEGMENT_GROUP_LOCAL:
    return L"Local";
  case DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL:
    return L"NonLocal";
  default:
    break;
  }

  static __declspec(thread) wchar_t buf[4][32];
  static __declspec(thread) uint32_t buf_index = 0;
  wchar_t *out = buf[buf_index++ & 3u];
  swprintf_s(out, sizeof(buf[0]) / sizeof(buf[0][0]), L"Unknown(%lu)", (unsigned long)group);
  return out;
}

static void PrintBytesAndMiB(ULONGLONG bytes) {
  const ULONGLONG mib = bytes / (1024ull * 1024ull);
  wprintf(L"%I64u bytes (%I64u MiB)", (unsigned long long)bytes, (unsigned long long)mib);
}

static bool IsPlausibleSegmentDescriptor(const DXGK_SEGMENTDESCRIPTOR &d) {
  // Keep heuristics permissive: this tool is primarily used on AeroGPU (single
  // system-memory segment), but should tolerate other WDDM adapters.
  if (d.Size == 0) {
    return false;
  }
  // Avoid obviously bogus results from mis-detected query types.
  if (d.Size > (1ull << 52)) { // 4 PiB
    return false;
  }
  if ((d.Size & 0xFFFu) != 0) {
    // Segment sizes are typically page-aligned.
    return false;
  }
  if (d.MemorySegmentGroup > 8u) {
    return false;
  }
  return true;
}

static bool FindQuerySegmentTypeAndData(const D3DKMT_FUNCS *f,
                                       D3DKMT_HANDLE hAdapter,
                                       UINT segmentCapacity,
                                       UINT *typeOut,
                                       DXGK_QUERYSEGMENTOUT **outBuf,
                                       SIZE_T *outBufSize) {
  if (typeOut) {
    *typeOut = 0;
  }
  if (outBuf) {
    *outBuf = NULL;
  }
  if (outBufSize) {
    *outBufSize = 0;
  }
  if (!f || !f->QueryAdapterInfo || !hAdapter || segmentCapacity == 0) {
    return false;
  }

  const SIZE_T bufSize = offsetof(DXGK_QUERYSEGMENTOUT, pSegmentDescriptor) +
                         (SIZE_T)segmentCapacity * sizeof(DXGK_SEGMENTDESCRIPTOR);

  DXGK_QUERYSEGMENTOUT *buf = (DXGK_QUERYSEGMENTOUT *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, bufSize);
  if (!buf) {
    return false;
  }

  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(buf, bufSize);
    NTSTATUS st = QueryAdapterInfoWithTimeout(f, hAdapter, type, buf, (UINT)bufSize);
    if (!NT_SUCCESS(st)) {
      continue;
    }

    const UINT n = buf->NbSegments;
    if (n == 0 || n > segmentCapacity) {
      continue;
    }

    bool ok = true;
    for (UINT i = 0; i < n; ++i) {
      if (!IsPlausibleSegmentDescriptor(buf->pSegmentDescriptor[i])) {
        ok = false;
        break;
      }
    }
    if (!ok) {
      continue;
    }

    if (typeOut) {
      *typeOut = type;
    }
    if (outBuf) {
      *outBuf = buf;
    }
    if (outBufSize) {
      *outBufSize = bufSize;
    }
    return true;
  }

  HeapFree(GetProcessHeap(), 0, buf);
  return false;
}

static bool FindSegmentGroupSizeTypeAndData(const D3DKMT_FUNCS *f,
                                           D3DKMT_HANDLE hAdapter,
                                           const DXGK_QUERYSEGMENTOUT *segments,
                                           UINT *typeOut,
                                           DXGK_SEGMENTGROUPSIZE *outSizes) {
  if (typeOut) {
    *typeOut = 0;
  }
  if (outSizes) {
    ZeroMemory(outSizes, sizeof(*outSizes));
  }
  if (!f || !f->QueryAdapterInfo || !hAdapter || !outSizes) {
    return false;
  }

  ULONGLONG localMin = 0;
  ULONGLONG nonLocalMin = 0;
  if (segments) {
    const UINT n = segments->NbSegments;
    for (UINT i = 0; i < n; ++i) {
      const DXGK_SEGMENTDESCRIPTOR &d = segments->pSegmentDescriptor[i];
      if (!IsPlausibleSegmentDescriptor(d)) {
        continue;
      }
      if (d.MemorySegmentGroup == DXGK_MEMORY_SEGMENT_GROUP_LOCAL) {
        localMin += d.Size;
      } else if (d.MemorySegmentGroup == DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL) {
        nonLocalMin += d.Size;
      }
    }
  }

  bool haveFallback = false;
  DXGK_SEGMENTGROUPSIZE fallback = {};
  UINT fallbackType = 0;

  for (UINT type = 0; type < 256; ++type) {
    DXGK_SEGMENTGROUPSIZE sizes;
    ZeroMemory(&sizes, sizeof(sizes));
    NTSTATUS st = QueryAdapterInfoWithTimeout(f, hAdapter, type, &sizes, sizeof(sizes));
    if (!NT_SUCCESS(st)) {
      continue;
    }

    // Basic sanity: reject very large/obviously bogus values (likely from probing
    // the wrong KMTQAITYPE).
    if (sizes.LocalMemorySize > (1ull << 52) || sizes.NonLocalMemorySize > (1ull << 52)) {
      continue;
    }
    if (((sizes.LocalMemorySize | sizes.NonLocalMemorySize) & 0xFFFu) != 0) {
      continue;
    }

    if (!haveFallback) {
      haveFallback = true;
      fallback = sizes;
      fallbackType = type;
    }

    // Prefer a type whose values are consistent with the QuerySegment results.
    if (segments) {
      if (sizes.LocalMemorySize >= localMin && sizes.NonLocalMemorySize >= nonLocalMin) {
        *outSizes = sizes;
        if (typeOut) {
          *typeOut = type;
        }
        return true;
      }
    } else {
      *outSizes = sizes;
      if (typeOut) {
        *typeOut = type;
      }
      return true;
    }
  }

  if (haveFallback) {
    *outSizes = fallback;
    if (typeOut) {
      *typeOut = fallbackType;
    }
    return true;
  }

  return false;
}

static const wchar_t *DeviceErrorCodeToString(uint32_t code) {
  switch (code) {
  case AEROGPU_ERROR_NONE:
    return L"NONE";
  case AEROGPU_ERROR_CMD_DECODE:
    return L"CMD_DECODE";
  case AEROGPU_ERROR_OOB:
    return L"OOB";
  case AEROGPU_ERROR_BACKEND:
    return L"BACKEND";
  case AEROGPU_ERROR_INTERNAL:
    return L"INTERNAL";
  default:
    return L"UNKNOWN";
  }
}

static int DoQueryVersion(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  static const uint32_t kLegacyMmioMagic = 0x41524750u; // "ARGP" little-endian
  const auto DumpFenceSnapshot = [&]() {
    aerogpu_escape_query_fence_out qf;
    ZeroMemory(&qf, sizeof(qf));
    qf.hdr.version = AEROGPU_ESCAPE_VERSION;
    qf.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    qf.hdr.size = sizeof(qf);
    qf.hdr.reserved0 = 0;

    NTSTATUS stFence = SendAerogpuEscape(f, hAdapter, &qf, sizeof(qf));
    if (!NT_SUCCESS(stFence)) {
      if (stFence == STATUS_NOT_SUPPORTED) {
        wprintf(L"Fences: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, stFence);
      }
      return;
    }

    wprintf(L"Last submitted fence: 0x%I64x (%I64u)\n",
            (unsigned long long)qf.last_submitted_fence,
            (unsigned long long)qf.last_submitted_fence);
    wprintf(L"Last completed fence: 0x%I64x (%I64u)\n",
            (unsigned long long)qf.last_completed_fence,
            (unsigned long long)qf.last_completed_fence);
    wprintf(L"Error IRQ count:      0x%I64x (%I64u)\n",
            (unsigned long long)qf.error_irq_count,
            (unsigned long long)qf.error_irq_count);
    wprintf(L"Last error fence:     0x%I64x (%I64u)\n",
            (unsigned long long)qf.last_error_fence,
            (unsigned long long)qf.last_error_fence);
  };

  const auto DumpErrorInfoSnapshot = [&]() {
    aerogpu_escape_query_error_out qe;
    ZeroMemory(&qe, sizeof(qe));
    qe.hdr.version = AEROGPU_ESCAPE_VERSION;
    qe.hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
    qe.hdr.size = sizeof(qe);
    qe.hdr.reserved0 = 0;

    NTSTATUS stErr = SendAerogpuEscape(f, hAdapter, &qe, sizeof(qe));
    if (!NT_SUCCESS(stErr)) {
      if (stErr == STATUS_NOT_SUPPORTED) {
        wprintf(L"Last error: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-error) failed", f, stErr);
      }
      return;
    }

    if ((qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID) == 0 ||
        (qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED) == 0) {
      wprintf(L"Last error: (not supported)\n");
      return;
    }

    if (qe.error_code == AEROGPU_ERROR_NONE) {
      wprintf(L"Last error: none (count=%lu)\n", (unsigned long)qe.error_count);
      return;
    }

    wprintf(L"Last error: code=%lu (%s) fence=0x%I64x (%I64u) count=%lu\n",
            (unsigned long)qe.error_code,
            DeviceErrorCodeToString(qe.error_code),
            (unsigned long long)qe.error_fence,
            (unsigned long long)qe.error_fence,
            (unsigned long)qe.error_count);
  };

  const auto DumpUmdPrivateSummary = [&]() {
    if (!f->QueryAdapterInfo) {
      wprintf(L"UMDRIVERPRIVATE: (not available)\n");
      return;
    }

    aerogpu_umd_private_v1 blob;
    ZeroMemory(&blob, sizeof(blob));

    UINT foundType = 0xFFFFFFFFu;
    NTSTATUS lastStatus = 0;
    for (UINT type = 0; type < 256; ++type) {
      ZeroMemory(&blob, sizeof(blob));
      NTSTATUS stUmd = QueryAdapterInfoWithTimeout(f, hAdapter, type, &blob, sizeof(blob));
      lastStatus = stUmd;
      if (!NT_SUCCESS(stUmd)) {
        if (stUmd == STATUS_TIMEOUT) {
          break;
        }
        continue;
      }

      if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
        continue;
      }

      const uint32_t magic = blob.device_mmio_magic;
      if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
        continue;
      }

      foundType = type;
      break;
    }

    if (foundType == 0xFFFFFFFFu) {
      if (lastStatus == STATUS_TIMEOUT) {
        wprintf(L"UMDRIVERPRIVATE: (timed out)\n");
      } else {
        wprintf(L"UMDRIVERPRIVATE: (not found)\n");
      }
      return;
    }

    wchar_t magicStr[5] = {0, 0, 0, 0, 0};
    {
      const uint32_t m = blob.device_mmio_magic;
      magicStr[0] = (wchar_t)((m >> 0) & 0xFF);
      magicStr[1] = (wchar_t)((m >> 8) & 0xFF);
      magicStr[2] = (wchar_t)((m >> 16) & 0xFF);
      magicStr[3] = (wchar_t)((m >> 24) & 0xFF);
    }

    const std::wstring decoded_features = aerogpu::FormatDeviceFeatureBits(blob.device_features, 0);
    wprintf(L"UMDRIVERPRIVATE: type=%lu magic=0x%08lx (%s) abi=0x%08lx features=0x%I64x (%s) flags=0x%08lx\n",
            (unsigned long)foundType,
            (unsigned long)blob.device_mmio_magic,
            magicStr,
            (unsigned long)blob.device_abi_version_u32,
            (unsigned long long)blob.device_features,
            decoded_features.c_str(),
            (unsigned long)blob.flags);
  };

  const auto DumpSegmentBudgetSummary = [&]() {
    if (!f->QueryAdapterInfo) {
      return;
    }

    DXGK_QUERYSEGMENTOUT *segments = NULL;
    const bool haveSegments =
        FindQuerySegmentTypeAndData(f, hAdapter, /*segmentCapacity=*/32, NULL, &segments, NULL);

    DXGK_SEGMENTGROUPSIZE groupSizes;
    const bool haveGroupSizes =
        FindSegmentGroupSizeTypeAndData(f, hAdapter, haveSegments ? segments : NULL, NULL, &groupSizes);

    if (haveSegments || haveGroupSizes) {
      wprintf(L"Segments:");
      if (haveSegments) {
        wprintf(L" count=%lu", (unsigned long)segments->NbSegments);
      }
      if (haveGroupSizes) {
        wprintf(L" Local=");
        PrintBytesAndMiB(groupSizes.LocalMemorySize);
        wprintf(L" NonLocal=");
        PrintBytesAndMiB(groupSizes.NonLocalMemorySize);
      }
      wprintf(L"\n");
    }

    if (segments) {
      HeapFree(GetProcessHeap(), 0, segments);
    }
  };

  const auto DumpRingSummary = [&]() {
    aerogpu_escape_dump_ring_v2_inout q2;
    ZeroMemory(&q2, sizeof(q2));
    q2.hdr.version = AEROGPU_ESCAPE_VERSION;
    q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
    q2.hdr.size = sizeof(q2);
    q2.hdr.reserved0 = 0;
    q2.ring_id = 0;
    q2.desc_capacity = 1;

    NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
    if (NT_SUCCESS(st)) {
      wprintf(L"Ring0:\n");
      wprintf(L"  format=%lu ring_size_bytes=%lu head=%lu tail=%lu desc_count=%lu\n",
              (unsigned long)q2.ring_format,
              (unsigned long)q2.ring_size_bytes,
              (unsigned long)q2.head,
              (unsigned long)q2.tail,
              (unsigned long)q2.desc_count);
      if (q2.desc_count > 0) {
        const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
        wprintf(L"  last: fence=0x%I64x cmd_gpa=0x%I64x cmd_size=%lu flags=0x%08lx alloc_table_gpa=0x%I64x alloc_table_size=%lu\n",
                (unsigned long long)d.fence,
                (unsigned long long)d.cmd_gpa,
                (unsigned long)d.cmd_size_bytes,
                (unsigned long)d.flags,
                (unsigned long long)d.alloc_table_gpa,
                (unsigned long)d.alloc_table_size_bytes);
      }
      return;
    }

    if (st == STATUS_NOT_SUPPORTED) {
      // Fall back to the legacy dump-ring packet for older drivers.
      aerogpu_escape_dump_ring_inout q1;
      ZeroMemory(&q1, sizeof(q1));
      q1.hdr.version = AEROGPU_ESCAPE_VERSION;
      q1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
      q1.hdr.size = sizeof(q1);
      q1.hdr.reserved0 = 0;
      q1.ring_id = 0;
      q1.desc_capacity = 1;

      NTSTATUS st1 = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
      if (!NT_SUCCESS(st1)) {
        if (st1 == STATUS_NOT_SUPPORTED) {
          wprintf(L"Ring0: (not supported)\n");
        } else {
          PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st1);
        }
        return;
      }

      wprintf(L"Ring0:\n");
      wprintf(L"  ring_size_bytes=%lu head=%lu tail=%lu desc_count=%lu\n",
              (unsigned long)q1.ring_size_bytes,
              (unsigned long)q1.head,
              (unsigned long)q1.tail,
              (unsigned long)q1.desc_count);
      if (q1.desc_count > 0) {
        const aerogpu_dbgctl_ring_desc &d = q1.desc[q1.desc_count - 1];
        wprintf(L"  last: fence=0x%I64x cmd_gpa=0x%I64x cmd_size=%lu flags=0x%08lx\n",
                (unsigned long long)d.signal_fence,
                (unsigned long long)d.cmd_gpa,
                (unsigned long)d.cmd_size_bytes,
                (unsigned long)d.flags);
      }
      return;
    }

    PrintNtStatus(L"D3DKMTEscape(dump-ring-v2) failed", f, st);
  };

  const auto DumpScanoutSnapshot = [&]() {
    aerogpu_escape_query_scanout_out qs;
    ZeroMemory(&qs, sizeof(qs));
    qs.hdr.version = AEROGPU_ESCAPE_VERSION;
    qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    qs.hdr.size = sizeof(qs);
    qs.hdr.reserved0 = 0;
    qs.vidpn_source_id = 0;

    NTSTATUS stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
    if (!NT_SUCCESS(stScanout)) {
      if (stScanout == STATUS_NOT_SUPPORTED) {
        wprintf(L"Scanout0: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, stScanout);
      }
      return;
    }

    wprintf(L"Scanout0:\n");
    wprintf(L"  cached: enable=%lu width=%lu height=%lu format=%S pitch=%lu\n",
            (unsigned long)qs.cached_enable,
            (unsigned long)qs.cached_width,
            (unsigned long)qs.cached_height,
            AerogpuFormatName(qs.cached_format),
            (unsigned long)qs.cached_pitch_bytes);
    wprintf(L"  mmio:   enable=%lu width=%lu height=%lu format=%S pitch=%lu fb_gpa=0x%I64x\n",
            (unsigned long)qs.mmio_enable,
            (unsigned long)qs.mmio_width,
            (unsigned long)qs.mmio_height,
            AerogpuFormatName(qs.mmio_format),
            (unsigned long)qs.mmio_pitch_bytes,
            (unsigned long long)qs.mmio_fb_gpa);
  };

  const auto DumpCursorSummary = [&]() {
    aerogpu_escape_query_cursor_out qc;
    ZeroMemory(&qc, sizeof(qc));
    qc.hdr.version = AEROGPU_ESCAPE_VERSION;
    qc.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
    qc.hdr.size = sizeof(qc);
    qc.hdr.reserved0 = 0;

    NTSTATUS stCursor = SendAerogpuEscape(f, hAdapter, &qc, sizeof(qc));
    if (!NT_SUCCESS(stCursor)) {
      // Older KMDs may not implement this escape; keep --status output stable.
      return;
    }

    bool supported = true;
    if ((qc.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
      supported = (qc.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
    }
    if (!supported) {
      return;
    }

    const int32_t x = (int32_t)qc.x;
    const int32_t y = (int32_t)qc.y;
    wprintf(L"Cursor: enable=%lu pos=(%ld,%ld) hot=(%lu,%lu) size=%lux%lu format=%S pitch=%lu fb_gpa=0x%I64x\n",
            (unsigned long)qc.enable,
            (long)x,
            (long)y,
            (unsigned long)qc.hot_x,
            (unsigned long)qc.hot_y,
            (unsigned long)qc.width,
            (unsigned long)qc.height,
            AerogpuFormatName(qc.format),
            (unsigned long)qc.pitch_bytes,
            (unsigned long long)qc.fb_gpa);
  };

  const auto DumpVblankSnapshot = [&]() {
    aerogpu_escape_query_vblank_out qv;
    ZeroMemory(&qv, sizeof(qv));
    qv.hdr.version = AEROGPU_ESCAPE_VERSION;
    qv.hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
    qv.hdr.size = sizeof(qv);
    qv.hdr.reserved0 = 0;
    qv.vidpn_source_id = 0;

    NTSTATUS stVblank = SendAerogpuEscape(f, hAdapter, &qv, sizeof(qv));
    if (!NT_SUCCESS(stVblank)) {
      if (stVblank == STATUS_NOT_SUPPORTED) {
        wprintf(L"Scanout0 vblank: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-vblank) failed", f, stVblank);
      }
      return;
    }

    bool supported = true;
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      supported = (qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }

    wprintf(L"Scanout0 vblank:\n");
    wprintf(L"  irq_enable: 0x%08lx\n", (unsigned long)qv.irq_enable);
    wprintf(L"  irq_status: 0x%08lx\n", (unsigned long)qv.irq_status);
    wprintf(L"  irq_active: 0x%08lx\n", (unsigned long)(qv.irq_enable & qv.irq_status));
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
        wprintf(L"  vblank_interrupt_type: %lu\n", (unsigned long)qv.vblank_interrupt_type);
      } else {
        wprintf(L"  vblank_interrupt_type: (not enabled or not reported)\n");
      }
    }
    if (!supported) {
      wprintf(L"  (not supported)\n");
      return;
    }

    if (qv.vblank_period_ns != 0) {
      const double hz = 1000000000.0 / (double)qv.vblank_period_ns;
      wprintf(L"  vblank_period_ns: %lu (~%.3f Hz)\n", (unsigned long)qv.vblank_period_ns, hz);
    } else {
      wprintf(L"  vblank_period_ns: 0\n");
    }
    wprintf(L"  vblank_seq: 0x%I64x (%I64u)\n", (unsigned long long)qv.vblank_seq, (unsigned long long)qv.vblank_seq);
    wprintf(L"  last_vblank_time_ns: 0x%I64x (%I64u ns)\n",
            (unsigned long long)qv.last_vblank_time_ns,
            (unsigned long long)qv.last_vblank_time_ns);
  };

  const auto DumpErrorSnapshot = [&]() {
    aerogpu_escape_query_error_out qe;
    ZeroMemory(&qe, sizeof(qe));
    qe.hdr.version = AEROGPU_ESCAPE_VERSION;
    qe.hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
    qe.hdr.size = sizeof(qe);
    qe.hdr.reserved0 = 0;
    NTSTATUS stErr = SendAerogpuEscape(f, hAdapter, &qe, sizeof(qe));
    if (!NT_SUCCESS(stErr)) {
      if (stErr == STATUS_NOT_SUPPORTED) {
        wprintf(L"Last error: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(query-error) failed", f, stErr);
      }
      return;
    }
 
    bool supported = true;
    if ((qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID) != 0) {
      supported = (qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED) != 0;
    }
    if (!supported) {
      wprintf(L"Last error: (not supported)\n");
      return;
    }
 
    wprintf(L"Last error: code=%lu (%s) fence=0x%I64x count=%lu\n",
            (unsigned long)qe.error_code,
            AerogpuErrorCodeName(qe.error_code),
            (unsigned long long)qe.error_fence,
            (unsigned long)qe.error_count);
  };

  const auto DumpCreateAllocationSummary = [&]() {
    aerogpu_escape_dump_createallocation_inout qa;
    ZeroMemory(&qa, sizeof(qa));
    qa.hdr.version = AEROGPU_ESCAPE_VERSION;
    qa.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
    qa.hdr.size = sizeof(qa);
    qa.hdr.reserved0 = 0;
    qa.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;

    NTSTATUS stAlloc = SendAerogpuEscape(f, hAdapter, &qa, sizeof(qa));
    if (!NT_SUCCESS(stAlloc)) {
      if (stAlloc == STATUS_NOT_SUPPORTED) {
        wprintf(L"CreateAllocation trace: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTEscape(dump-createalloc) failed", f, stAlloc);
      }
      return;
    }

    wprintf(L"CreateAllocation trace: write_index=%lu entry_count=%lu entry_capacity=%lu\n",
            (unsigned long)qa.write_index,
            (unsigned long)qa.entry_count,
            (unsigned long)qa.entry_capacity);
  };

  aerogpu_escape_query_device_v2_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    // Fall back to legacy QUERY_DEVICE for older drivers.
    aerogpu_escape_query_device_out q1;
    ZeroMemory(&q1, sizeof(q1));
    q1.hdr.version = AEROGPU_ESCAPE_VERSION;
    q1.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
    q1.hdr.size = sizeof(q1);
    q1.hdr.reserved0 = 0;

    st = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(query-version) failed", f, st);
      return 2;
    }

    const uint32_t major = (uint32_t)(q1.mmio_version >> 16);
    const uint32_t minor = (uint32_t)(q1.mmio_version & 0xFFFFu);
    wprintf(L"AeroGPU escape ABI: %lu\n", (unsigned long)q1.hdr.version);
    wprintf(L"AeroGPU ABI version: 0x%08lx (%lu.%lu)\n",
            (unsigned long)q1.mmio_version,
            (unsigned long)major,
            (unsigned long)minor);
 
    DumpFenceSnapshot();
    DumpErrorInfoSnapshot();
    DumpUmdPrivateSummary();
    DumpSegmentBudgetSummary();
    DumpRingSummary();
    DumpErrorSnapshot();
    DumpScanoutSnapshot();
    DumpCursorSummary();
    DumpVblankSnapshot();
    DumpCreateAllocationSummary();
    return 0;
  }

  const wchar_t *abiStr = L"unknown";
  if (q.detected_mmio_magic == kLegacyMmioMagic) {
    abiStr = L"legacy (ARGP)";
  } else if (q.detected_mmio_magic == AEROGPU_MMIO_MAGIC) {
    abiStr = L"new (AGPU)";
  }

  const uint32_t major = (uint32_t)(q.abi_version_u32 >> 16);
  const uint32_t minor = (uint32_t)(q.abi_version_u32 & 0xFFFFu);

  wprintf(L"AeroGPU escape ABI: %lu\n", (unsigned long)q.hdr.version);
  wprintf(L"AeroGPU device ABI: %s\n", abiStr);
  wprintf(L"AeroGPU MMIO magic: 0x%08lx\n", (unsigned long)q.detected_mmio_magic);
  wprintf(L"AeroGPU ABI version: 0x%08lx (%lu.%lu)\n",
          (unsigned long)q.abi_version_u32,
          (unsigned long)major,
          (unsigned long)minor);

  wprintf(L"AeroGPU features:\n");
  wprintf(L"  lo=0x%I64x hi=0x%I64x\n", (unsigned long long)q.features_lo, (unsigned long long)q.features_hi);
  if (q.detected_mmio_magic == kLegacyMmioMagic) {
    wprintf(L"  (note: legacy device; feature bits are best-effort)\n");
  }
  const std::wstring decoded = aerogpu::FormatDeviceFeatureBits(q.features_lo, q.features_hi);
  wprintf(L"  decoded: %s\n", decoded.c_str());

  DumpFenceSnapshot();
  DumpErrorInfoSnapshot();
  DumpUmdPrivateSummary();
  DumpSegmentBudgetSummary();
  DumpRingSummary();
  DumpErrorSnapshot();
  DumpScanoutSnapshot();
  DumpCursorSummary();
  DumpVblankSnapshot();
  DumpCreateAllocationSummary();

  return 0;
}

static void JsonWriteU64HexDec(JsonWriter &w, const char *key, uint64_t v) {
  w.Key(key);
  w.BeginObject();
  w.Key("hex");
  w.String(HexU64(v));
  w.Key("dec");
  w.String(DecU64(v));
  w.EndObject();
}

static void JsonWriteU32Hex(JsonWriter &w, const char *key, uint32_t v) {
  w.Key(key);
  w.String(HexU32(v));
}

static void JsonWriteBytesAndMiB(JsonWriter &w, const char *key, uint64_t bytes) {
  w.Key(key);
  w.BeginObject();
  w.Key("bytes");
  w.String(DecU64(bytes));
  w.Key("mib");
  w.String(DecU64(bytes / (1024ull * 1024ull)));
  w.EndObject();
}

static void JsonWriteDecodedFeatureList(JsonWriter &w, const char *key, const std::wstring &decoded) {
  const std::string utf8 = WideToUtf8(decoded);
  w.Key(key);
  w.BeginArray();
  size_t start = 0;
  while (start < utf8.size()) {
    size_t end = utf8.find(',', start);
    size_t part_end = (end == std::string::npos) ? utf8.size() : end;
    size_t a = start;
    while (a < part_end && (utf8[a] == ' ' || utf8[a] == '\t' || utf8[a] == '\r' || utf8[a] == '\n')) {
      ++a;
    }
    size_t b = part_end;
    while (b > a && (utf8[b - 1] == ' ' || utf8[b - 1] == '\t' || utf8[b - 1] == '\r' || utf8[b - 1] == '\n')) {
      --b;
    }
    if (b > a) {
      w.String(utf8.substr(a, b - a));
    }
    if (end == std::string::npos) {
      break;
    }
    start = end + 1;
  }
  w.EndArray();
}

static int DoStatusJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }

  static const uint32_t kLegacyMmioMagic = 0x41524750u; // "ARGP" little-endian

  // Query device (prefer v2, fall back to legacy).
  bool deviceV2 = false;
  aerogpu_escape_query_device_v2_out q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;

  NTSTATUS stDevice = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
  aerogpu_escape_query_device_out q1;
  ZeroMemory(&q1, sizeof(q1));
  if (!NT_SUCCESS(stDevice)) {
    // Legacy fallback.
    q1.hdr.version = AEROGPU_ESCAPE_VERSION;
    q1.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
    q1.hdr.size = sizeof(q1);
    q1.hdr.reserved0 = 0;
    stDevice = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
    if (!NT_SUCCESS(stDevice)) {
      JsonWriteTopLevelError(out, "status", f, "D3DKMTEscape(query-device) failed", stDevice);
      return 2;
    }
    deviceV2 = false;
  } else {
    deviceV2 = true;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("status");
  w.Key("ok");
  w.Bool(true);

  // Device / ABI / features.
  w.Key("device");
  w.BeginObject();
  w.Key("escape_abi");
  w.Uint32((uint32_t)(deviceV2 ? q2.hdr.version : q1.hdr.version));
  w.Key("query_device");
  w.String(deviceV2 ? "query-device-v2" : "query-device-legacy");

  uint32_t abi_version_u32 = 0;
  if (deviceV2) {
    abi_version_u32 = q2.abi_version_u32;
    w.Key("mmio_magic_u32_hex");
    w.String(HexU32(q2.detected_mmio_magic));

    const char *abiKind = "unknown";
    if (q2.detected_mmio_magic == kLegacyMmioMagic) {
      abiKind = "legacy";
    } else if (q2.detected_mmio_magic == AEROGPU_MMIO_MAGIC) {
      abiKind = "new";
    }
    w.Key("device_abi");
    w.String(abiKind);
  } else {
    abi_version_u32 = q1.mmio_version;
    w.Key("mmio_magic_u32_hex");
    w.Null();
    w.Key("device_abi");
    w.String("unknown");
  }

  w.Key("abi_version_u32_hex");
  w.String(HexU32(abi_version_u32));
  w.Key("abi_version");
  w.BeginObject();
  w.Key("major");
  w.Uint32((uint32_t)(abi_version_u32 >> 16));
  w.Key("minor");
  w.Uint32((uint32_t)(abi_version_u32 & 0xFFFFu));
  w.EndObject();

  w.Key("features");
  w.BeginObject();
  if (deviceV2) {
    w.Key("available");
    w.Bool(true);
    w.Key("lo_hex");
    w.String(HexU64(q2.features_lo));
    w.Key("hi_hex");
    w.String(HexU64(q2.features_hi));
    const std::wstring decoded = aerogpu::FormatDeviceFeatureBits(q2.features_lo, q2.features_hi);
    w.Key("decoded");
    w.String(WideToUtf8(decoded));
    JsonWriteDecodedFeatureList(w, "decoded_list", decoded);
    if (q2.detected_mmio_magic == kLegacyMmioMagic) {
      w.Key("note");
      w.String("legacy device; feature bits are best-effort");
    }
  } else {
    w.Key("available");
    w.Bool(false);
  }
  w.EndObject();
  w.EndObject();

  // Fences.
  w.Key("fences");
  w.BeginObject();
  aerogpu_escape_query_fence_out qf;
  ZeroMemory(&qf, sizeof(qf));
  qf.hdr.version = AEROGPU_ESCAPE_VERSION;
  qf.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  qf.hdr.size = sizeof(qf);
  qf.hdr.reserved0 = 0;
  NTSTATUS stFence = SendAerogpuEscape(f, hAdapter, &qf, sizeof(qf));
  if (NT_SUCCESS(stFence)) {
    w.Key("supported");
    w.Bool(true);
    JsonWriteU64HexDec(w, "last_submitted_fence", qf.last_submitted_fence);
    JsonWriteU64HexDec(w, "last_completed_fence", qf.last_completed_fence);
    JsonWriteU64HexDec(w, "error_irq_count", qf.error_irq_count);
    JsonWriteU64HexDec(w, "last_error_fence", qf.last_error_fence);
  } else {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stFence);
  }
  w.EndObject();

  // Perf snapshot.
  w.Key("perf");
  w.BeginObject();
  aerogpu_escape_query_perf_out qp;
  ZeroMemory(&qp, sizeof(qp));
  qp.hdr.version = AEROGPU_ESCAPE_VERSION;
  qp.hdr.op = AEROGPU_ESCAPE_OP_QUERY_PERF;
  qp.hdr.size = sizeof(qp);
  qp.hdr.reserved0 = 0;
  const NTSTATUS stPerf = SendAerogpuEscape(f, hAdapter, &qp, sizeof(qp));
  if (!NT_SUCCESS(stPerf)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stPerf);
  } else {
    w.Key("supported");
    w.Bool(true);

    const uint64_t submitted = (uint64_t)qp.last_submitted_fence;
    const uint64_t completed = (uint64_t)qp.last_completed_fence;
    const uint64_t pendingFences = (submitted >= completed) ? (submitted - completed) : 0;

    uint32_t ringPending = 0;
    if (qp.ring0_entry_count != 0) {
      const uint32_t head = qp.ring0_head;
      const uint32_t tail = qp.ring0_tail;
      if (tail >= head) {
        ringPending = tail - head;
      } else {
        ringPending = tail + qp.ring0_entry_count - head;
      }
      if (ringPending > qp.ring0_entry_count) {
        ringPending = qp.ring0_entry_count;
      }
    }

    w.Key("fences");
    w.BeginObject();
    JsonWriteU64HexDec(w, "last_submitted_fence", submitted);
    JsonWriteU64HexDec(w, "last_completed_fence", completed);
    w.Key("pending");
    w.String(DecU64(pendingFences));
    w.EndObject();

    w.Key("ring0");
    w.BeginObject();
    w.Key("head");
    w.Uint32(qp.ring0_head);
    w.Key("tail");
    w.Uint32(qp.ring0_tail);
    w.Key("pending");
    w.Uint32(ringPending);
    w.Key("entry_count");
    w.Uint32(qp.ring0_entry_count);
    w.Key("size_bytes");
    w.Uint32(qp.ring0_size_bytes);
    w.EndObject();

    w.Key("submits");
    w.BeginObject();
    JsonWriteU64HexDec(w, "total", qp.total_submissions);
    JsonWriteU64HexDec(w, "render", qp.total_render_submits);
    JsonWriteU64HexDec(w, "present", qp.total_presents);
    JsonWriteU64HexDec(w, "internal", qp.total_internal_submits);
    w.EndObject();

    w.Key("irqs");
    w.BeginObject();
    JsonWriteU64HexDec(w, "fence_delivered", qp.irq_fence_delivered);
    JsonWriteU64HexDec(w, "vblank_delivered", qp.irq_vblank_delivered);
    JsonWriteU64HexDec(w, "spurious", qp.irq_spurious);
    w.EndObject();

    w.Key("resets");
    w.BeginObject();
    JsonWriteU64HexDec(w, "reset_from_timeout_count", qp.reset_from_timeout_count);
    JsonWriteU64HexDec(w, "last_reset_time_100ns", qp.last_reset_time_100ns);
    w.EndObject();

    w.Key("vblank");
    w.BeginObject();
    JsonWriteU64HexDec(w, "seq", qp.vblank_seq);
    JsonWriteU64HexDec(w, "last_time_ns", qp.last_vblank_time_ns);
    w.Key("period_ns");
    w.Uint32(qp.vblank_period_ns);
    w.EndObject();
  }
  w.EndObject();

  // Segment budget summary (QueryAdapterInfo probing).
  w.Key("segments");
  w.BeginObject();
  if (!f->QueryAdapterInfo) {
    w.Key("available");
    w.Bool(false);
    w.Key("reason");
    w.String("missing_gdi32_export");
  } else {
    w.Key("available");
    w.Bool(true);
    DXGK_QUERYSEGMENTOUT *segments = NULL;
    UINT queryType = 0;
    const bool haveSegments = FindQuerySegmentTypeAndData(f, hAdapter, /*segmentCapacity=*/32, &queryType, &segments, NULL);
    if (haveSegments) {
      w.Key("query_segment_type");
      w.Uint32(queryType);
      w.Key("count");
      w.Uint32(segments->NbSegments);
    } else {
      w.Key("count");
      w.Null();
    }
    DXGK_SEGMENTGROUPSIZE groupSizes;
    UINT groupType = 0;
    const bool haveGroupSizes = FindSegmentGroupSizeTypeAndData(f, hAdapter, haveSegments ? segments : NULL, &groupType, &groupSizes);
    if (haveGroupSizes) {
      w.Key("group_sizes");
      w.BeginObject();
      w.Key("type");
      w.Uint32(groupType);
      JsonWriteBytesAndMiB(w, "local_memory_size", (uint64_t)groupSizes.LocalMemorySize);
      JsonWriteBytesAndMiB(w, "non_local_memory_size", (uint64_t)groupSizes.NonLocalMemorySize);
      w.EndObject();
    } else {
      w.Key("group_sizes");
      w.Null();
    }
    if (segments) {
      HeapFree(GetProcessHeap(), 0, segments);
    }
  }
  w.EndObject();

  // UMDRIVERPRIVATE summary.
  w.Key("umd_private");
  w.BeginObject();
  if (!f->QueryAdapterInfo) {
    w.Key("available");
    w.Bool(false);
    w.Key("reason");
    w.String("missing_gdi32_export");
  } else {
    w.Key("available");
    w.Bool(true);
    aerogpu_umd_private_v1 blob;
    ZeroMemory(&blob, sizeof(blob));
    UINT foundType = 0xFFFFFFFFu;
    NTSTATUS lastStatus = 0;
    for (UINT type = 0; type < 256; ++type) {
      ZeroMemory(&blob, sizeof(blob));
      const NTSTATUS stUmd = QueryAdapterInfoWithTimeout(f, hAdapter, type, &blob, sizeof(blob));
      lastStatus = stUmd;
      if (!NT_SUCCESS(stUmd)) {
        if (stUmd == STATUS_TIMEOUT) {
          break;
        }
        continue;
      }
      if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
        continue;
      }
      const uint32_t magic = blob.device_mmio_magic;
      if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
        continue;
      }
      foundType = type;
      break;
    }

    if (foundType == 0xFFFFFFFFu) {
      w.Key("found");
      w.Bool(false);
      w.Key("reason");
      w.String((lastStatus == STATUS_TIMEOUT) ? "timeout" : "not_found");
      if (lastStatus != 0) {
        w.Key("last_error");
        JsonWriteNtStatusError(w, f, lastStatus);
      }
    } else {
      w.Key("found");
      w.Bool(true);
      w.Key("type");
      w.Uint32((uint32_t)foundType);

      char magicStr[5] = {0, 0, 0, 0, 0};
      {
        const uint32_t m = blob.device_mmio_magic;
        magicStr[0] = (char)((m >> 0) & 0xFF);
        magicStr[1] = (char)((m >> 8) & 0xFF);
        magicStr[2] = (char)((m >> 16) & 0xFF);
        magicStr[3] = (char)((m >> 24) & 0xFF);
      }

      w.Key("device_mmio_magic_u32_hex");
      w.String(HexU32(blob.device_mmio_magic));
      w.Key("device_mmio_magic_str");
      w.String(magicStr);
      w.Key("device_abi_version_u32_hex");
      w.String(HexU32(blob.device_abi_version_u32));

      w.Key("device_abi_version");
      w.BeginObject();
      w.Key("major");
      w.Uint32((uint32_t)(blob.device_abi_version_u32 >> 16));
      w.Key("minor");
      w.Uint32((uint32_t)(blob.device_abi_version_u32 & 0xFFFFu));
      w.EndObject();

      w.Key("device_features_u64_hex");
      w.String(HexU64(blob.device_features));
      const std::wstring decoded_features = aerogpu::FormatDeviceFeatureBits(blob.device_features, 0);
      w.Key("decoded_features");
      w.String(WideToUtf8(decoded_features));
      JsonWriteDecodedFeatureList(w, "decoded_features_list", decoded_features);

      w.Key("flags_u32_hex");
      w.String(HexU32(blob.flags));
      w.Key("flags");
      w.BeginObject();
      w.Key("is_legacy");
      w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) != 0);
      w.Key("has_vblank");
      w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0);
      w.Key("has_fence_page");
      w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE) != 0);
      w.EndObject();
    }
  }
  w.EndObject();

  // Ring0 summary.
  w.Key("ring0");
  w.BeginObject();
  aerogpu_escape_dump_ring_v2_inout qr2;
  ZeroMemory(&qr2, sizeof(qr2));
  qr2.hdr.version = AEROGPU_ESCAPE_VERSION;
  qr2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  qr2.hdr.size = sizeof(qr2);
  qr2.hdr.reserved0 = 0;
  qr2.ring_id = 0;
  qr2.desc_capacity = 1;
  NTSTATUS stRing = SendAerogpuEscape(f, hAdapter, &qr2, sizeof(qr2));
  if (NT_SUCCESS(stRing)) {
    w.Key("supported");
    w.Bool(true);
    w.Key("format");
    const char *fmt = "unknown";
    switch (qr2.ring_format) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      fmt = "legacy";
      break;
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      fmt = "agpu";
      break;
    default:
      fmt = "unknown";
      break;
    }
    w.String(fmt);
    w.Key("ring_size_bytes");
    w.Uint32(qr2.ring_size_bytes);
    w.Key("head");
    w.Uint32(qr2.head);
    w.Key("tail");
    w.Uint32(qr2.tail);
    w.Key("desc_count");
    w.Uint32(qr2.desc_count);
    if (qr2.desc_count > 0) {
      const aerogpu_dbgctl_ring_desc_v2 &d = qr2.desc[qr2.desc_count - 1];
      w.Key("last");
      w.BeginObject();
      JsonWriteU64HexDec(w, "fence", d.fence);
      w.Key("cmd_gpa_hex");
      w.String(HexU64(d.cmd_gpa));
      w.Key("cmd_size_bytes");
      w.Uint32(d.cmd_size_bytes);
      JsonWriteU32Hex(w, "flags_u32_hex", d.flags);
      w.Key("alloc_table_gpa_hex");
      w.String(HexU64(d.alloc_table_gpa));
      w.Key("alloc_table_size_bytes");
      w.Uint32(d.alloc_table_size_bytes);
      w.EndObject();
    }
  } else if (stRing == STATUS_NOT_SUPPORTED) {
    aerogpu_escape_dump_ring_inout qr1;
    ZeroMemory(&qr1, sizeof(qr1));
    qr1.hdr.version = AEROGPU_ESCAPE_VERSION;
    qr1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
    qr1.hdr.size = sizeof(qr1);
    qr1.hdr.reserved0 = 0;
    qr1.ring_id = 0;
    qr1.desc_capacity = 1;
    NTSTATUS stRing1 = SendAerogpuEscape(f, hAdapter, &qr1, sizeof(qr1));
    if (NT_SUCCESS(stRing1)) {
      w.Key("supported");
      w.Bool(true);
      w.Key("format");
      w.String("legacy_v1");
      w.Key("ring_size_bytes");
      w.Uint32(qr1.ring_size_bytes);
      w.Key("head");
      w.Uint32(qr1.head);
      w.Key("tail");
      w.Uint32(qr1.tail);
      w.Key("desc_count");
      w.Uint32(qr1.desc_count);
      if (qr1.desc_count > 0) {
        const aerogpu_dbgctl_ring_desc &d = qr1.desc[qr1.desc_count - 1];
        w.Key("last");
        w.BeginObject();
        JsonWriteU64HexDec(w, "fence", d.signal_fence);
        w.Key("cmd_gpa_hex");
        w.String(HexU64(d.cmd_gpa));
        w.Key("cmd_size_bytes");
        w.Uint32(d.cmd_size_bytes);
        JsonWriteU32Hex(w, "flags_u32_hex", d.flags);
        w.EndObject();
      }
    } else {
      w.Key("supported");
      w.Bool(false);
      w.Key("error");
      JsonWriteNtStatusError(w, f, stRing1);
    }
  } else {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stRing);
  }
  w.EndObject();

  // Last error snapshot.
  w.Key("last_error");
  w.BeginObject();
  aerogpu_escape_query_error_out qe;
  ZeroMemory(&qe, sizeof(qe));
  qe.hdr.version = AEROGPU_ESCAPE_VERSION;
  qe.hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
  qe.hdr.size = sizeof(qe);
  qe.hdr.reserved0 = 0;
  const NTSTATUS stErr = SendAerogpuEscape(f, hAdapter, &qe, sizeof(qe));
  if (!NT_SUCCESS(stErr)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stErr);
  } else {
    bool supported = true;
    if ((qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID) != 0) {
      supported = (qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED) != 0;
    }
    w.Key("supported");
    w.Bool(supported);
    JsonWriteU32Hex(w, "flags_u32_hex", qe.flags);
    if (supported) {
      w.Key("error_code");
      w.Uint32(qe.error_code);
      w.Key("error_code_name");
      w.String(WideToUtf8(AerogpuErrorCodeName(qe.error_code)));
      JsonWriteU64HexDec(w, "error_fence", qe.error_fence);
      w.Key("error_count");
      w.Uint32(qe.error_count);
    }
  }
  w.EndObject();

  // Scanout0 snapshot.
  w.Key("scanout0");
  w.BeginObject();
  aerogpu_escape_query_scanout_out qs;
  ZeroMemory(&qs, sizeof(qs));
  qs.hdr.version = AEROGPU_ESCAPE_VERSION;
  qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  qs.hdr.size = sizeof(qs);
  qs.hdr.reserved0 = 0;
  qs.vidpn_source_id = 0;
  NTSTATUS stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
  if (NT_SUCCESS(stScanout)) {
    w.Key("supported");
    w.Bool(true);
    w.Key("vidpn_source_id");
    w.Uint32(qs.vidpn_source_id);
    w.Key("cached");
    w.BeginObject();
    w.Key("enable");
    w.Uint32(qs.cached_enable);
    w.Key("width");
    w.Uint32(qs.cached_width);
    w.Key("height");
    w.Uint32(qs.cached_height);
    w.Key("format");
    w.String(AerogpuFormatName(qs.cached_format));
    w.Key("pitch_bytes");
    w.Uint32(qs.cached_pitch_bytes);
    w.EndObject();
    w.Key("mmio");
    w.BeginObject();
    w.Key("enable");
    w.Uint32(qs.mmio_enable);
    w.Key("width");
    w.Uint32(qs.mmio_width);
    w.Key("height");
    w.Uint32(qs.mmio_height);
    w.Key("format");
    w.String(AerogpuFormatName(qs.mmio_format));
    w.Key("pitch_bytes");
    w.Uint32(qs.mmio_pitch_bytes);
    w.Key("fb_gpa_hex");
    w.String(HexU64(qs.mmio_fb_gpa));
    w.EndObject();
  } else {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stScanout);
  }
  w.EndObject();

  // Cursor summary.
  w.Key("cursor");
  w.BeginObject();
  aerogpu_escape_query_cursor_out qc;
  ZeroMemory(&qc, sizeof(qc));
  qc.hdr.version = AEROGPU_ESCAPE_VERSION;
  qc.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  qc.hdr.size = sizeof(qc);
  qc.hdr.reserved0 = 0;
  NTSTATUS stCursor = SendAerogpuEscape(f, hAdapter, &qc, sizeof(qc));
  if (!NT_SUCCESS(stCursor)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stCursor);
  } else {
    bool cursorSupported = true;
    if ((qc.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
      cursorSupported = (qc.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
    }
    w.Key("supported");
    w.Bool(cursorSupported);
    JsonWriteU32Hex(w, "flags_u32_hex", qc.flags);
    if (cursorSupported) {
      w.Key("enable");
      w.Uint32(qc.enable);
      w.Key("x");
      w.Int32((int32_t)qc.x);
      w.Key("y");
      w.Int32((int32_t)qc.y);
      w.Key("hot_x");
      w.Uint32(qc.hot_x);
      w.Key("hot_y");
      w.Uint32(qc.hot_y);
      w.Key("width");
      w.Uint32(qc.width);
      w.Key("height");
      w.Uint32(qc.height);
      w.Key("format");
      w.String(AerogpuFormatName(qc.format));
      w.Key("pitch_bytes");
      w.Uint32(qc.pitch_bytes);
      w.Key("fb_gpa_hex");
      w.String(HexU64(qc.fb_gpa));
    }
  }
  w.EndObject();

  // Vblank snapshot.
  w.Key("vblank");
  w.BeginObject();
  aerogpu_escape_query_vblank_out qv;
  ZeroMemory(&qv, sizeof(qv));
  qv.hdr.version = AEROGPU_ESCAPE_VERSION;
  qv.hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
  qv.hdr.size = sizeof(qv);
  qv.hdr.reserved0 = 0;
  qv.vidpn_source_id = 0;
  NTSTATUS stVblank = SendAerogpuEscape(f, hAdapter, &qv, sizeof(qv));
  if (!NT_SUCCESS(stVblank)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stVblank);
  } else {
    bool vblankSupported = true;
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      vblankSupported = (qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }
    w.Key("supported");
    w.Bool(vblankSupported);
    w.Key("vidpn_source_id");
    w.Uint32(qv.vidpn_source_id);
    JsonWriteU32Hex(w, "flags_u32_hex", qv.flags);
    JsonWriteU32Hex(w, "irq_enable_u32_hex", qv.irq_enable);
    JsonWriteU32Hex(w, "irq_status_u32_hex", qv.irq_status);
    JsonWriteU32Hex(w, "irq_active_u32_hex", (uint32_t)(qv.irq_enable & qv.irq_status));
    if ((qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0 &&
        (qv.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
      w.Key("vblank_interrupt_type");
      w.Uint32(qv.vblank_interrupt_type);
    }
    if (vblankSupported) {
      w.Key("vblank_period_ns");
      w.Uint32(qv.vblank_period_ns);
      JsonWriteU64HexDec(w, "vblank_seq", qv.vblank_seq);
      JsonWriteU64HexDec(w, "last_vblank_time_ns", qv.last_vblank_time_ns);
    }
  }
  w.EndObject();

  // CreateAllocation trace summary.
  w.Key("createallocation_trace");
  w.BeginObject();
  aerogpu_escape_dump_createallocation_inout qa;
  ZeroMemory(&qa, sizeof(qa));
  qa.hdr.version = AEROGPU_ESCAPE_VERSION;
  qa.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
  qa.hdr.size = sizeof(qa);
  qa.hdr.reserved0 = 0;
  qa.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
  NTSTATUS stAlloc = SendAerogpuEscape(f, hAdapter, &qa, sizeof(qa));
  if (!NT_SUCCESS(stAlloc)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stAlloc);
  } else {
    w.Key("supported");
    w.Bool(true);
    w.Key("write_index");
    w.Uint32(qa.write_index);
    w.Key("entry_count");
    w.Uint32(qa.entry_count);
    w.Key("entry_capacity");
    w.Uint32(qa.entry_capacity);
  }
  w.EndObject();

  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryFence(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_query_fence_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, st);
    return 2;
  }

  wprintf(L"Last submitted fence: 0x%I64x (%I64u)\n", (unsigned long long)q.last_submitted_fence,
          (unsigned long long)q.last_submitted_fence);
  wprintf(L"Last completed fence: 0x%I64x (%I64u)\n", (unsigned long long)q.last_completed_fence,
          (unsigned long long)q.last_completed_fence);
  wprintf(L"Error IRQ count:      0x%I64x (%I64u)\n", (unsigned long long)q.error_irq_count,
          (unsigned long long)q.error_irq_count);
  wprintf(L"Last error fence:     0x%I64x (%I64u)\n", (unsigned long long)q.last_error_fence,
          (unsigned long long)q.last_error_fence);
  return 0;
}

static int DoWatchFence(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t samples, uint32_t intervalMs,
                        uint32_t overallTimeoutMs) {
  // Stall threshold: warn after ~2 seconds of no completed-fence progress while work is pending.
  static const uint32_t kStallWarnTimeMs = 2000;

  if (samples == 0) {
    fwprintf(stderr, L"--samples must be > 0\n");
    return 1;
  }
  if (samples > 1000000) {
    samples = 1000000;
  }

  LARGE_INTEGER freq;
  if (!QueryPerformanceFrequency(&freq) || freq.QuadPart <= 0) {
    fwprintf(stderr, L"QueryPerformanceFrequency failed\n");
    return 1;
  }

  const uint32_t stallWarnIntervals =
      (intervalMs != 0) ? ((kStallWarnTimeMs + intervalMs - 1) / intervalMs) : 3;

  LARGE_INTEGER start;
  QueryPerformanceCounter(&start);

  bool havePrev = false;
  uint64_t prevSubmitted = 0;
  uint64_t prevCompleted = 0;
  LARGE_INTEGER prevTime;
  ZeroMemory(&prevTime, sizeof(prevTime));
  uint32_t stallIntervals = 0;

  for (uint32_t i = 0; i < samples; ++i) {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    const double elapsedMs =
        (double)(before.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;

    if (overallTimeoutMs != 0 && elapsedMs >= (double)overallTimeoutMs) {
      fwprintf(stderr, L"watch-fence: overall timeout after %lu ms (printed %lu/%lu samples)\n",
               (unsigned long)overallTimeoutMs, (unsigned long)i, (unsigned long)samples);
      return 2;
    }

    aerogpu_escape_query_fence_out q;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;

    NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(query-fence) failed", f, st);
      return 2;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double tMs = (double)(now.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;

    aerogpu_fence_delta_stats delta;
    ZeroMemory(&delta, sizeof(delta));
    double dtMs = 0.0;
    if (havePrev) {
      const double dtSeconds = (double)(now.QuadPart - prevTime.QuadPart) / (double)freq.QuadPart;
      dtMs = dtSeconds * 1000.0;
      delta = aerogpu_fence_compute_delta(prevSubmitted, prevCompleted, q.last_submitted_fence, q.last_completed_fence,
                                          dtSeconds);
    } else {
      delta.delta_submitted = 0;
      delta.delta_completed = 0;
      delta.completed_per_s = 0.0;
      delta.reset = 0;
    }

    const bool hasPending =
        (q.last_submitted_fence > q.last_completed_fence) && (!delta.reset || !havePrev);
    if (havePrev && !delta.reset && hasPending && delta.delta_completed == 0) {
      stallIntervals += 1;
    } else {
      stallIntervals = 0;
    }

    const bool warnStall = (stallIntervals != 0 && stallIntervals >= stallWarnIntervals);
    const wchar_t *warn = L"-";
    if (havePrev && delta.reset) {
      warn = L"RESET";
    } else if (warnStall) {
      warn = L"STALL";
    }

    const uint64_t pending =
        (q.last_submitted_fence >= q.last_completed_fence) ? (q.last_submitted_fence - q.last_completed_fence) : 0;

    wprintf(L"watch-fence sample=%lu/%lu t_ms=%.3f submitted=0x%I64x completed=0x%I64x pending=%I64u d_sub=%I64u d_comp=%I64u dt_ms=%.3f rate_comp_per_s=%.3f stall_intervals=%lu warn=%s\n",
            (unsigned long)(i + 1), (unsigned long)samples, tMs, (unsigned long long)q.last_submitted_fence,
            (unsigned long long)q.last_completed_fence, (unsigned long long)pending,
            (unsigned long long)delta.delta_submitted, (unsigned long long)delta.delta_completed, dtMs,
            delta.completed_per_s, (unsigned long)stallIntervals, warn);

    prevSubmitted = q.last_submitted_fence;
    prevCompleted = q.last_completed_fence;
    prevTime = now;
    havePrev = true;

    if (i + 1 < samples && intervalMs != 0) {
      DWORD sleepMs = intervalMs;
      if (overallTimeoutMs != 0) {
        LARGE_INTEGER preSleep;
        QueryPerformanceCounter(&preSleep);
        const double elapsedMs2 =
            (double)(preSleep.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;
        if (elapsedMs2 >= (double)overallTimeoutMs) {
          fwprintf(stderr, L"watch-fence: overall timeout after %lu ms (printed %lu/%lu samples)\n",
                   (unsigned long)overallTimeoutMs, (unsigned long)(i + 1), (unsigned long)samples);
          return 2;
        }
        const double remainingMs = (double)overallTimeoutMs - elapsedMs2;
        if (remainingMs < (double)sleepMs) {
          sleepMs = (DWORD)remainingMs;
        }
      }
      if (sleepMs != 0) {
        Sleep(sleepMs);
      }
    }
  }

  return 0;
}

static int DoQueryPerf(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_query_perf_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_PERF;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"QueryPerf: (not supported by this KMD; upgrade AeroGPU driver)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(query-perf) failed", f, st);
    return 2;
  }

  const uint64_t submitted = (uint64_t)q.last_submitted_fence;
  const uint64_t completed = (uint64_t)q.last_completed_fence;
  const uint64_t pendingFences = (submitted >= completed) ? (submitted - completed) : 0;

  uint32_t ringPending = 0;
  if (q.ring0_entry_count != 0) {
    const uint32_t head = q.ring0_head;
    const uint32_t tail = q.ring0_tail;
    if (tail >= head) {
      ringPending = tail - head;
    } else {
      ringPending = tail + q.ring0_entry_count - head;
    }
    if (ringPending > q.ring0_entry_count) {
      ringPending = q.ring0_entry_count;
    }
  }

  bool haveError = false;
  aerogpu_escape_query_error_out qe;
  ZeroMemory(&qe, sizeof(qe));
  qe.hdr.version = AEROGPU_ESCAPE_VERSION;
  qe.hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
  qe.hdr.size = sizeof(qe);
  qe.hdr.reserved0 = 0;
  NTSTATUS stErr = SendAerogpuEscape(f, hAdapter, &qe, sizeof(qe));
  if (NT_SUCCESS(stErr)) {
    bool supported = true;
    if ((qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID) != 0) {
      supported = (qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED) != 0;
    }
    if (supported) {
      haveError = true;
    }
  }

  wprintf(L"Perf counters (snapshot):\n");
  wprintf(L"  fences: submitted=0x%I64x completed=0x%I64x pending=%I64u\n",
          (unsigned long long)submitted,
          (unsigned long long)completed,
          (unsigned long long)pendingFences);
  wprintf(L"  ring0:  head=%lu tail=%lu pending=%lu entry_count=%lu size_bytes=%lu\n",
          (unsigned long)q.ring0_head,
          (unsigned long)q.ring0_tail,
          (unsigned long)ringPending,
          (unsigned long)q.ring0_entry_count,
          (unsigned long)q.ring0_size_bytes);
  wprintf(L"  submits: total=%I64u render=%I64u present=%I64u internal=%I64u\n",
          (unsigned long long)q.total_submissions,
          (unsigned long long)q.total_render_submits,
          (unsigned long long)q.total_presents,
          (unsigned long long)q.total_internal_submits);
  wprintf(L"  irqs: fence=%I64u vblank=%I64u spurious=%I64u\n",
          (unsigned long long)q.irq_fence_delivered,
          (unsigned long long)q.irq_vblank_delivered,
          (unsigned long long)q.irq_spurious);
  const bool havePerfErrorIrq =
      (q.hdr.size >= offsetof(aerogpu_escape_query_perf_out, last_error_fence) + sizeof(q.last_error_fence));
  if (havePerfErrorIrq) {
    wprintf(L"  irq_error: count=%I64u last_fence=0x%I64x\n",
            (unsigned long long)q.error_irq_count,
            (unsigned long long)q.last_error_fence);
  } else {
    // Backward compatibility: older KMD builds may not include the appended error IRQ fields
    // in QUERY_PERF; fall back to QUERY_FENCE if available.
    aerogpu_escape_query_fence_out qf;
    ZeroMemory(&qf, sizeof(qf));
    qf.hdr.version = AEROGPU_ESCAPE_VERSION;
    qf.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    qf.hdr.size = sizeof(qf);
    qf.hdr.reserved0 = 0;
    NTSTATUS stFence = SendAerogpuEscape(f, hAdapter, &qf, sizeof(qf));
    if (NT_SUCCESS(stFence)) {
      wprintf(L"  irq_error: count=%I64u last_fence=0x%I64x\n",
              (unsigned long long)qf.error_irq_count,
              (unsigned long long)qf.last_error_fence);
    }
  }
  if (haveError) {
    wprintf(L"  error: code=%lu (%s) fence=0x%I64x count=%lu\n",
            (unsigned long)qe.error_code,
            AerogpuErrorCodeName(qe.error_code),
            (unsigned long long)qe.error_fence,
            (unsigned long)qe.error_count);
  }
  wprintf(L"  resets: ResetFromTimeout=%I64u last_reset_time_100ns=%I64u\n",
          (unsigned long long)q.reset_from_timeout_count,
          (unsigned long long)q.last_reset_time_100ns);

  const bool errorLatched = (q.reserved0 & 0x80000000u) != 0;
  const uint32_t lastErrorTime10ms = (q.reserved0 & 0x7FFFFFFFu);
  wprintf(L"  device_error: latched=%s last_time_10ms=%lu\n",
          errorLatched ? L"true" : L"false",
          (unsigned long)lastErrorTime10ms);

  wprintf(L"  vblank: seq=0x%I64x last_time_ns=0x%I64x period_ns=%lu\n",
          (unsigned long long)q.vblank_seq,
          (unsigned long long)q.last_vblank_time_ns,
          (unsigned long)q.vblank_period_ns);

  wprintf(L"Raw:\n");
  wprintf(L"  last_submitted_fence=%I64u\n", (unsigned long long)q.last_submitted_fence);
  wprintf(L"  last_completed_fence=%I64u\n", (unsigned long long)q.last_completed_fence);
  wprintf(L"  ring0_head=%lu\n", (unsigned long)q.ring0_head);
  wprintf(L"  ring0_tail=%lu\n", (unsigned long)q.ring0_tail);
  wprintf(L"  ring0_size_bytes=%lu\n", (unsigned long)q.ring0_size_bytes);
  wprintf(L"  ring0_entry_count=%lu\n", (unsigned long)q.ring0_entry_count);
  wprintf(L"  total_submissions=%I64u\n", (unsigned long long)q.total_submissions);
  wprintf(L"  total_presents=%I64u\n", (unsigned long long)q.total_presents);
  wprintf(L"  total_render_submits=%I64u\n", (unsigned long long)q.total_render_submits);
  wprintf(L"  total_internal_submits=%I64u\n", (unsigned long long)q.total_internal_submits);
  wprintf(L"  irq_fence_delivered=%I64u\n", (unsigned long long)q.irq_fence_delivered);
  wprintf(L"  irq_vblank_delivered=%I64u\n", (unsigned long long)q.irq_vblank_delivered);
  wprintf(L"  irq_spurious=%I64u\n", (unsigned long long)q.irq_spurious);
  if (q.hdr.size >= offsetof(aerogpu_escape_query_perf_out, last_error_fence) + sizeof(q.last_error_fence)) {
    wprintf(L"  error_irq_count=%I64u\n", (unsigned long long)q.error_irq_count);
    wprintf(L"  last_error_fence=%I64u\n", (unsigned long long)q.last_error_fence);
  }
  wprintf(L"  reset_from_timeout_count=%I64u\n", (unsigned long long)q.reset_from_timeout_count);
  wprintf(L"  last_reset_time_100ns=%I64u\n", (unsigned long long)q.last_reset_time_100ns);
  wprintf(L"  reserved0=0x%08lx\n", (unsigned long)q.reserved0);
  wprintf(L"  vblank_seq=%I64u\n", (unsigned long long)q.vblank_seq);
  wprintf(L"  last_vblank_time_ns=%I64u\n", (unsigned long long)q.last_vblank_time_ns);
  wprintf(L"  vblank_period_ns=%lu\n", (unsigned long)q.vblank_period_ns);
  if (haveError) {
    wprintf(L"  error_code=%lu\n", (unsigned long)qe.error_code);
    wprintf(L"  error_fence=%I64u\n", (unsigned long long)qe.error_fence);
    wprintf(L"  error_count=%lu\n", (unsigned long)qe.error_count);
  }

  return 0;
}

static int DoQueryScanout(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId) {
  const auto QueryScanout = [&](uint32_t requestedVidpnSourceId, aerogpu_escape_query_scanout_out *out) -> bool {
    ZeroMemory(out, sizeof(*out));
    out->hdr.version = AEROGPU_ESCAPE_VERSION;
    out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    out->hdr.size = sizeof(*out);
    out->hdr.reserved0 = 0;
    out->vidpn_source_id = requestedVidpnSourceId;

    NTSTATUS st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
    if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && requestedVidpnSourceId != 0) {
      // Older KMDs may only support source 0; retry.
      ZeroMemory(out, sizeof(*out));
      out->hdr.version = AEROGPU_ESCAPE_VERSION;
      out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
      out->hdr.size = sizeof(*out);
      out->hdr.reserved0 = 0;
      out->vidpn_source_id = 0;
      st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
    }
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, st);
      return false;
    }
    return true;
  };

  aerogpu_escape_query_scanout_out q;
  if (!QueryScanout(vidpnSourceId, &q)) {
    return 2;
  }

  wprintf(L"Scanout%lu:\n", (unsigned long)q.vidpn_source_id);
  wprintf(L"  cached: enable=%lu width=%lu height=%lu format=%S pitch=%lu\n",
          (unsigned long)q.cached_enable,
          (unsigned long)q.cached_width,
          (unsigned long)q.cached_height,
          AerogpuFormatName(q.cached_format),
          (unsigned long)q.cached_pitch_bytes);
  wprintf(L"  mmio:   enable=%lu width=%lu height=%lu format=%S pitch=%lu fb_gpa=0x%I64x\n",
          (unsigned long)q.mmio_enable,
          (unsigned long)q.mmio_width,
          (unsigned long)q.mmio_height,
          AerogpuFormatName(q.mmio_format),
          (unsigned long)q.mmio_pitch_bytes,
           (unsigned long long)q.mmio_fb_gpa);
  return 0;
}

static int DoQueryCursor(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"Cursor: (not supported)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(query-cursor) failed", f, st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }

  if (!supported) {
    wprintf(L"Cursor: (not supported)\n");
    return 2;
  }

  const int32_t x = (int32_t)q.x;
  const int32_t y = (int32_t)q.y;
  wprintf(L"Cursor: enable=%lu pos=(%ld,%ld) hot=(%lu,%lu) size=%lux%lu format=%S pitch=%lu fb_gpa=0x%I64x\n",
          (unsigned long)q.enable,
          (long)x,
          (long)y,
          (unsigned long)q.hot_x,
          (unsigned long)q.hot_y,
          (unsigned long)q.width,
          (unsigned long)q.height,
          AerogpuFormatName(q.format),
          (unsigned long)q.pitch_bytes,
          (unsigned long long)q.fb_gpa);
  return 0;
}

static bool WriteCreateAllocationCsv(const wchar_t *path, const aerogpu_escape_dump_createallocation_inout &q) {
  if (!path) {
    return false;
  }

  FILE *fp = NULL;
  errno_t ferr = _wfopen_s(&fp, path, L"w");
  if (ferr != 0 || !fp) {
    fwprintf(stderr, L"Failed to open CSV file for writing: %s (errno=%d)\n", path, (int)ferr);
    return false;
  }

  // Stable, machine-parseable header row.
  fprintf(fp,
          "write_index,entry_count,entry_capacity,seq,call_seq,alloc_index,num_allocations,create_flags,alloc_id,"
          "priv_flags,pitch_bytes,share_token,size_bytes,flags_in,flags_out\n");

  for (uint32_t i = 0; i < q.entry_count && i < q.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    fprintf(fp,
            "%lu,%lu,%lu,%lu,%lu,%lu,%lu,0x%08lx,%lu,0x%08lx,%lu,0x%016I64x,%I64u,0x%08lx,0x%08lx\n",
            (unsigned long)q.write_index,
            (unsigned long)q.entry_count,
            (unsigned long)q.entry_capacity,
            (unsigned long)e.seq,
            (unsigned long)e.call_seq,
            (unsigned long)e.alloc_index,
            (unsigned long)e.num_allocations,
            (unsigned long)e.create_flags,
            (unsigned long)e.alloc_id,
            (unsigned long)e.priv_flags,
            (unsigned long)e.pitch_bytes,
            (unsigned long long)e.share_token,
            (unsigned long long)e.size_bytes,
            (unsigned long)e.flags_in,
            (unsigned long)e.flags_out);
  }

  fclose(fp);
  return true;
}

static bool WriteCreateAllocationJson(const wchar_t *path, const aerogpu_escape_dump_createallocation_inout &q) {
  if (!path) {
    return false;
  }

  FILE *fp = NULL;
  errno_t ferr = _wfopen_s(&fp, path, L"w");
  if (ferr != 0 || !fp) {
    fwprintf(stderr, L"Failed to open JSON file for writing: %s (errno=%d)\n", path, (int)ferr);
    return false;
  }

  const uint32_t n = (q.entry_count < q.entry_capacity) ? q.entry_count : q.entry_capacity;
  const uint32_t count = (n < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS) ? n : AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;

  // Stable, machine-parseable JSON document.
  fprintf(fp, "{\n");
  fprintf(fp, "  \"schema_version\": 1,\n");
  fprintf(fp, "  \"write_index\": %lu,\n", (unsigned long)q.write_index);
  fprintf(fp, "  \"entry_capacity\": %lu,\n", (unsigned long)q.entry_capacity);
  fprintf(fp, "  \"entries\": [\n");
  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    const char *comma = (i + 1 < count) ? "," : "";
    fprintf(fp, "    {\n");
    fprintf(fp, "      \"seq\": %lu,\n", (unsigned long)e.seq);
    fprintf(fp, "      \"call_seq\": %lu,\n", (unsigned long)e.call_seq);
    fprintf(fp, "      \"alloc_index\": %lu,\n", (unsigned long)e.alloc_index);
    fprintf(fp, "      \"num_allocations\": %lu,\n", (unsigned long)e.num_allocations);
    fprintf(fp, "      \"create_flags\": \"0x%08lx\",\n", (unsigned long)e.create_flags);
    fprintf(fp, "      \"alloc_id\": %lu,\n", (unsigned long)e.alloc_id);
    fprintf(fp, "      \"priv_flags\": \"0x%08lx\",\n", (unsigned long)e.priv_flags);
    fprintf(fp, "      \"pitch_bytes\": %lu,\n", (unsigned long)e.pitch_bytes);
    fprintf(fp, "      \"share_token\": \"0x%016I64x\",\n", (unsigned long long)e.share_token);
    fprintf(fp, "      \"size_bytes\": \"%I64u\",\n", (unsigned long long)e.size_bytes);
    fprintf(fp, "      \"flags_in\": \"0x%08lx\",\n", (unsigned long)e.flags_in);
    fprintf(fp, "      \"flags_out\": \"0x%08lx\"\n", (unsigned long)e.flags_out);
    fprintf(fp, "    }%s\n", comma);
  }
  fprintf(fp, "  ]\n");
  fprintf(fp, "}\n");

  fclose(fp);
  return true;
}

static NTSTATUS ReadGpa(const D3DKMT_FUNCS *f,
                        D3DKMT_HANDLE hAdapter,
                        uint64_t gpa,
                        void *dst,
                        uint32_t sizeBytes,
                        uint8_t *escapeBuf,
                        uint32_t escapeBufCapacity) {
  if (!dst || sizeBytes == 0 || !escapeBuf) {
    return STATUS_INVALID_PARAMETER;
  }

  if (sizeBytes > AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) {
    return STATUS_INVALID_PARAMETER;
  }
  if (escapeBufCapacity < (uint32_t)sizeof(aerogpu_escape_read_gpa_inout)) {
    return STATUS_BUFFER_TOO_SMALL;
  }

  aerogpu_escape_read_gpa_inout *io = (aerogpu_escape_read_gpa_inout *)escapeBuf;
  ZeroMemory(io, sizeof(*io));

  io->hdr.version = AEROGPU_ESCAPE_VERSION;
  io->hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
  io->hdr.size = sizeof(*io);
  io->hdr.reserved0 = 0;
  io->gpa = (aerogpu_escape_u64)gpa;
  io->size_bytes = (aerogpu_escape_u32)sizeBytes;
  io->reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscapeDirect(f, hAdapter, io, io->hdr.size);
  if (!NT_SUCCESS(st)) {
    return st;
  }

  // Defensive validation: if the op is wrong (or the KMD returned a different packet),
  // don't silently treat whatever happens to be in the buffer as framebuffer bytes.
  if (io->hdr.op != AEROGPU_ESCAPE_OP_READ_GPA || io->hdr.size != sizeof(*io) ||
      io->size_bytes != (aerogpu_escape_u32)sizeBytes) {
    return STATUS_INVALID_PARAMETER;
  }

  const NTSTATUS op = (NTSTATUS)io->status;
  uint32_t copied = io->bytes_copied;
  if (copied > sizeBytes) {
    copied = sizeBytes;
  }
  if (copied != 0) {
    memcpy(dst, io->data, copied);
  }

  // For this helper (used by --dump-scanout-bmp/--dump-cursor-bmp), we expect full reads; treat any truncation as failure.
  if (NT_SUCCESS(op) && copied != sizeBytes) {
    return STATUS_PARTIAL_COPY;
  }
  return op;
}

static int DumpLinearFramebufferToBmp(const D3DKMT_FUNCS *f,
                                      D3DKMT_HANDLE hAdapter,
                                      const wchar_t *label,
                                      uint32_t width,
                                      uint32_t height,
                                      uint32_t format,
                                      uint32_t pitchBytes,
                                      uint64_t fbGpa,
                                      const wchar_t *path,
                                      bool quiet = false) {
  if (!f || !f->Escape || !hAdapter || !label || !path) {
    return 2;
  }

  uint32_t srcBpp = 0;
  switch ((enum aerogpu_format)format) {
  case AEROGPU_FORMAT_B8G8R8A8_UNORM:
  case AEROGPU_FORMAT_B8G8R8X8_UNORM:
  case AEROGPU_FORMAT_R8G8B8A8_UNORM:
  case AEROGPU_FORMAT_R8G8B8X8_UNORM:
  case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
  case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
  case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
  case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
    srcBpp = 4;
    break;
  case AEROGPU_FORMAT_B5G6R5_UNORM:
  case AEROGPU_FORMAT_B5G5R5A1_UNORM:
    srcBpp = 2;
    break;
  default:
    fwprintf(stderr, L"%s: unsupported format: %S (%lu)\n",
             label,
             AerogpuFormatName(format),
             (unsigned long)format);
    return 2;
  }

  // Validate row byte sizes and BMP file size (avoid overflows and surprising huge dumps).
  uint64_t rowSrcBytes64 = 0;
  if (!MulU64((uint64_t)width, (uint64_t)srcBpp, &rowSrcBytes64) || rowSrcBytes64 == 0) {
    fwprintf(stderr, L"%s: invalid width/bpp combination: width=%lu bpp=%lu\n",
             label,
             (unsigned long)width,
             (unsigned long)srcBpp);
    return 2;
  }
  if ((uint64_t)pitchBytes < rowSrcBytes64) {
    fwprintf(stderr,
             L"%s: invalid pitch (pitch=%lu < row_bytes=%I64u)\n",
             label,
             (unsigned long)pitchBytes,
             (unsigned long long)rowSrcBytes64);
    return 2;
  }

  uint64_t rowOutBytes64 = 0;
  if (!MulU64((uint64_t)width, 4ull, &rowOutBytes64) || rowOutBytes64 == 0) {
    fwprintf(stderr, L"%s: invalid width for BMP output: width=%lu\n", label, (unsigned long)width);
    return 2;
  }
  uint64_t imageBytes64 = 0;
  if (!MulU64(rowOutBytes64, (uint64_t)height, &imageBytes64)) {
    fwprintf(stderr, L"%s: image size overflow: %lux%lu\n", label, (unsigned long)width, (unsigned long)height);
    return 2;
  }

  // Refuse absurdly large dumps (debug tool safety).
  const uint64_t kMaxImageBytes = 512ull * 1024ull * 1024ull; // 512 MiB
  if (imageBytes64 > kMaxImageBytes) {
    fwprintf(stderr,
             L"%s: refusing to dump %I64u bytes (%lux%lu) to BMP (limit %I64u MiB)\n",
             label,
             (unsigned long long)imageBytes64,
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long long)(kMaxImageBytes / (1024ull * 1024ull)));
    return 2;
  }

  if (width > 0x7FFFFFFFu || height > 0x7FFFFFFFu) {
    fwprintf(stderr, L"%s: refusing to dump: width/height exceed BMP limits (%lux%lu)\n",
             label,
             (unsigned long)width,
             (unsigned long)height);
    return 2;
  }

  const uint64_t headerBytes64 = (uint64_t)sizeof(bmp_file_header) + (uint64_t)sizeof(bmp_info_header);
  uint64_t fileBytes64 = 0;
  if (!AddU64(headerBytes64, imageBytes64, &fileBytes64) || fileBytes64 > 0xFFFFFFFFull) {
    fwprintf(stderr, L"%s: BMP size overflow: %I64u bytes\n", label, (unsigned long long)fileBytes64);
    return 2;
  }

  FILE *fp = NULL;
  errno_t ferr = _wfopen_s(&fp, path, L"wb");
  if (ferr != 0 || !fp) {
    fwprintf(stderr, L"%s: failed to open output file: %s (errno=%d)\n", label, path, (int)ferr);
    return 2;
  }

  bmp_file_header fh;
  ZeroMemory(&fh, sizeof(fh));
  fh.bfType = 0x4D42u; /* 'BM' */
  fh.bfSize = (uint32_t)fileBytes64;
  fh.bfReserved1 = 0;
  fh.bfReserved2 = 0;
  fh.bfOffBits = (uint32_t)headerBytes64;

  bmp_info_header ih;
  ZeroMemory(&ih, sizeof(ih));
  ih.biSize = sizeof(bmp_info_header);
  ih.biWidth = (int32_t)width;
  ih.biHeight = (int32_t)height; /* bottom-up */
  ih.biPlanes = 1;
  ih.biBitCount = 32;
  ih.biCompression = 0; /* BI_RGB */
  ih.biSizeImage = (uint32_t)imageBytes64;
  ih.biXPelsPerMeter = 0;
  ih.biYPelsPerMeter = 0;
  ih.biClrUsed = 0;
  ih.biClrImportant = 0;

  if (fwrite(&fh, sizeof(fh), 1, fp) != 1 || fwrite(&ih, sizeof(ih), 1, fp) != 1) {
    fwprintf(stderr, L"%s: failed to write BMP header to %s\n", label, path);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  const uint64_t sizeMax = (uint64_t)(~(size_t)0);
  if (rowSrcBytes64 > sizeMax || rowOutBytes64 > sizeMax) {
    fwprintf(stderr, L"%s: refusing to dump: row buffers exceed addressable size\n", label);
    fclose(fp);
    _wremove(path);
    return 2;
  }
  const size_t rowSrcBytes = (size_t)rowSrcBytes64;
  const size_t rowOutBytes = (size_t)rowOutBytes64;

  uint8_t *rowSrc = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, rowSrcBytes);
  uint8_t *rowOut = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, rowOutBytes);
  if (!rowSrc || !rowOut) {
    fwprintf(stderr, L"%s: out of memory allocating row buffers (%Iu, %Iu bytes)\n", label, rowSrcBytes, rowOutBytes);
    if (rowSrc) HeapFree(GetProcessHeap(), 0, rowSrc);
    if (rowOut) HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  // Escape buffer for READ_GPA: reuse a single buffer to avoid per-chunk allocations.
  //
  // Note: AEROGPU_ESCAPE_OP_READ_GPA has a fixed max payload (`AEROGPU_DBGCTL_READ_GPA_MAX_BYTES`).
  const uint32_t maxReadChunk = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
  const uint32_t escapeBufCap = (uint32_t)sizeof(aerogpu_escape_read_gpa_inout);
  uint8_t *escapeBuf = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, (size_t)escapeBufCap);
  if (!escapeBuf) {
    fwprintf(stderr, L"%s: out of memory allocating escape buffer (%lu bytes)\n", label, (unsigned long)escapeBufCap);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  // Dump bottom-up BMP: write last row first.
  const int32_t h32 = (int32_t)height;
  for (int32_t y = h32 - 1; y >= 0; --y) {
    uint64_t rowGpa = 0;
    uint64_t rowOffset = 0;
    if (!MulU64((uint64_t)(uint32_t)y, (uint64_t)pitchBytes, &rowOffset) || !AddU64(fbGpa, rowOffset, &rowGpa)) {
      fwprintf(stderr, L"%s: GPA overflow computing row %ld address\n", label, (long)y);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }

    // Read row bytes in bounded chunks.
    size_t done = 0;
    while (done < rowSrcBytes) {
      const uint32_t remaining = (uint32_t)(rowSrcBytes - done);
      uint32_t chunk = (remaining < maxReadChunk) ? remaining : maxReadChunk;

      uint64_t chunkGpa = 0;
      if (!AddU64(rowGpa, (uint64_t)done, &chunkGpa)) {
        fwprintf(stderr, L"%s: GPA overflow computing read offset for row %ld\n", label, (long)y);
        HeapFree(GetProcessHeap(), 0, escapeBuf);
        HeapFree(GetProcessHeap(), 0, rowSrc);
        HeapFree(GetProcessHeap(), 0, rowOut);
        fclose(fp);
        _wremove(path);
        return 2;
      }

      const NTSTATUS rst = ReadGpa(f, hAdapter, chunkGpa, rowSrc + done, chunk, escapeBuf, escapeBufCap);
      if (!NT_SUCCESS(rst)) {
        PrintNtStatus(L"read-gpa failed", f, rst);
        if (rst == STATUS_NOT_SUPPORTED) {
          fwprintf(stderr, L"%s: hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n", label);
        }
        fwprintf(stderr, L"%s: failed to read row %ld (offset %Iu, size %lu)\n", label, (long)y, done, (unsigned long)chunk);
        HeapFree(GetProcessHeap(), 0, escapeBuf);
        HeapFree(GetProcessHeap(), 0, rowSrc);
        HeapFree(GetProcessHeap(), 0, rowOut);
        fclose(fp);
        _wremove(path);
        return 2;
      }
      done += (size_t)chunk;
    }

    // Convert to 32bpp BMP (BGRA). Preserve alpha when the source format has it.
    switch ((enum aerogpu_format)format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
        d[3] = s[3];
      }
      break;
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
        d[3] = 0xFFu;
      }
      break;
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
      }
      break;
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 0xFFu;
      }
      break;
    case AEROGPU_FORMAT_B5G6R5_UNORM: {
      const uint16_t *src16 = (const uint16_t *)rowSrc;
      for (uint32_t x = 0; x < width; ++x) {
        const uint16_t p = src16[x];
        const uint8_t b5 = (uint8_t)(p & 0x1Fu);
        const uint8_t g6 = (uint8_t)((p >> 5) & 0x3Fu);
        const uint8_t r5 = (uint8_t)((p >> 11) & 0x1Fu);
        const uint8_t b = (uint8_t)((b5 << 3) | (b5 >> 2));
        const uint8_t g = (uint8_t)((g6 << 2) | (g6 >> 4));
        const uint8_t r = (uint8_t)((r5 << 3) | (r5 >> 2));
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = b;
        d[1] = g;
        d[2] = r;
        d[3] = 0xFFu;
      }
      break;
    }
    case AEROGPU_FORMAT_B5G5R5A1_UNORM: {
      const uint16_t *src16 = (const uint16_t *)rowSrc;
      for (uint32_t x = 0; x < width; ++x) {
        const uint16_t p = src16[x];
        const uint8_t a1 = (uint8_t)((p >> 15) & 0x1u);
        const uint8_t b5 = (uint8_t)(p & 0x1Fu);
        const uint8_t g5 = (uint8_t)((p >> 5) & 0x1Fu);
        const uint8_t r5 = (uint8_t)((p >> 10) & 0x1Fu);
        const uint8_t b = (uint8_t)((b5 << 3) | (b5 >> 2));
        const uint8_t g = (uint8_t)((g5 << 3) | (g5 >> 2));
        const uint8_t r = (uint8_t)((r5 << 3) | (r5 >> 2));
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = b;
        d[1] = g;
        d[2] = r;
        d[3] = a1 ? 0xFFu : 0x00u;
      }
      break;
    }
    default:
      fwprintf(stderr, L"%s: unsupported format during conversion: %S (%lu)\n",
               label,
               AerogpuFormatName(format),
               (unsigned long)format);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }

    if (fwrite(rowOut, 1, rowOutBytes, fp) != rowOutBytes) {
      fwprintf(stderr, L"%s: failed to write BMP pixel data to %s\n", label, path);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }
  }

  HeapFree(GetProcessHeap(), 0, escapeBuf);
  HeapFree(GetProcessHeap(), 0, rowSrc);
  HeapFree(GetProcessHeap(), 0, rowOut);
  fclose(fp);

  if (!quiet) {
    wprintf(L"Wrote %s: %lux%lu format=%S pitch=%lu fb_gpa=0x%I64x -> %s\n",
            label,
            (unsigned long)width,
            (unsigned long)height,
            AerogpuFormatName(format),
            (unsigned long)pitchBytes,
            (unsigned long long)fbGpa,
            path);
  }
  return 0;
}

static int DumpLinearFramebufferToPng(const D3DKMT_FUNCS *f,
                                      D3DKMT_HANDLE hAdapter,
                                      const wchar_t *label,
                                      uint32_t width,
                                      uint32_t height,
                                      uint32_t format,
                                      uint32_t pitchBytes,
                                      uint64_t fbGpa,
                                      const wchar_t *path,
                                      bool quiet = false) {
  if (!f || !f->Escape || !hAdapter || !label || !path) {
    return 2;
  }

  uint32_t srcBpp = 0;
  switch ((enum aerogpu_format)format) {
  case AEROGPU_FORMAT_B8G8R8A8_UNORM:
  case AEROGPU_FORMAT_B8G8R8X8_UNORM:
  case AEROGPU_FORMAT_R8G8B8A8_UNORM:
  case AEROGPU_FORMAT_R8G8B8X8_UNORM:
  case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
  case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
  case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
  case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
    srcBpp = 4;
    break;
  case AEROGPU_FORMAT_B5G6R5_UNORM:
  case AEROGPU_FORMAT_B5G5R5A1_UNORM:
    srcBpp = 2;
    break;
  default:
    fwprintf(stderr, L"%s: unsupported format: %S (%lu)\n",
             label,
             AerogpuFormatName(format),
             (unsigned long)format);
    return 2;
  }

  // Validate row byte sizes and PNG size computations (avoid overflows / huge dumps).
  uint64_t rowSrcBytes64 = 0;
  if (!MulU64((uint64_t)width, (uint64_t)srcBpp, &rowSrcBytes64) || rowSrcBytes64 == 0) {
    fwprintf(stderr, L"%s: invalid width/bpp combination: width=%lu bpp=%lu\n",
             label,
             (unsigned long)width,
             (unsigned long)srcBpp);
    return 2;
  }
  if ((uint64_t)pitchBytes < rowSrcBytes64) {
    fwprintf(stderr,
             L"%s: invalid pitch (pitch=%lu < row_bytes=%I64u)\n",
             label,
             (unsigned long)pitchBytes,
             (unsigned long long)rowSrcBytes64);
    return 2;
  }

  uint64_t rowOutBytes64 = 0;
  if (!MulU64((uint64_t)width, 4ull, &rowOutBytes64) || rowOutBytes64 == 0) {
    fwprintf(stderr, L"%s: invalid width for PNG output: width=%lu\n", label, (unsigned long)width);
    return 2;
  }
  uint64_t imageBytes64 = 0;
  if (!MulU64(rowOutBytes64, (uint64_t)height, &imageBytes64)) {
    fwprintf(stderr, L"%s: image size overflow: %lux%lu\n", label, (unsigned long)width, (unsigned long)height);
    return 2;
  }

  // Refuse absurdly large dumps (debug tool safety).
  const uint64_t kMaxImageBytes = 512ull * 1024ull * 1024ull; // 512 MiB
  if (imageBytes64 > kMaxImageBytes) {
    fwprintf(stderr,
             L"%s: refusing to dump %I64u bytes (%lux%lu) to PNG (limit %I64u MiB)\n",
             label,
             (unsigned long long)imageBytes64,
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long long)(kMaxImageBytes / (1024ull * 1024ull)));
    return 2;
  }

  if (width == 0 || height == 0) {
    fwprintf(stderr, L"%s: invalid size %lux%lu\n", label, (unsigned long)width, (unsigned long)height);
    return 2;
  }

  // PNG stores scanlines as: [filter_byte][RGBA...].
  uint64_t rowRawBytes64 = 0;
  if (!AddU64(rowOutBytes64, 1ull, &rowRawBytes64)) {
    fwprintf(stderr, L"%s: row size overflow\n", label);
    return 2;
  }
  uint64_t rawBytes64 = 0;
  if (!MulU64(rowRawBytes64, (uint64_t)height, &rawBytes64)) {
    fwprintf(stderr, L"%s: raw image size overflow\n", label);
    return 2;
  }

  // zlib stream for IDAT: 2-byte header + N stored blocks + Adler32.
  const uint64_t kDeflateBlockMax = 65535ull;
  const uint64_t numBlocks = (rawBytes64 + (kDeflateBlockMax - 1ull)) / kDeflateBlockMax;
  uint64_t blockOverhead64 = 0;
  if (!MulU64(numBlocks, 5ull, &blockOverhead64)) {
    fwprintf(stderr, L"%s: deflate overhead overflow\n", label);
    return 2;
  }
  uint64_t zlibPayload64 = 0;
  if (!AddU64(rawBytes64, blockOverhead64, &zlibPayload64)) {
    fwprintf(stderr, L"%s: deflate payload overflow\n", label);
    return 2;
  }
  uint64_t idatLen64 = 0;
  // 2 bytes zlib header + payload + 4 bytes Adler32 footer.
  if (!AddU64(zlibPayload64, 6ull, &idatLen64) || idatLen64 > 0xFFFFFFFFull) {
    fwprintf(stderr, L"%s: refusing to dump: IDAT chunk too large (%I64u bytes)\n",
             label,
             (unsigned long long)idatLen64);
    return 2;
  }
  const uint32_t idatLen = (uint32_t)idatLen64;

  const uint64_t sizeMax = (uint64_t)(~(size_t)0);
  if (rowSrcBytes64 > sizeMax || rowOutBytes64 > sizeMax) {
    fwprintf(stderr, L"%s: refusing to dump: row buffers exceed addressable size\n", label);
    return 2;
  }
  const size_t rowSrcBytes = (size_t)rowSrcBytes64;
  const size_t rowOutBytes = (size_t)rowOutBytes64;

  FILE *fp = NULL;
  errno_t ferr = _wfopen_s(&fp, path, L"wb");
  if (ferr != 0 || !fp) {
    fwprintf(stderr, L"%s: failed to open output file: %s (errno=%d)\n", label, path, (int)ferr);
    return 2;
  }

  static const uint8_t kPngSig[8] = {0x89u, 'P', 'N', 'G', '\r', '\n', 0x1Au, '\n'};
  if (fwrite(kPngSig, 1, sizeof(kPngSig), fp) != sizeof(kPngSig)) {
    fwprintf(stderr, L"%s: failed to write PNG signature to %s\n", label, path);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  uint8_t ihdr[13];
  ihdr[0] = (uint8_t)((width >> 24) & 0xFFu);
  ihdr[1] = (uint8_t)((width >> 16) & 0xFFu);
  ihdr[2] = (uint8_t)((width >> 8) & 0xFFu);
  ihdr[3] = (uint8_t)(width & 0xFFu);
  ihdr[4] = (uint8_t)((height >> 24) & 0xFFu);
  ihdr[5] = (uint8_t)((height >> 16) & 0xFFu);
  ihdr[6] = (uint8_t)((height >> 8) & 0xFFu);
  ihdr[7] = (uint8_t)(height & 0xFFu);
  ihdr[8] = 8u;  // bit depth
  ihdr[9] = 6u;  // color type: RGBA
  ihdr[10] = 0u; // compression: deflate
  ihdr[11] = 0u; // filter: none
  ihdr[12] = 0u; // interlace: none

  if (!WritePngChunk(fp, "IHDR", ihdr, (uint32_t)sizeof(ihdr))) {
    fwprintf(stderr, L"%s: failed to write PNG IHDR chunk to %s\n", label, path);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  uint8_t *rowSrc = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, rowSrcBytes);
  uint8_t *rowOut = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, rowOutBytes);
  if (!rowSrc || !rowOut) {
    fwprintf(stderr, L"%s: out of memory allocating row buffers (%Iu, %Iu bytes)\n", label, rowSrcBytes, rowOutBytes);
    if (rowSrc) HeapFree(GetProcessHeap(), 0, rowSrc);
    if (rowOut) HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  // Escape buffer for READ_GPA: reuse a single buffer to avoid per-chunk allocations.
  const uint32_t maxReadChunk = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
  const uint32_t escapeBufCap = (uint32_t)sizeof(aerogpu_escape_read_gpa_inout);
  uint8_t *escapeBuf = (uint8_t *)HeapAlloc(GetProcessHeap(), 0, (size_t)escapeBufCap);
  if (!escapeBuf) {
    fwprintf(stderr, L"%s: out of memory allocating escape buffer (%lu bytes)\n", label, (unsigned long)escapeBufCap);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  // IDAT chunk: zlib stream using stored (uncompressed) deflate blocks.
  uint32_t idatCrc = 0;
  if (!WritePngChunkHeader(fp, "IDAT", idatLen, &idatCrc)) {
    fwprintf(stderr, L"%s: failed to start PNG IDAT chunk\n", label);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  const uint8_t zhdr[2] = {0x78u, 0x01u}; // CMF/FLG for deflate/no compression
  if (fwrite(zhdr, 1, sizeof(zhdr), fp) != sizeof(zhdr)) {
    fwprintf(stderr, L"%s: failed to write zlib header\n", label);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }
  idatCrc = PngCrc32Update(idatCrc, zhdr, sizeof(zhdr));

  uint64_t rawRemaining = rawBytes64;
  uint32_t blockRemaining = 0;
  uint32_t adler = 1u;

  const auto WriteRaw = [&](const void *data, uint32_t len) -> bool {
    const uint8_t *p = (const uint8_t *)data;
    uint32_t off = 0;
    while (off < len) {
      if (rawRemaining == 0) {
        return false;
      }
      if (blockRemaining == 0) {
        const uint32_t blkLen =
            (rawRemaining > kDeflateBlockMax) ? (uint32_t)kDeflateBlockMax : (uint32_t)rawRemaining;
        const uint8_t bfinal = (rawRemaining <= kDeflateBlockMax) ? 1u : 0u;
        const uint8_t hdr = bfinal; // BTYPE=00 (stored)

        if (fwrite(&hdr, 1, 1, fp) != 1) {
          return false;
        }
        idatCrc = PngCrc32Update(idatCrc, &hdr, 1);

        const uint16_t len16 = (uint16_t)blkLen;
        const uint16_t nlen16 = (uint16_t)(~len16);
        uint8_t le[4];
        le[0] = (uint8_t)(len16 & 0xFFu);
        le[1] = (uint8_t)((len16 >> 8) & 0xFFu);
        le[2] = (uint8_t)(nlen16 & 0xFFu);
        le[3] = (uint8_t)((nlen16 >> 8) & 0xFFu);
        if (fwrite(le, 1, sizeof(le), fp) != sizeof(le)) {
          return false;
        }
        idatCrc = PngCrc32Update(idatCrc, le, sizeof(le));
        blockRemaining = blkLen;
      }

      uint32_t chunk = len - off;
      if (chunk > blockRemaining) {
        chunk = blockRemaining;
      }

      if (fwrite(p + off, 1, chunk, fp) != chunk) {
        return false;
      }
      idatCrc = PngCrc32Update(idatCrc, p + off, chunk);
      adler = PngAdler32Update(adler, p + off, chunk);

      off += chunk;
      blockRemaining -= chunk;
      rawRemaining -= (uint64_t)chunk;
    }
    return true;
  };

  // Write scanlines top-down.
  for (uint32_t y = 0; y < height; ++y) {
    uint64_t rowGpa = 0;
    uint64_t rowOffset = 0;
    if (!MulU64((uint64_t)y, (uint64_t)pitchBytes, &rowOffset) || !AddU64(fbGpa, rowOffset, &rowGpa)) {
      fwprintf(stderr, L"%s: GPA overflow computing row %lu address\n", label, (unsigned long)y);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }

    // Read row bytes in bounded chunks.
    size_t done = 0;
    while (done < rowSrcBytes) {
      const uint32_t remaining = (uint32_t)(rowSrcBytes - done);
      uint32_t chunk = (remaining < maxReadChunk) ? remaining : maxReadChunk;

      uint64_t chunkGpa = 0;
      if (!AddU64(rowGpa, (uint64_t)done, &chunkGpa)) {
        fwprintf(stderr, L"%s: GPA overflow computing read offset for row %lu\n", label, (unsigned long)y);
        HeapFree(GetProcessHeap(), 0, escapeBuf);
        HeapFree(GetProcessHeap(), 0, rowSrc);
        HeapFree(GetProcessHeap(), 0, rowOut);
        fclose(fp);
        _wremove(path);
        return 2;
      }

      const NTSTATUS rst = ReadGpa(f, hAdapter, chunkGpa, rowSrc + done, chunk, escapeBuf, escapeBufCap);
      if (!NT_SUCCESS(rst)) {
        PrintNtStatus(L"read-gpa failed", f, rst);
        if (rst == STATUS_NOT_SUPPORTED) {
          fwprintf(stderr, L"%s: hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n", label);
        }
        fwprintf(stderr,
                 L"%s: failed to read row %lu (offset %Iu, size %lu)\n",
                 label,
                 (unsigned long)y,
                 done,
                 (unsigned long)chunk);
        HeapFree(GetProcessHeap(), 0, escapeBuf);
        HeapFree(GetProcessHeap(), 0, rowSrc);
        HeapFree(GetProcessHeap(), 0, rowOut);
        fclose(fp);
        _wremove(path);
        return 2;
      }
      done += (size_t)chunk;
    }

    // Convert to 32bpp RGBA8.
    switch ((enum aerogpu_format)format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = s[3];
      }
      break;
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 0xFFu;
      }
      break;
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
        d[3] = s[3];
      }
      break;
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
      for (uint32_t x = 0; x < width; ++x) {
        const uint8_t *s = rowSrc + (size_t)x * 4u;
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = s[0];
        d[1] = s[1];
        d[2] = s[2];
        d[3] = 0xFFu;
      }
      break;
    case AEROGPU_FORMAT_B5G6R5_UNORM: {
      const uint16_t *src16 = (const uint16_t *)rowSrc;
      for (uint32_t x = 0; x < width; ++x) {
        const uint16_t p = src16[x];
        const uint8_t b5 = (uint8_t)(p & 0x1Fu);
        const uint8_t g6 = (uint8_t)((p >> 5) & 0x3Fu);
        const uint8_t r5 = (uint8_t)((p >> 11) & 0x1Fu);
        const uint8_t b = (uint8_t)((b5 << 3) | (b5 >> 2));
        const uint8_t g = (uint8_t)((g6 << 2) | (g6 >> 4));
        const uint8_t r = (uint8_t)((r5 << 3) | (r5 >> 2));
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = r;
        d[1] = g;
        d[2] = b;
        d[3] = 0xFFu;
      }
      break;
    }
    case AEROGPU_FORMAT_B5G5R5A1_UNORM: {
      const uint16_t *src16 = (const uint16_t *)rowSrc;
      for (uint32_t x = 0; x < width; ++x) {
        const uint16_t p = src16[x];
        const uint8_t a1 = (uint8_t)((p >> 15) & 0x1u);
        const uint8_t b5 = (uint8_t)(p & 0x1Fu);
        const uint8_t g5 = (uint8_t)((p >> 5) & 0x1Fu);
        const uint8_t r5 = (uint8_t)((p >> 10) & 0x1Fu);
        const uint8_t b = (uint8_t)((b5 << 3) | (b5 >> 2));
        const uint8_t g = (uint8_t)((g5 << 3) | (g5 >> 2));
        const uint8_t r = (uint8_t)((r5 << 3) | (r5 >> 2));
        uint8_t *d = rowOut + (size_t)x * 4u;
        d[0] = r;
        d[1] = g;
        d[2] = b;
        d[3] = a1 ? 0xFFu : 0x00u;
      }
      break;
    }
    default:
      fwprintf(stderr, L"%s: unsupported format during conversion: %S (%lu)\n",
               label,
               AerogpuFormatName(format),
               (unsigned long)format);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }

    const uint8_t filter = 0u;
    if (!WriteRaw(&filter, 1) || !WriteRaw(rowOut, (uint32_t)rowOutBytes)) {
      fwprintf(stderr, L"%s: failed to write PNG IDAT data\n", label);
      HeapFree(GetProcessHeap(), 0, escapeBuf);
      HeapFree(GetProcessHeap(), 0, rowSrc);
      HeapFree(GetProcessHeap(), 0, rowOut);
      fclose(fp);
      _wremove(path);
      return 2;
    }
  }

  if (rawRemaining != 0 || blockRemaining != 0) {
    fwprintf(stderr, L"%s: internal error: PNG writer rawRemaining=%I64u blockRemaining=%lu\n",
             label,
             (unsigned long long)rawRemaining,
             (unsigned long)blockRemaining);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  uint8_t adlerBe[4];
  adlerBe[0] = (uint8_t)((adler >> 24) & 0xFFu);
  adlerBe[1] = (uint8_t)((adler >> 16) & 0xFFu);
  adlerBe[2] = (uint8_t)((adler >> 8) & 0xFFu);
  adlerBe[3] = (uint8_t)(adler & 0xFFu);
  if (fwrite(adlerBe, 1, sizeof(adlerBe), fp) != sizeof(adlerBe)) {
    fwprintf(stderr, L"%s: failed to write PNG Adler32\n", label);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }
  idatCrc = PngCrc32Update(idatCrc, adlerBe, sizeof(adlerBe));

  if (!WritePngChunkCrc(fp, idatCrc)) {
    fwprintf(stderr, L"%s: failed to write PNG IDAT CRC\n", label);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  if (!WritePngChunk(fp, "IEND", "", 0)) {
    fwprintf(stderr, L"%s: failed to write PNG IEND chunk\n", label);
    HeapFree(GetProcessHeap(), 0, escapeBuf);
    HeapFree(GetProcessHeap(), 0, rowSrc);
    HeapFree(GetProcessHeap(), 0, rowOut);
    fclose(fp);
    _wremove(path);
    return 2;
  }

  HeapFree(GetProcessHeap(), 0, escapeBuf);
  HeapFree(GetProcessHeap(), 0, rowSrc);
  HeapFree(GetProcessHeap(), 0, rowOut);
  fclose(fp);

  if (!quiet) {
    wprintf(L"Wrote %s: %lux%lu format=%S pitch=%lu fb_gpa=0x%I64x -> %s\n",
            label,
            (unsigned long)width,
            (unsigned long)height,
            AerogpuFormatName(format),
            (unsigned long)pitchBytes,
            (unsigned long long)fbGpa,
            path);
  }
  return 0;
}

static int DoDumpScanoutBmp(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, const wchar_t *path) {
  if (!path || path[0] == 0) {
    fwprintf(stderr, L"--dump-scanout-bmp requires a non-empty path\n");
    return 1;
  }

  // Query scanout state (MMIO snapshot preferred).
  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    // Older KMDs may only support source 0; retry.
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, st);
    return 2;
  }

  // Prefer MMIO snapshot values (these reflect what the device is actually using).
  const uint32_t enable = (q.mmio_enable != 0) ? q.mmio_enable : q.cached_enable;
  const uint32_t width = (q.mmio_width != 0) ? q.mmio_width : q.cached_width;
  const uint32_t height = (q.mmio_height != 0) ? q.mmio_height : q.cached_height;
  const uint32_t format = (q.mmio_format != 0) ? q.mmio_format : q.cached_format;
  const uint32_t pitchBytes = (q.mmio_pitch_bytes != 0) ? q.mmio_pitch_bytes : q.cached_pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.mmio_fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    fwprintf(stderr,
             L"Scanout%lu: invalid mode (enable=%lu width=%lu height=%lu pitch=%lu)\n",
             (unsigned long)q.vidpn_source_id,
             (unsigned long)enable,
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long)pitchBytes);
    fwprintf(stderr, L"Hint: run --query-scanout to inspect cached vs MMIO values.\n");
    return 2;
  }

  if (fbGpa == 0) {
    fwprintf(stderr, L"Scanout%lu: MMIO framebuffer GPA is 0; cannot dump framebuffer.\n",
             (unsigned long)q.vidpn_source_id);
    fwprintf(stderr, L"Hint: ensure the installed KMD supports scanout registers (and AEROGPU_ESCAPE_OP_QUERY_SCANOUT).\n");
    return 2;
  }

  wchar_t label[32];
  swprintf_s(label, sizeof(label) / sizeof(label[0]), L"scanout%lu", (unsigned long)q.vidpn_source_id);
  return DumpLinearFramebufferToBmp(f, hAdapter, label, width, height, format, pitchBytes, fbGpa, path);
}

static int DoDumpScanoutPng(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, const wchar_t *path) {
  if (!path || path[0] == 0) {
    fwprintf(stderr, L"--dump-scanout-png requires a non-empty path\n");
    return 1;
  }

  // Query scanout state (MMIO snapshot preferred).
  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    // Older KMDs may only support source 0; retry.
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(query-scanout) failed", f, st);
    return 2;
  }

  // Prefer MMIO snapshot values (these reflect what the device is actually using).
  const uint32_t enable = (q.mmio_enable != 0) ? q.mmio_enable : q.cached_enable;
  const uint32_t width = (q.mmio_width != 0) ? q.mmio_width : q.cached_width;
  const uint32_t height = (q.mmio_height != 0) ? q.mmio_height : q.cached_height;
  const uint32_t format = (q.mmio_format != 0) ? q.mmio_format : q.cached_format;
  const uint32_t pitchBytes = (q.mmio_pitch_bytes != 0) ? q.mmio_pitch_bytes : q.cached_pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.mmio_fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    fwprintf(stderr,
             L"Scanout%lu: invalid mode (enable=%lu width=%lu height=%lu pitch=%lu)\n",
             (unsigned long)q.vidpn_source_id,
             (unsigned long)enable,
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long)pitchBytes);
    fwprintf(stderr, L"Hint: run --query-scanout to inspect cached vs MMIO values.\n");
    return 2;
  }

  if (fbGpa == 0) {
    fwprintf(stderr, L"Scanout%lu: MMIO framebuffer GPA is 0; cannot dump framebuffer.\n",
             (unsigned long)q.vidpn_source_id);
    fwprintf(stderr, L"Hint: ensure the installed KMD supports scanout registers (and AEROGPU_ESCAPE_OP_QUERY_SCANOUT).\n");
    return 2;
  }

  wchar_t label[32];
  swprintf_s(label, sizeof(label) / sizeof(label[0]), L"scanout%lu", (unsigned long)q.vidpn_source_id);
  return DumpLinearFramebufferToPng(f, hAdapter, label, width, height, format, pitchBytes, fbGpa, path);
}

static int DoDumpCursorBmp(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, const wchar_t *path) {
  if (!path || path[0] == 0) {
    fwprintf(stderr, L"--dump-cursor-bmp requires a non-empty path\n");
    return 1;
  }

  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"Cursor: (not supported)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(query-cursor) failed", f, st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }
  if (!supported) {
    wprintf(L"Cursor: (not supported)\n");
    return 2;
  }

  const uint32_t width = (uint32_t)q.width;
  const uint32_t height = (uint32_t)q.height;
  const uint32_t format = (uint32_t)q.format;
  const uint32_t pitchBytes = (uint32_t)q.pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    fwprintf(stderr, L"Cursor: invalid mode (width=%lu height=%lu pitch=%lu)\n",
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long)pitchBytes);
    fwprintf(stderr, L"Hint: run --query-cursor to inspect cursor MMIO state.\n");
    return 2;
  }

  if (fbGpa == 0) {
    fwprintf(stderr, L"Cursor: framebuffer GPA is 0; cannot dump cursor.\n");
    fwprintf(stderr, L"Hint: run --query-cursor to inspect cursor MMIO state.\n");
    return 2;
  }

  return DumpLinearFramebufferToBmp(f, hAdapter, L"cursor", width, height, format, pitchBytes, fbGpa, path);
}

static int DoDumpCursorPng(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, const wchar_t *path) {
  if (!path || path[0] == 0) {
    fwprintf(stderr, L"--dump-cursor-png requires a non-empty path\n");
    return 1;
  }

  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"Cursor: (not supported)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(query-cursor) failed", f, st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }
  if (!supported) {
    wprintf(L"Cursor: (not supported)\n");
    return 2;
  }

  const uint32_t width = (uint32_t)q.width;
  const uint32_t height = (uint32_t)q.height;
  const uint32_t format = (uint32_t)q.format;
  const uint32_t pitchBytes = (uint32_t)q.pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    fwprintf(stderr, L"Cursor: invalid mode (width=%lu height=%lu pitch=%lu)\n",
             (unsigned long)width,
             (unsigned long)height,
             (unsigned long)pitchBytes);
    fwprintf(stderr, L"Hint: run --query-cursor to inspect cursor MMIO state.\n");
    return 2;
  }

  if (fbGpa == 0) {
    fwprintf(stderr, L"Cursor: framebuffer GPA is 0; cannot dump cursor.\n");
    fwprintf(stderr, L"Hint: run --query-cursor to inspect cursor MMIO state.\n");
    return 2;
  }

  return DumpLinearFramebufferToPng(f, hAdapter, L"cursor", width, height, format, pitchBytes, fbGpa, path);
}

static int DoDumpCreateAllocation(const D3DKMT_FUNCS *f,
                                  D3DKMT_HANDLE hAdapter,
                                  const wchar_t *csvPath,
                                  const wchar_t *jsonPath) {
  aerogpu_escape_dump_createallocation_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.write_index = 0;
  q.entry_count = 0;
  q.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
  q.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    if (st == STATUS_NOT_SUPPORTED) {
      wprintf(L"CreateAllocation trace: (not supported)\n");
      return 2;
    }
    PrintNtStatus(L"D3DKMTEscape(dump-createalloc) failed", f, st);
    return 2;
  }

  if (csvPath || jsonPath) {
    if (csvPath && !WriteCreateAllocationCsv(csvPath, q)) {
      return 2;
    }
    if (jsonPath && !WriteCreateAllocationJson(jsonPath, q)) {
      return 2;
    }

    wprintf(L"CreateAllocation trace: write_index=%lu entry_count=%lu entry_capacity=%lu\n",
            (unsigned long)q.write_index,
            (unsigned long)q.entry_count,
            (unsigned long)q.entry_capacity);
    if (csvPath) {
      wprintf(L"Wrote CSV: %s\n", csvPath);
    }
    if (jsonPath) {
      wprintf(L"Wrote JSON: %s\n", jsonPath);
    }
    return 0;
  }

  wprintf(L"CreateAllocation trace:\n");
  wprintf(L"  write_index=%lu entry_count=%lu entry_capacity=%lu\n", (unsigned long)q.write_index,
          (unsigned long)q.entry_count, (unsigned long)q.entry_capacity);
  for (uint32_t i = 0; i < q.entry_count && i < q.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    wprintf(L"  [%lu] seq=%lu call=%lu create_flags=0x%08lx alloc[%lu/%lu] alloc_id=%lu share_token=0x%I64x size=%I64u priv_flags=0x%08lx pitch=%lu flags=0x%08lx->0x%08lx\n",
            (unsigned long)i,
            (unsigned long)e.seq,
            (unsigned long)e.call_seq,
            (unsigned long)e.create_flags,
            (unsigned long)e.alloc_index,
            (unsigned long)e.num_allocations,
            (unsigned long)e.alloc_id,
            (unsigned long long)e.share_token,
            (unsigned long long)e.size_bytes,
            (unsigned long)e.priv_flags,
            (unsigned long)e.pitch_bytes,
            (unsigned long)e.flags_in,
            (unsigned long)e.flags_out);
  }
  return 0;
}

static int DoMapSharedHandle(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint64_t sharedHandle) {
  aerogpu_escape_map_shared_handle_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.shared_handle = sharedHandle;
  q.debug_token = 0;
  q.reserved0 = 0;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(map-shared-handle) failed", f, st);
    return 2;
  }

  wprintf(L"debug_token: 0x%08lx (%lu)\n", (unsigned long)q.debug_token, (unsigned long)q.debug_token);
  return 0;
}

static int DoQueryUmdPrivate(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  if (!f->QueryAdapterInfo) {
    fwprintf(stderr, L"D3DKMTQueryAdapterInfo not available (missing gdi32 export)\n");
    return 1;
  }

  aerogpu_umd_private_v1 blob;
  ZeroMemory(&blob, sizeof(blob));

  // We intentionally avoid depending on WDK headers for the numeric
  // KMTQAITYPE_UMDRIVERPRIVATE constant. Instead, probe a small range of values
  // and look for a valid AeroGPU UMDRIVERPRIVATE v1 blob.
  UINT foundType = 0xFFFFFFFFu;
  NTSTATUS lastStatus = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&blob, sizeof(blob));
    NTSTATUS st = QueryAdapterInfoWithTimeout(f, hAdapter, type, &blob, sizeof(blob));
    lastStatus = st;
    if (!NT_SUCCESS(st)) {
      if (st == STATUS_TIMEOUT) {
        break;
      }
      continue;
    }

    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }

    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }

    foundType = type;
    break;
  }

  if (foundType == 0xFFFFFFFFu) {
    if (lastStatus == STATUS_TIMEOUT) {
      PrintNtStatus(L"D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) timed out", f, lastStatus);
      fwprintf(stderr, L"(note: timed out probing UMDRIVERPRIVATE; KMD may be wedged)\n");
    } else {
      PrintNtStatus(L"D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) failed", f, lastStatus);
      fwprintf(stderr, L"(note: UMDRIVERPRIVATE type probing range exhausted)\n");
    }
    return 2;
  }

  wchar_t magicStr[5] = {0, 0, 0, 0, 0};
  {
    const uint32_t m = blob.device_mmio_magic;
    magicStr[0] = (wchar_t)((m >> 0) & 0xFF);
    magicStr[1] = (wchar_t)((m >> 8) & 0xFF);
    magicStr[2] = (wchar_t)((m >> 16) & 0xFF);
    magicStr[3] = (wchar_t)((m >> 24) & 0xFF);
  }

  wprintf(L"UMDRIVERPRIVATE (type %lu)\n", (unsigned long)foundType);
  wprintf(L"  size_bytes: %lu\n", (unsigned long)blob.size_bytes);
  wprintf(L"  struct_version: %lu\n", (unsigned long)blob.struct_version);
  wprintf(L"  device_mmio_magic: 0x%08lx (%s)\n", (unsigned long)blob.device_mmio_magic, magicStr);

  const uint32_t abiMajor = (uint32_t)(blob.device_abi_version_u32 >> 16);
  const uint32_t abiMinor = (uint32_t)(blob.device_abi_version_u32 & 0xFFFFu);
  wprintf(L"  device_abi_version_u32: 0x%08lx (%lu.%lu)\n",
          (unsigned long)blob.device_abi_version_u32,
          (unsigned long)abiMajor,
          (unsigned long)abiMinor);

  wprintf(L"  device_features: 0x%I64x\n", (unsigned long long)blob.device_features);
  const std::wstring decoded_features = aerogpu::FormatDeviceFeatureBits(blob.device_features, 0);
  wprintf(L"  decoded_features: %s\n", decoded_features.c_str());
  wprintf(L"  flags: 0x%08lx\n", (unsigned long)blob.flags);
  wprintf(L"    is_legacy: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) != 0));
  wprintf(L"    has_vblank: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0));
  wprintf(L"    has_fence_page: %lu\n", (unsigned long)((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE) != 0));

  return 0;
}

static int DoQuerySegments(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter) {
  if (!f->QueryAdapterInfo) {
    fwprintf(stderr, L"D3DKMTQueryAdapterInfo not available (missing gdi32 export)\n");
    return 1;
  }

  DXGK_QUERYSEGMENTOUT *segments = NULL;
  if (!FindQuerySegmentTypeAndData(f, hAdapter, /*segmentCapacity=*/64, NULL, &segments, NULL)) {
    fwprintf(stderr, L"Failed to find a working KMTQAITYPE_QUERYSEGMENT value (probing range exhausted)\n");
    return 2;
  }

  wprintf(L"Segments (QuerySegment)\n");
  wprintf(L"  count: %lu\n", (unsigned long)segments->NbSegments);
  for (UINT i = 0; i < segments->NbSegments; ++i) {
    const DXGK_SEGMENTDESCRIPTOR &d = segments->pSegmentDescriptor[i];

    wprintf(L"  [%lu] size=", (unsigned long)i);
    PrintBytesAndMiB(d.Size);
    wprintf(L" flags=0x%08lx", (unsigned long)d.Flags.Value);

    wprintf(L" [");
    bool first = true;
    const auto Emit = [&](bool on, const wchar_t *name) {
      if (!on) {
        return;
      }
      if (!first) {
        wprintf(L"|");
      }
      wprintf(L"%s", name);
      first = false;
    };
    Emit(d.Flags.CpuVisible != 0, L"CpuVisible");
    Emit(d.Flags.Aperture != 0, L"Aperture");
    if (first) {
      wprintf(L"0");
    }
    wprintf(L"]");

    wprintf(L" group=%s\n", DxgkMemorySegmentGroupToString(d.MemorySegmentGroup));
  }

  DXGK_SEGMENTGROUPSIZE groupSizes;
  if (FindSegmentGroupSizeTypeAndData(f, hAdapter, segments, NULL, &groupSizes)) {
    wprintf(L"Segment group sizes (GetSegmentGroupSize)\n");
    wprintf(L"  LocalMemorySize: ");
    PrintBytesAndMiB(groupSizes.LocalMemorySize);
    wprintf(L"\n");
    wprintf(L"  NonLocalMemorySize: ");
    PrintBytesAndMiB(groupSizes.NonLocalMemorySize);
    wprintf(L"\n");
  } else {
    wprintf(L"Segment group sizes (GetSegmentGroupSize): (not available)\n");
  }

  HeapFree(GetProcessHeap(), 0, segments);
  return 0;
}

static int DoDumpRing(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId) {
  // Prefer the extended dump-ring packet (supports both legacy and new rings),
  // but fall back to the legacy format for older drivers.
  aerogpu_escape_dump_ring_v2_inout q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.ring_id = ringId;
  q2.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
  if (NT_SUCCESS(st)) {
    const wchar_t *fmt = L"unknown";
    switch (q2.ring_format) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      fmt = L"legacy";
      break;
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      fmt = L"agpu";
      break;
    default:
      fmt = L"unknown";
      break;
    }

    wprintf(L"Ring %lu (%s)\n", (unsigned long)q2.ring_id, fmt);
    wprintf(L"  size: %lu bytes\n", (unsigned long)q2.ring_size_bytes);
    wprintf(L"  head: 0x%08lx\n", (unsigned long)q2.head);
    wprintf(L"  tail: 0x%08lx\n", (unsigned long)q2.tail);
    if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
      wprintf(L"  descriptors (recent tail window): %lu\n", (unsigned long)q2.desc_count);
    } else {
      wprintf(L"  descriptors: %lu\n", (unsigned long)q2.desc_count);
    }

    uint32_t count = q2.desc_count;
    if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
    uint32_t window_start = 0;
    if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU && count != 0) {
      window_start = q2.tail - count;
    }

    for (uint32_t i = 0; i < count; ++i) {
      const aerogpu_dbgctl_ring_desc_v2 *d = &q2.desc[i];
      if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
        wprintf(L"    [%lu] ringIndex=%lu signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx allocTableGpa=0x%I64x allocTableBytes=%lu\n",
                (unsigned long)i, (unsigned long)(window_start + i), (unsigned long long)d->fence, (unsigned long long)d->cmd_gpa,
                (unsigned long)d->cmd_size_bytes, (unsigned long)d->flags,
                (unsigned long long)d->alloc_table_gpa, (unsigned long)d->alloc_table_size_bytes);
      } else {
        wprintf(L"    [%lu] signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx\n",
                (unsigned long)i, (unsigned long long)d->fence, (unsigned long long)d->cmd_gpa,
                (unsigned long)d->cmd_size_bytes, (unsigned long)d->flags);
      }
    }

    return 0;
  }

  aerogpu_escape_dump_ring_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.ring_id = ringId;
  q.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st);
    return 2;
  }

  wprintf(L"Ring %lu\n", (unsigned long)q.ring_id);
  wprintf(L"  size: %lu bytes\n", (unsigned long)q.ring_size_bytes);
  wprintf(L"  head: 0x%08lx\n", (unsigned long)q.head);
  wprintf(L"  tail: 0x%08lx\n", (unsigned long)q.tail);
  wprintf(L"  descriptors: %lu\n", (unsigned long)q.desc_count);

  uint32_t count = q.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }

  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_ring_desc *d = &q.desc[i];
    wprintf(L"    [%lu] signalFence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx\n", (unsigned long)i,
            (unsigned long long)d->signal_fence, (unsigned long long)d->cmd_gpa, (unsigned long)d->cmd_size_bytes,
            (unsigned long)d->flags);
  }

  return 0;
}

static int DoWatchRing(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId, uint32_t samples,
                       uint32_t intervalMs) {
  // Stall threshold: warn after ~2 seconds of no observed pending-count change while work is pending.
  static const uint32_t kStallWarnTimeMs = 2000;

  if (samples == 0 || intervalMs == 0) {
    fwprintf(stderr, L"--watch-ring requires --samples N and --interval-ms N\n");
    PrintUsage();
    return 1;
  }

  if (samples > 1000000u) {
    samples = 1000000u;
  }
  if (intervalMs > 60000u) {
    intervalMs = 60000u;
  }

  // sizeof(aerogpu_legacy_ring_entry) (see drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h).
  static const uint32_t kLegacyRingEntrySizeBytes = 24u;

  const auto RingFormatToString = [&](uint32_t fmt) -> const wchar_t * {
    switch (fmt) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      return L"legacy";
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      return L"agpu";
    default:
      return L"unknown";
    }
  };

  const auto TryComputeLegacyPending = [&](uint32_t ringSizeBytes, uint32_t head, uint32_t tail,
                                           uint64_t *pendingOut) -> bool {
    if (!pendingOut) {
      return false;
    }
    if (ringSizeBytes == 0 || (ringSizeBytes % kLegacyRingEntrySizeBytes) != 0) {
      return false;
    }
    const uint32_t entryCount = ringSizeBytes / kLegacyRingEntrySizeBytes;
    if (entryCount == 0 || head >= entryCount || tail >= entryCount) {
      return false;
    }
    if (tail >= head) {
      *pendingOut = (uint64_t)(tail - head);
    } else {
      *pendingOut = (uint64_t)(tail + entryCount - head);
    }
    return true;
  };

  wprintf(L"Watching ring %lu: samples=%lu interval_ms=%lu\n", (unsigned long)ringId, (unsigned long)samples,
          (unsigned long)intervalMs);

  bool decided = false;
  bool useV2 = false;
  uint32_t v2DescCapacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  bool havePrevPending = false;
  uint64_t prevPending = 0;
  uint32_t stallIntervals = 0;
  const uint32_t stallWarnIntervals = (intervalMs != 0) ? ((kStallWarnTimeMs + intervalMs - 1) / intervalMs) : 3;

  for (uint32_t i = 0; i < samples; ++i) {
    uint32_t head = 0;
    uint32_t tail = 0;
    uint64_t pending = 0;
    const wchar_t *fmtStr = L"unknown";

    bool haveLast = false;
    uint64_t lastFence = 0;
    uint32_t lastFlags = 0;

    if (!decided || useV2) {
      aerogpu_escape_dump_ring_v2_inout q2;
      ZeroMemory(&q2, sizeof(q2));
      q2.hdr.version = AEROGPU_ESCAPE_VERSION;
      q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
      q2.hdr.size = sizeof(q2);
      q2.hdr.reserved0 = 0;
      q2.ring_id = ringId;
      q2.desc_capacity = v2DescCapacity;

      NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
      if (NT_SUCCESS(st)) {
        decided = true;
        useV2 = true;

        head = q2.head;
        tail = q2.tail;
        fmtStr = RingFormatToString(q2.ring_format);

        if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
          // Monotonic indices (modulo u32 wrap).
          pending = (uint64_t)(uint32_t)(tail - head);

          // v2 AGPU dumps are a recent tail window; newest is last.
          if (q2.desc_count > 0 && q2.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
            const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
            lastFence = (uint64_t)d.fence;
            lastFlags = (uint32_t)d.flags;
            haveLast = true;
          }

          // For watch mode, only ask the KMD to return the newest descriptor.
          v2DescCapacity = 1;
        } else {
          // Legacy (masked indices) or unknown: compute pending best-effort using the legacy ring layout.
          if (!TryComputeLegacyPending(q2.ring_size_bytes, head, tail, &pending)) {
            pending = (uint64_t)(uint32_t)(tail - head);
          }

          // Only print the "last" descriptor if we know we captured the full pending region.
          if (pending != 0 && pending == (uint64_t)q2.desc_count && q2.desc_count > 0 &&
              q2.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
            const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
            lastFence = (uint64_t)d.fence;
            lastFlags = (uint32_t)d.flags;
            haveLast = true;
          }

          v2DescCapacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
        }
      } else if (st == STATUS_NOT_SUPPORTED) {
        decided = true;
        useV2 = false;
        // Fall through to legacy dump-ring below.
      } else {
        PrintNtStatus(L"D3DKMTEscape(dump-ring-v2) failed", f, st);
        return 2;
      }
    }

    if (decided && !useV2) {
      aerogpu_escape_dump_ring_inout q;
      ZeroMemory(&q, sizeof(q));
      q.hdr.version = AEROGPU_ESCAPE_VERSION;
      q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
      q.hdr.size = sizeof(q);
      q.hdr.reserved0 = 0;
      q.ring_id = ringId;
      q.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

      NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
      if (!NT_SUCCESS(st)) {
        PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st);
        return 2;
      }

      head = q.head;
      tail = q.tail;

      // Best-effort legacy detection (tail<head wrap requires knowing entry_count).
      bool assumedLegacy = false;
      if (TryComputeLegacyPending(q.ring_size_bytes, head, tail, &pending)) {
        assumedLegacy = true;
      } else {
        pending = (uint64_t)(uint32_t)(tail - head);
      }
      fmtStr = assumedLegacy ? L"legacy" : L"unknown";

      // Only print the "last" descriptor if we know we captured the full pending region.
      if (pending != 0 && pending == (uint64_t)q.desc_count && q.desc_count > 0 &&
          q.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
        const aerogpu_dbgctl_ring_desc &d = q.desc[q.desc_count - 1];
        lastFence = (uint64_t)d.signal_fence;
        lastFlags = (uint32_t)d.flags;
        haveLast = true;
      }
    }

    const int64_t dPending = havePrevPending ? ((int64_t)pending - (int64_t)prevPending) : 0;
    if (havePrevPending && pending != 0 && pending == prevPending) {
      stallIntervals += 1;
    } else {
      stallIntervals = 0;
    }
    const bool warnStall = (stallIntervals != 0 && stallIntervals >= stallWarnIntervals);
    const wchar_t *warn = warnStall ? L"STALL" : L"-";

    if (haveLast) {
      wprintf(L"ring[%lu/%lu] fmt=%s head=%lu tail=%lu pending=%I64u d_pending=%I64d stall_intervals=%lu warn=%s last_fence=0x%I64x last_flags=0x%08lx\n",
              (unsigned long)(i + 1), (unsigned long)samples, fmtStr, (unsigned long)head, (unsigned long)tail,
              (unsigned long long)pending, (long long)dPending, (unsigned long)stallIntervals, warn,
              (unsigned long long)lastFence, (unsigned long)lastFlags);
    } else {
      wprintf(L"ring[%lu/%lu] fmt=%s head=%lu tail=%lu pending=%I64u d_pending=%I64d stall_intervals=%lu warn=%s\n",
              (unsigned long)(i + 1), (unsigned long)samples, fmtStr, (unsigned long)head, (unsigned long)tail,
              (unsigned long long)pending, (long long)dPending, (unsigned long)stallIntervals, warn);
    }
    fflush(stdout);

    prevPending = pending;
    havePrevPending = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }
  return 0;
}

static bool AddU64NoOverflow(uint64_t a, uint64_t b, uint64_t *out) {
  if (b > UINT64_MAX - a) {
    return false;
  }
  if (out) {
    *out = a + b;
  }
  return true;
}

static wchar_t *HeapWcsCatSuffix(const wchar_t *base, const wchar_t *suffix) {
  if (!base || !suffix) {
    return NULL;
  }

  const size_t baseLen = wcslen(base);
  const size_t suffixLen = wcslen(suffix);
  const size_t totalLen = baseLen + suffixLen + 1;
  const size_t kMax = (size_t)-1;
  if (totalLen == 0 || totalLen > (kMax / sizeof(wchar_t))) {
    return NULL;
  }

  wchar_t *out = (wchar_t *)HeapAlloc(GetProcessHeap(), 0, totalLen * sizeof(wchar_t));
  if (!out) {
    return NULL;
  }
  memcpy(out, base, baseLen * sizeof(wchar_t));
  memcpy(out + baseLen, suffix, (suffixLen + 1) * sizeof(wchar_t));
  return out;
}

static wchar_t *HeapBuildIndexedBinPath(const wchar_t *base, uint32_t index) {
  if (!base || !base[0]) {
    return NULL;
  }

  // Common case: user passes something like "last_cmd.bin". When dumping multiple submissions,
  // generate "last_cmd_<index>.bin" (strip a trailing ".bin" case-insensitively).
  const wchar_t *kExt = L".bin";
  const size_t baseLen = wcslen(base);
  size_t prefixLen = baseLen;
  if (baseLen >= 4 && _wcsicmp(base + (baseLen - 4), kExt) == 0) {
    prefixLen = baseLen - 4;
  }

  wchar_t suffixBuf[32];
  swprintf_s(suffixBuf, _countof(suffixBuf), L"_%lu%s", (unsigned long)index, kExt);
  const size_t suffixLen = wcslen(suffixBuf);

  const size_t totalLen = prefixLen + suffixLen + 1;
  const size_t kMax = (size_t)-1;
  if (totalLen == 0 || totalLen > (kMax / sizeof(wchar_t))) {
    return NULL;
  }

  wchar_t *out = (wchar_t *)HeapAlloc(GetProcessHeap(), 0, totalLen * sizeof(wchar_t));
  if (!out) {
    return NULL;
  }

  if (prefixLen != 0) {
    memcpy(out, base, prefixLen * sizeof(wchar_t));
  }
  memcpy(out + prefixLen, suffixBuf, (suffixLen + 1) * sizeof(wchar_t));
  return out;
}

static int DumpGpaRangeToFile(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint64_t gpa, uint64_t sizeBytes,
                              const wchar_t *outPath, uint32_t *outFirstDword) {
  if (!f || !outPath) {
    return 1;
  }

  FILE *fp = NULL;
  errno_t ferr = _wfopen_s(&fp, outPath, L"wb");
  if (ferr != 0 || !fp) {
    fwprintf(stderr, L"Failed to open output file: %s (errno=%d)\n", outPath, (int)ferr);
    return 2;
  }

  int rc = 0;
  uint64_t remaining = sizeBytes;
  uint64_t curGpa = gpa;

  bool gotFirst = false;
  uint32_t firstDword = 0;

  while (remaining != 0) {
    uint32_t chunk = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
    if (remaining < (uint64_t)chunk) {
      chunk = (uint32_t)remaining;
    }

    aerogpu_escape_read_gpa_inout q;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.gpa = (aerogpu_escape_u64)curGpa;
    q.size_bytes = chunk;
    q.reserved0 = 0;

    const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"read-gpa failed", f, st);
      if (st == STATUS_NOT_SUPPORTED) {
        fwprintf(stderr, L"hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n");
      }
      rc = 2;
      goto cleanup;
    }

    const NTSTATUS op = (NTSTATUS)q.status;
    uint32_t bytesRead = q.bytes_copied;
    if (bytesRead > chunk) {
      bytesRead = chunk;
    }
    if (bytesRead > AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) {
      bytesRead = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
    }

    if (!NT_SUCCESS(op) && op != STATUS_PARTIAL_COPY) {
      PrintNtStatus(L"read-gpa operation failed", f, op);
      if (op == STATUS_NOT_SUPPORTED) {
        fwprintf(stderr, L"hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n");
      }
      rc = 2;
      goto cleanup;
    }
    if (bytesRead == 0) {
      fwprintf(stderr, L"read-gpa returned 0 bytes at gpa=0x%I64x (status=0x%08lx)\n",
               (unsigned long long)curGpa, (unsigned long)op);
      rc = 2;
      goto cleanup;
    }

    if (!gotFirst && outFirstDword && bytesRead >= 4) {
      memcpy(&firstDword, q.data, 4);
      gotFirst = true;
    }

    const size_t wrote = fwrite(q.data, 1, bytesRead, fp);
    if (wrote != (size_t)bytesRead) {
      fwprintf(stderr, L"Failed to write to output file: %s\n", outPath);
      rc = 2;
      goto cleanup;
    }

    curGpa += (uint64_t)bytesRead;
    remaining -= (uint64_t)bytesRead;

    if (op == STATUS_PARTIAL_COPY) {
      // We made some progress but did not satisfy the request; treat as failure so callers
      // don't mistakenly interpret the output as complete.
      PrintNtStatus(L"read-gpa partial copy", f, op);
      rc = 2;
      goto cleanup;
    }
  }

  rc = 0;
  if (outFirstDword && gotFirst) {
    *outFirstDword = firstDword;
  }

cleanup:
  if (fclose(fp) != 0 && rc == 0) {
    fwprintf(stderr, L"Failed to close output file: %s\n", outPath);
    rc = 2;
  }
  if (rc != 0) {
    BestEffortDeleteOutputFile(outPath);
  }
  return rc;
}

static const wchar_t *RingFormatToString(uint32_t fmt) {
  switch (fmt) {
  case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
    return L"legacy";
  case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
    return L"agpu";
  default:
    return L"unknown";
  }
}

static int DoDumpLastCmd(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId, uint32_t indexFromTail,
                         uint32_t count, const wchar_t *outPath, const wchar_t *allocOutPath, bool force) {
  if (!outPath || !outPath[0]) {
    fwprintf(stderr, L"--dump-last-submit/--dump-last-cmd requires --cmd-out <path> (or --out <path>)\n");
    return 1;
  }
  if (count == 0) {
    fwprintf(stderr, L"--count must be >= 1\n");
    return 1;
  }

  // Prefer the v2 dump-ring packet (AGPU tail window + alloc_table fields).
  aerogpu_escape_dump_ring_v2_inout q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.ring_id = ringId;
  q2.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  aerogpu_escape_dump_ring_inout q1;
  ZeroMemory(&q1, sizeof(q1));
  bool usedV2 = false;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));

  uint32_t ringFormat = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
  uint32_t head = 0;
  uint32_t tail = 0;
  uint32_t ringSizeBytes = 0;
  uint32_t descCount = 0;

  if (NT_SUCCESS(st)) {
    usedV2 = true;
    ringFormat = q2.ring_format;
    head = q2.head;
    tail = q2.tail;
    ringSizeBytes = q2.ring_size_bytes;
    descCount = q2.desc_count;
    if (descCount > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      descCount = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
    if (descCount == 0) {
      wprintf(L"Ring %lu (%s): no descriptors available\n", (unsigned long)ringId, RingFormatToString(ringFormat));
      return 0;
    }
  } else if (st == STATUS_NOT_SUPPORTED) {
    // Fallback to legacy dump-ring for older KMDs.
    q1.hdr.version = AEROGPU_ESCAPE_VERSION;
    q1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
    q1.hdr.size = sizeof(q1);
    q1.hdr.reserved0 = 0;
    q1.ring_id = ringId;
    q1.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

    st = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTEscape(dump-ring) failed", f, st);
      return 2;
    }

    ringFormat = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
    head = q1.head;
    tail = q1.tail;
    ringSizeBytes = q1.ring_size_bytes;
    descCount = q1.desc_count;
    if (descCount > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      descCount = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
    if (descCount == 0) {
      wprintf(L"Ring %lu: no descriptors available\n", (unsigned long)ringId);
      return 0;
    }
  } else {
    PrintNtStatus(L"D3DKMTEscape(dump-ring-v2) failed", f, st);
    return 2;
  }

  if (indexFromTail >= descCount) {
    fwprintf(stderr, L"--index-from-tail %lu out of range (ring returned %lu descriptors)\n",
             (unsigned long)indexFromTail, (unsigned long)descCount);
    return 1;
  }

  uint32_t actualCount = count;
  const uint32_t remaining = descCount - indexFromTail;
  if (actualCount > remaining) {
    actualCount = remaining;
  }

  wprintf(L"Ring %lu (%s)\n", (unsigned long)ringId, RingFormatToString(ringFormat));
  wprintf(L"  size: %lu bytes\n", (unsigned long)ringSizeBytes);
  wprintf(L"  head: 0x%08lx\n", (unsigned long)head);
  wprintf(L"  tail: 0x%08lx\n", (unsigned long)tail);

  if (actualCount != count) {
    wprintf(L"  note: requested --count=%lu but only %lu descriptors are available from index_from_tail=%lu\n",
            (unsigned long)count, (unsigned long)actualCount, (unsigned long)indexFromTail);
  }
  if (actualCount > 1) {
    wprintf(L"  dumping: index_from_tail=%lu..%lu (%lu submissions)\n", (unsigned long)indexFromTail,
            (unsigned long)(indexFromTail + actualCount - 1u), (unsigned long)actualCount);
  }

  if (allocOutPath && allocOutPath[0] && actualCount > 1) {
    fwprintf(stderr, L"--alloc-out is not supported with --count > 1\n");
    fwprintf(stderr, L"Hint: omit --alloc-out to use the default <cmd_path>.alloc_table.bin naming.\n");
    return 1;
  }

  for (uint32_t dumpIndex = 0; dumpIndex < actualCount; ++dumpIndex) {
    const uint32_t curIndexFromTail = indexFromTail + dumpIndex;
    const uint32_t idx = (descCount - 1u) - curIndexFromTail;

    aerogpu_dbgctl_ring_desc_v2 d;
    ZeroMemory(&d, sizeof(d));
    if (usedV2) {
      d = q2.desc[idx];
    } else {
      const aerogpu_dbgctl_ring_desc &d1 = q1.desc[idx];
      d.fence = d1.signal_fence;
      d.cmd_gpa = d1.cmd_gpa;
      d.cmd_size_bytes = d1.cmd_size_bytes;
      d.flags = d1.flags;
      d.alloc_table_gpa = 0;
      d.alloc_table_size_bytes = 0;
      d.reserved0 = 0;
    }

    uint32_t selectedRingIndex = idx;
    if (usedV2 && ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU && tail >= descCount) {
      selectedRingIndex = (tail - descCount) + idx;
    }

    const wchar_t *curOutPath = outPath;
    wchar_t *curOutPathOwned = NULL;
    if (actualCount > 1) {
      curOutPathOwned = HeapBuildIndexedBinPath(outPath, curIndexFromTail);
      if (!curOutPathOwned) {
        fwprintf(stderr, L"Out of memory building output path for index_from_tail=%lu\n",
                 (unsigned long)curIndexFromTail);
        return 2;
      }
      curOutPath = curOutPathOwned;
    }

    wprintf(
        L"  selected: index_from_tail=%lu -> ringIndex=%lu fence=0x%I64x cmdGpa=0x%I64x cmdBytes=%lu flags=0x%08lx\n",
        (unsigned long)curIndexFromTail, (unsigned long)selectedRingIndex, (unsigned long long)d.fence,
        (unsigned long long)d.cmd_gpa, (unsigned long)d.cmd_size_bytes, (unsigned long)d.flags);
    if (ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
      wprintf(L"            allocTableGpa=0x%I64x allocTableBytes=%lu\n", (unsigned long long)d.alloc_table_gpa,
              (unsigned long)d.alloc_table_size_bytes);
    }

    const uint64_t cmdGpa = (uint64_t)d.cmd_gpa;
    const uint64_t cmdSizeBytes = (uint64_t)d.cmd_size_bytes;
    if (cmdGpa == 0 && cmdSizeBytes == 0) {
      wprintf(L"  cmd: empty (cmd_gpa=0)\n");
      FILE *fp = NULL;
      errno_t ferr = _wfopen_s(&fp, curOutPath, L"wb");
      if (ferr != 0 || !fp) {
        fwprintf(stderr, L"Failed to create output file: %s (errno=%d)\n", curOutPath, (int)ferr);
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }
      fclose(fp);
      wprintf(L"  cmd dumped: %s (empty)\n", curOutPath);
    } else {
      if (cmdGpa == 0 || cmdSizeBytes == 0) {
        fwprintf(stderr, L"Invalid cmd_gpa/cmd_size_bytes pair: cmd_gpa=0x%I64x cmd_size_bytes=%I64u\n",
                 (unsigned long long)cmdGpa, (unsigned long long)cmdSizeBytes);
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }
      if (cmdSizeBytes > kDumpLastCmdHardMaxBytes) {
        fwprintf(stderr, L"Refusing to dump %I64u bytes (hard cap %I64u bytes)\n", (unsigned long long)cmdSizeBytes,
                 (unsigned long long)kDumpLastCmdHardMaxBytes);
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }
      if (cmdSizeBytes > kDumpLastCmdDefaultMaxBytes && !force) {
        fwprintf(stderr, L"Refusing to dump %I64u bytes (default cap %I64u bytes). Use --force to override.\n",
                 (unsigned long long)cmdSizeBytes, (unsigned long long)kDumpLastCmdDefaultMaxBytes);
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }
      if (!AddU64NoOverflow(cmdGpa, cmdSizeBytes, NULL)) {
        fwprintf(stderr, L"Invalid cmd_gpa/cmd_size_bytes range (overflow): gpa=0x%I64x size=%I64u\n",
                 (unsigned long long)cmdGpa, (unsigned long long)cmdSizeBytes);
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }

      uint32_t firstDword = 0;
      const int dumpRc = DumpGpaRangeToFile(f, hAdapter, cmdGpa, cmdSizeBytes, curOutPath, &firstDword);
      if (dumpRc != 0) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return dumpRc;
      }
      wprintf(L"  cmd dumped: %s (%I64u bytes)\n", curOutPath, (unsigned long long)cmdSizeBytes);

      if (cmdSizeBytes >= 4) {
        if (firstDword == AEROGPU_CMD_STREAM_MAGIC) {
          wprintf(L"  cmd stream: magic=0x%08lx (ACMD)\n", (unsigned long)firstDword);
        } else {
          wprintf(L"  cmd stream: magic=0x%08lx (expected 0x%08lx)\n", (unsigned long)firstDword,
                  (unsigned long)AEROGPU_CMD_STREAM_MAGIC);
        }
      }
    }

    wchar_t *summaryPath = HeapWcsCatSuffix(curOutPath, L".txt");
    if (summaryPath) {
      FILE *sf = NULL;
      errno_t serr = _wfopen_s(&sf, summaryPath, L"wt");
      if (serr == 0 && sf) {
        fwprintf(sf, L"ring_id=%lu\n", (unsigned long)ringId);
        fwprintf(sf, L"ring_format=%s\n", RingFormatToString(ringFormat));
        fwprintf(sf, L"head=0x%08lx\n", (unsigned long)head);
        fwprintf(sf, L"tail=0x%08lx\n", (unsigned long)tail);
        fwprintf(sf, L"selected_index_from_tail=%lu\n", (unsigned long)curIndexFromTail);
        fwprintf(sf, L"selected_ring_index=%lu\n", (unsigned long)selectedRingIndex);
        fwprintf(sf, L"fence=0x%I64x\n", (unsigned long long)d.fence);
        fwprintf(sf, L"flags=0x%08lx\n", (unsigned long)d.flags);
        fwprintf(sf, L"cmd_gpa=0x%I64x\n", (unsigned long long)d.cmd_gpa);
        fwprintf(sf, L"cmd_size_bytes=%lu\n", (unsigned long)d.cmd_size_bytes);
        if (ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
          fwprintf(sf, L"alloc_table_gpa=0x%I64x\n", (unsigned long long)d.alloc_table_gpa);
          fwprintf(sf, L"alloc_table_size_bytes=%lu\n", (unsigned long)d.alloc_table_size_bytes);
        }
        fclose(sf);
      }
      HeapFree(GetProcessHeap(), 0, summaryPath);
    }

    // Optional alloc table dump (AGPU only).
    if (ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
      const uint64_t allocGpa = (uint64_t)d.alloc_table_gpa;
      const uint64_t allocSizeBytes = (uint64_t)d.alloc_table_size_bytes;
      if (allocGpa == 0 && allocSizeBytes == 0) {
        if (allocOutPath && allocOutPath[0]) {
          // Some submissions do not require an alloc table, and legacy rings do not expose it.
          // Still create the output file if explicitly requested to keep scripting simple.
          if (!CreateEmptyFile(allocOutPath)) {
            if (curOutPathOwned) {
              HeapFree(GetProcessHeap(), 0, curOutPathOwned);
            }
            return 2;
          }
          wprintf(L"  alloc table: not present (wrote empty file)\n");
        }
      } else {
        if (allocGpa == 0 || allocSizeBytes == 0) {
          fwprintf(stderr, L"Invalid alloc_table_gpa/alloc_table_size_bytes pair: gpa=0x%I64x size=%I64u\n",
                   (unsigned long long)allocGpa, (unsigned long long)allocSizeBytes);
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          return 2;
        }
        if (allocSizeBytes > kDumpLastCmdHardMaxBytes) {
          fwprintf(stderr, L"Refusing to dump alloc table %I64u bytes (hard cap %I64u bytes)\n",
                   (unsigned long long)allocSizeBytes, (unsigned long long)kDumpLastCmdHardMaxBytes);
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          return 2;
        }
        if (allocSizeBytes > kDumpLastCmdDefaultMaxBytes && !force) {
          fwprintf(stderr,
                   L"Refusing to dump alloc table %I64u bytes (default cap %I64u bytes). Use --force to override.\n",
                   (unsigned long long)allocSizeBytes, (unsigned long long)kDumpLastCmdDefaultMaxBytes);
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          return 2;
        }
        if (!AddU64NoOverflow(allocGpa, allocSizeBytes, NULL)) {
          fwprintf(stderr, L"Invalid alloc table range (overflow): gpa=0x%I64x size=%I64u\n",
                   (unsigned long long)allocGpa, (unsigned long long)allocSizeBytes);
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          return 2;
        }

        const wchar_t *allocPath = NULL;
        wchar_t *allocPathOwned = NULL;
        if (allocOutPath && allocOutPath[0]) {
          allocPath = allocOutPath;
        } else {
          allocPathOwned = HeapWcsCatSuffix(curOutPath, L".alloc_table.bin");
          if (!allocPathOwned) {
            fwprintf(stderr, L"Out of memory building alloc table output path\n");
            if (curOutPathOwned) {
              HeapFree(GetProcessHeap(), 0, curOutPathOwned);
            }
            return 2;
          }
          allocPath = allocPathOwned;
        }

        const int dumpAllocRc = DumpGpaRangeToFile(f, hAdapter, allocGpa, allocSizeBytes, allocPath, NULL);
        if (dumpAllocRc == 0) {
          wprintf(L"  alloc table dumped: %s\n", allocPath);
        }
        if (allocPathOwned) {
          HeapFree(GetProcessHeap(), 0, allocPathOwned);
        }
        if (dumpAllocRc != 0) {
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          return dumpAllocRc;
        }
      }
    }
    else if (allocOutPath && allocOutPath[0]) {
      // Non-AGPU ring formats do not expose alloc tables; still create an empty output if requested.
      if (!CreateEmptyFile(allocOutPath)) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        return 2;
      }
      wprintf(L"  alloc table: not available for ring format %s (wrote empty file)\n", RingFormatToString(ringFormat));
    }

    if (curOutPathOwned) {
      HeapFree(GetProcessHeap(), 0, curOutPathOwned);
    }
  }

  return 0;

}

static bool QueryVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId,
                        aerogpu_escape_query_vblank_out *out, bool *supportedOut) {
  ZeroMemory(out, sizeof(*out));
  out->hdr.version = AEROGPU_ESCAPE_VERSION;
  out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
  out->hdr.size = sizeof(*out);
  out->hdr.reserved0 = 0;
  out->vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    wprintf(L"QueryVblank: VidPnSourceId=%lu not supported; retrying with source 0\n", (unsigned long)vidpnSourceId);
    ZeroMemory(out, sizeof(*out));
    out->hdr.version = AEROGPU_ESCAPE_VERSION;
    out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
    out->hdr.size = sizeof(*out);
    out->hdr.reserved0 = 0;
    out->vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  }
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(dump-vblank) failed", f, st);
    return false;
  }

  if (supportedOut) {
    bool supported = true;
    if ((out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      supported = (out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }
    *supportedOut = supported;
  }
  return true;
}

static void PrintIrqMask(const wchar_t *label, uint32_t mask) {
  wprintf(L"  %s: 0x%08lx", label, (unsigned long)mask);
  if (mask != 0) {
    wprintf(L" [");
    bool first = true;
    const auto Emit = [&](uint32_t bit, const wchar_t *name) {
      if ((mask & bit) == 0) {
        return;
      }
      if (!first) {
        wprintf(L"|");
      }
      wprintf(L"%s", name);
      first = false;
    };
    Emit(kAerogpuIrqFence, L"FENCE");
    Emit(kAerogpuIrqScanoutVblank, L"VBLANK");
    Emit(kAerogpuIrqError, L"ERROR");
    wprintf(L"]");
  }
  wprintf(L"\n");
}

static void PrintVblankSnapshot(const aerogpu_escape_query_vblank_out *q, bool supported) {
  wprintf(L"Vblank (VidPn source %lu)\n", (unsigned long)q->vidpn_source_id);
  PrintIrqMask(L"IRQ_ENABLE", q->irq_enable);
  PrintIrqMask(L"IRQ_STATUS", q->irq_status);
  PrintIrqMask(L"IRQ_ACTIVE", q->irq_enable & q->irq_status);
  if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
    if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
      wprintf(L"  vblank_interrupt_type: %lu\n", (unsigned long)q->vblank_interrupt_type);
    } else {
      wprintf(L"  vblank_interrupt_type: (not enabled or not reported)\n");
    }
  }

  if (!supported) {
    if ((q->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      wprintf(L"  vblank: not supported (flags=0x%08lx)\n", (unsigned long)q->flags);
    } else {
      wprintf(L"  vblank: not supported\n");
    }
    return;
  }

  wprintf(L"  vblank_seq: 0x%I64x (%I64u)\n", (unsigned long long)q->vblank_seq, (unsigned long long)q->vblank_seq);
  wprintf(L"  last_vblank_time_ns: 0x%I64x (%I64u ns)\n",
          (unsigned long long)q->last_vblank_time_ns,
          (unsigned long long)q->last_vblank_time_ns);

  if (q->vblank_period_ns != 0) {
    const double hz = 1000000000.0 / (double)q->vblank_period_ns;
    wprintf(L"  vblank_period_ns: %lu (~%.3f Hz)\n", (unsigned long)q->vblank_period_ns, hz);
  } else {
    wprintf(L"  vblank_period_ns: 0\n");
  }
}

typedef struct WaitThreadCtx {
  const D3DKMT_FUNCS *f;
  D3DKMT_HANDLE hAdapter;
  UINT vid_pn_source_id;
  HANDLE request_event;
  HANDLE done_event;
  HANDLE thread;
  volatile LONG stop;
  volatile LONG last_status;
} WaitThreadCtx;

static DWORD WINAPI WaitThreadProc(LPVOID param) {
  WaitThreadCtx *ctx = (WaitThreadCtx *)param;
  for (;;) {
    DWORD w = WaitForSingleObject(ctx->request_event, INFINITE);
    if (w != WAIT_OBJECT_0) {
      InterlockedExchange(&ctx->last_status, (LONG)0xC0000001L /* STATUS_UNSUCCESSFUL */);
      SetEvent(ctx->done_event);
      continue;
    }

    if (InterlockedCompareExchange(&ctx->stop, 0, 0) != 0) {
      break;
    }

    D3DKMT_WAITFORVERTICALBLANKEVENT e;
    ZeroMemory(&e, sizeof(e));
    e.hAdapter = ctx->hAdapter;
    e.hDevice = 0;
    e.VidPnSourceId = ctx->vid_pn_source_id;
    NTSTATUS st = ctx->f->WaitForVerticalBlankEvent(&e);
    InterlockedExchange(&ctx->last_status, st);
    SetEvent(ctx->done_event);
  }
  return 0;
}

static bool StartWaitThread(WaitThreadCtx *out, const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, UINT vidpnSourceId) {
  ZeroMemory(out, sizeof(*out));
  out->f = f;
  out->hAdapter = hAdapter;
  out->vid_pn_source_id = vidpnSourceId;
  out->stop = 0;
  out->last_status = 0;
  out->request_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  out->done_event = CreateEventW(NULL, FALSE, FALSE, NULL);
  if (!out->request_event || !out->done_event) {
    if (out->request_event) {
      CloseHandle(out->request_event);
      out->request_event = NULL;
    }
    if (out->done_event) {
      CloseHandle(out->done_event);
      out->done_event = NULL;
    }
    return false;
  }

  out->thread = CreateThread(NULL, 0, WaitThreadProc, out, 0, NULL);
  if (!out->thread) {
    CloseHandle(out->request_event);
    out->request_event = NULL;
    CloseHandle(out->done_event);
    out->done_event = NULL;
    return false;
  }
  return true;
}

static void StopWaitThread(WaitThreadCtx *ctx) {
  if (!ctx) {
    return;
  }

  if (ctx->thread) {
    InterlockedExchange(&ctx->stop, 1);
    SetEvent(ctx->request_event);
    WaitForSingleObject(ctx->thread, 5000);
    CloseHandle(ctx->thread);
    ctx->thread = NULL;
  }

  if (ctx->request_event) {
    CloseHandle(ctx->request_event);
    ctx->request_event = NULL;
  }
  if (ctx->done_event) {
    CloseHandle(ctx->done_event);
    ctx->done_event = NULL;
  }
}

static int DoWaitVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                        uint32_t timeoutMs, bool *skipCloseAdapter) {
  if (skipCloseAdapter) {
    *skipCloseAdapter = false;
  }
  if (!f->WaitForVerticalBlankEvent) {
    fwprintf(stderr, L"D3DKMTWaitForVerticalBlankEvent not available (missing gdi32 export)\n");
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }
  if (timeoutMs == 0) {
    timeoutMs = 1;
  }

  LARGE_INTEGER freq;
  if (!QueryPerformanceFrequency(&freq) || freq.QuadPart <= 0) {
    fwprintf(stderr, L"QueryPerformanceFrequency failed\n");
    return 1;
  }

  // Allocate on heap so we can safely leak on timeout (the wait thread may be
  // blocked inside the kernel thunk; tearing it down can deadlock).
  WaitThreadCtx *waiter = (WaitThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(WaitThreadCtx));
  if (!waiter) {
    fwprintf(stderr, L"HeapAlloc failed\n");
    return 1;
  }

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
    fwprintf(stderr, L"Failed to start wait thread\n");
    HeapFree(GetProcessHeap(), 0, waiter);
    return 1;
  }

  DWORD w = 0;
  NTSTATUS st = 0;
  for (;;) {
    // Prime: perform one wait so subsequent deltas represent full vblank periods.
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      fwprintf(stderr, L"vblank wait timed out after %lu ms (sample 1/%lu)\n", (unsigned long)timeoutMs,
               (unsigned long)samples);
      if (skipCloseAdapter) {
        // The wait thread may be blocked inside the kernel thunk. Avoid calling
        // D3DKMTCloseAdapter in this case; just exit the process.
        *skipCloseAdapter = true;
      }
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      fwprintf(stderr, L"WaitForSingleObject failed (rc=%lu)\n", (unsigned long)w);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      wprintf(L"WaitForVBlank: VidPnSourceId=%lu not supported; retrying with source 0\n",
              (unsigned long)effectiveVidpnSourceId);
      StopWaitThread(waiter);
      effectiveVidpnSourceId = 0;
      if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
        fwprintf(stderr, L"Failed to restart wait thread\n");
        HeapFree(GetProcessHeap(), 0, waiter);
        return 1;
      }
      continue;
    }
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTWaitForVerticalBlankEvent failed", f, st);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }
    break;
  }

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);

  double min_ms = 1e9;
  double max_ms = 0.0;
  double sum_ms = 0.0;
  uint32_t deltas = 0;

  for (uint32_t i = 1; i < samples; ++i) {
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      fwprintf(stderr, L"vblank wait timed out after %lu ms (sample %lu/%lu)\n", (unsigned long)timeoutMs,
               (unsigned long)(i + 1), (unsigned long)samples);
      if (skipCloseAdapter) {
        // The wait thread may be blocked inside the kernel thunk. Avoid calling
        // D3DKMTCloseAdapter in this case; just exit the process.
        *skipCloseAdapter = true;
      }
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      fwprintf(stderr, L"WaitForSingleObject failed (rc=%lu)\n", (unsigned long)w);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTWaitForVerticalBlankEvent failed", f, st);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt_ms = (double)(now.QuadPart - last.QuadPart) * 1000.0 / (double)freq.QuadPart;
    last = now;

    if (dt_ms < min_ms) {
      min_ms = dt_ms;
    }
    if (dt_ms > max_ms) {
      max_ms = dt_ms;
    }
    sum_ms += dt_ms;
    deltas += 1;

    wprintf(L"vblank[%lu/%lu]: %.3f ms\n", (unsigned long)(i + 1), (unsigned long)samples, dt_ms);
  }

  StopWaitThread(waiter);
  HeapFree(GetProcessHeap(), 0, waiter);

  if (deltas != 0) {
    const double avg_ms = sum_ms / (double)deltas;
    const double hz = (avg_ms > 0.0) ? (1000.0 / avg_ms) : 0.0;
    wprintf(L"Summary (%lu waits): avg=%.3f ms min=%.3f ms max=%.3f ms (~%.3f Hz)\n", (unsigned long)samples, avg_ms,
            min_ms, max_ms, hz);
  } else {
    wprintf(L"vblank wait OK\n");
  }

  return 0;
}

static int DoQueryScanline(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                           uint32_t intervalMs) {
  if (!f->GetScanLine) {
    fwprintf(stderr, L"D3DKMTGetScanLine not available (missing gdi32 export)\n");
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  uint32_t inVblank = 0;
  uint32_t outVblank = 0;
  uint32_t minLine = 0xFFFFFFFFu;
  uint32_t maxLine = 0;

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  for (uint32_t i = 0; i < samples; ++i) {
    D3DKMT_GETSCANLINE s;
    ZeroMemory(&s, sizeof(s));
    s.hAdapter = hAdapter;
    s.VidPnSourceId = effectiveVidpnSourceId;

    NTSTATUS st = f->GetScanLine(&s);
    if (!NT_SUCCESS(st) && st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      wprintf(L"GetScanLine: VidPnSourceId=%lu not supported; retrying with source 0\n",
              (unsigned long)effectiveVidpnSourceId);
      effectiveVidpnSourceId = 0;
      s.VidPnSourceId = effectiveVidpnSourceId;
      st = f->GetScanLine(&s);
    }
    if (!NT_SUCCESS(st)) {
      PrintNtStatus(L"D3DKMTGetScanLine failed", f, st);
      return 2;
    }

    wprintf(L"scanline[%lu/%lu]: %lu%s\n", (unsigned long)(i + 1), (unsigned long)samples, (unsigned long)s.ScanLine,
            s.InVerticalBlank ? L" (vblank)" : L"");

    if (s.InVerticalBlank) {
      inVblank += 1;
    } else {
      outVblank += 1;
      if ((uint32_t)s.ScanLine < minLine) {
        minLine = (uint32_t)s.ScanLine;
      }
      if ((uint32_t)s.ScanLine > maxLine) {
        maxLine = (uint32_t)s.ScanLine;
      }
    }

    if (i + 1 < samples && intervalMs != 0) {
      Sleep(intervalMs);
    }
  }

  wprintf(L"Summary: in_vblank=%lu out_vblank=%lu", (unsigned long)inVblank, (unsigned long)outVblank);
  if (outVblank != 0) {
    wprintf(L" out_scanline_range=[%lu, %lu]", (unsigned long)minLine, (unsigned long)maxLine);
  }
  wprintf(L"\n");
  return 0;
}

static int DoDumpVblank(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                        uint32_t intervalMs) {
  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  aerogpu_escape_query_vblank_out q;
  aerogpu_escape_query_vblank_out prev;
  bool supported = false;
  bool prevSupported = false;
  bool havePrev = false;
  uint32_t stallCount = 0;
  uint64_t perVblankUsMin = 0;
  uint64_t perVblankUsMax = 0;
  uint64_t perVblankUsSum = 0;
  uint64_t perVblankUsSamples = 0;

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  bool scanlineFallbackToSource0 = false;
  for (uint32_t i = 0; i < samples; ++i) {
    if (!QueryVblank(f, hAdapter, effectiveVidpnSourceId, &q, &supported)) {
      return 2;
    }
    effectiveVidpnSourceId = q.vidpn_source_id;

    if (samples > 1) {
      wprintf(L"Sample %lu/%lu:\n", (unsigned long)(i + 1), (unsigned long)samples);
    }
    PrintVblankSnapshot(&q, supported);
    if (f->GetScanLine) {
      D3DKMT_GETSCANLINE s;
      ZeroMemory(&s, sizeof(s));
      s.hAdapter = hAdapter;
      s.VidPnSourceId = scanlineFallbackToSource0 ? 0 : effectiveVidpnSourceId;
      NTSTATUS st = f->GetScanLine(&s);
      if (!NT_SUCCESS(st) && st == STATUS_INVALID_PARAMETER && s.VidPnSourceId != 0) {
        wprintf(L"  GetScanLine: VidPnSourceId=%lu not supported; retrying with source 0\n",
                (unsigned long)s.VidPnSourceId);
        scanlineFallbackToSource0 = true;
        s.VidPnSourceId = 0;
        st = f->GetScanLine(&s);
      }
      if (NT_SUCCESS(st)) {
        wprintf(L"  scanline: %lu%s\n", (unsigned long)s.ScanLine, s.InVerticalBlank ? L" (vblank)" : L"");
      } else if (st == STATUS_NOT_SUPPORTED) {
        wprintf(L"  scanline: (not supported)\n");
      } else {
        PrintNtStatus(L"D3DKMTGetScanLine failed", f, st);
      }
    }

    if (!supported) {
      PrintNtStatus(L"Vblank not supported by device/KMD", f, STATUS_NOT_SUPPORTED);
      return 2;
    }

    if (havePrev && supported && prevSupported) {
      if (q.vblank_seq < prev.vblank_seq || q.last_vblank_time_ns < prev.last_vblank_time_ns) {
        wprintf(L"  delta: counters reset (prev seq=0x%I64x time=0x%I64x, now seq=0x%I64x time=0x%I64x)\n",
                (unsigned long long)prev.vblank_seq,
                (unsigned long long)prev.last_vblank_time_ns,
                (unsigned long long)q.vblank_seq,
                (unsigned long long)q.last_vblank_time_ns);
      } else {
        const uint64_t dseq = q.vblank_seq - prev.vblank_seq;
        const uint64_t dt = q.last_vblank_time_ns - prev.last_vblank_time_ns;
        wprintf(L"  delta: seq=%I64u time=%I64u ns\n", (unsigned long long)dseq, (unsigned long long)dt);
        if (dseq != 0 && dt != 0) {
          const double hz = (double)dseq * 1000000000.0 / (double)dt;
          wprintf(L"  observed: ~%.3f Hz\n", hz);

          const uint64_t perVblankUs = (dt / dseq) / 1000ull;
          if (perVblankUsSamples == 0) {
            perVblankUsMin = perVblankUs;
            perVblankUsMax = perVblankUs;
          } else {
            if (perVblankUs < perVblankUsMin) {
              perVblankUsMin = perVblankUs;
            }
            if (perVblankUs > perVblankUsMax) {
              perVblankUsMax = perVblankUs;
            }
          }
          perVblankUsSum += perVblankUs;
          perVblankUsSamples += 1;
        } else if (dseq == 0) {
          stallCount += 1;
        }
      }
    }

    prev = q;
    prevSupported = supported;
    havePrev = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }

  if (samples > 1 && perVblankUsSamples != 0) {
    const uint64_t avg = perVblankUsSum / perVblankUsSamples;
    wprintf(L"Summary (%I64u deltas): per-vblank ~%I64u us (min=%I64u max=%I64u), stalls=%lu\n",
            (unsigned long long)perVblankUsSamples,
            (unsigned long long)avg,
            (unsigned long long)perVblankUsMin,
            (unsigned long long)perVblankUsMax,
            (unsigned long)stallCount);
  }

  return 0;
}

static int DoSelftest(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t timeoutMs, uint32_t vidpnSourceId) {
  // Best-effort: query device feature bits so we can print which selftest sub-checks are applicable.
  uint64_t features = 0;
  bool haveFeatures = false;
  {
    aerogpu_escape_query_device_v2_out dev;
    ZeroMemory(&dev, sizeof(dev));
    dev.hdr.version = AEROGPU_ESCAPE_VERSION;
    dev.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
    dev.hdr.size = sizeof(dev);
    dev.hdr.reserved0 = 0;

    NTSTATUS stDev = SendAerogpuEscape(f, hAdapter, &dev, sizeof(dev));
    if (NT_SUCCESS(stDev)) {
      features = dev.features_lo;
      haveFeatures = true;
    }
  }

  const bool featureVblank = haveFeatures && ((features & AEROGPU_FEATURE_VBLANK) != 0);
  const bool featureCursor = haveFeatures && ((features & AEROGPU_FEATURE_CURSOR) != 0);

  // Best-effort: query scanout enable so we can distinguish "vblank skipped because scanout is disabled"
  // from "vblank passed". The KMD selftest only validates vblank/IRQ delivery while scanout is enabled
  // because some device models gate vblank tick generation on scanout enable.
  bool scanoutKnown = false;
  bool scanoutEnabled = false;
  {
    aerogpu_escape_query_scanout_out qs;
    ZeroMemory(&qs, sizeof(qs));
    qs.hdr.version = AEROGPU_ESCAPE_VERSION;
    qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    qs.hdr.size = sizeof(qs);
    qs.hdr.reserved0 = 0;
    qs.vidpn_source_id = vidpnSourceId;

    NTSTATUS stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
    if (!NT_SUCCESS(stScanout) &&
        (stScanout == STATUS_INVALID_PARAMETER || stScanout == STATUS_NOT_SUPPORTED) &&
        vidpnSourceId != 0) {
      // Older KMDs may only support source 0; retry.
      ZeroMemory(&qs, sizeof(qs));
      qs.hdr.version = AEROGPU_ESCAPE_VERSION;
      qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
      qs.hdr.size = sizeof(qs);
      qs.hdr.reserved0 = 0;
      qs.vidpn_source_id = 0;
      stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
    }
    if (NT_SUCCESS(stScanout)) {
      scanoutKnown = true;
      scanoutEnabled = (qs.mmio_enable != 0);
    }
  }

  aerogpu_escape_selftest_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.timeout_ms = timeoutMs;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTEscape(selftest) failed", f, st);
    // Use an out-of-band nonzero value to distinguish transport failures from
    // KMD-reported selftest failures (whose exit codes match error_code).
    return 254;
  }

  enum SelftestStage {
    STAGE_RING = 0,
    STAGE_VBLANK = 1,
    STAGE_IRQ = 2,
    STAGE_CURSOR = 3,
    STAGE_DONE = 4,
  };

  const bool timeBudgetExhausted =
      (!q.passed && q.error_code == AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED);

  SelftestStage failedStage = STAGE_DONE;
  if (!q.passed) {
    switch (q.error_code) {
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK:
      failedStage = STAGE_VBLANK;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED:
      failedStage = STAGE_IRQ;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH:
      failedStage = STAGE_CURSOR;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED:
      // The KMD only reports TIME_BUDGET_EXHAUSTED after the ring head advancement check
      // succeeds, while attempting optional sub-checks. Treat it as "after ring" but
      // handle per-subcheck reporting below.
      failedStage = STAGE_VBLANK;
      break;
    default:
      failedStage = STAGE_RING;
      break;
    }
  }

  const auto PrintStep = [&](const wchar_t *name, const wchar_t *status, const wchar_t *detail) {
    if (detail && detail[0]) {
      wprintf(L"  %-8s: %s (%s)\n", name, status, detail);
    } else {
      wprintf(L"  %-8s: %s\n", name, status);
    }
  };

  // Ring is always the first check.
  if (q.passed || timeBudgetExhausted || failedStage > STAGE_RING) {
    PrintStep(L"ring", L"PASS", L"ring head advances");
  } else {
    PrintStep(L"ring", L"FAIL", SelftestErrorToString(q.error_code));
  }

  // VBlank (optional, feature-gated).
  if (timeBudgetExhausted) {
    PrintStep(L"vblank", L"SKIP", L"time budget exhausted (increase --timeout-ms)");
  } else if (!haveFeatures) {
    PrintStep(L"vblank", L"?", L"features unknown");
  } else if (!featureVblank) {
    PrintStep(L"vblank", L"SKIP", L"AEROGPU_FEATURE_VBLANK not set");
  } else if (scanoutKnown && !scanoutEnabled) {
    PrintStep(L"vblank", L"SKIP", L"scanout disabled");
  } else if (q.passed || failedStage > STAGE_VBLANK) {
    PrintStep(L"vblank", L"PASS", L"SCANOUT0_VBLANK_SEQ changes");
  } else if (failedStage == STAGE_VBLANK) {
    PrintStep(L"vblank", L"FAIL", SelftestErrorToString(q.error_code));
  } else {
    PrintStep(L"vblank", L"SKIP", L"not reached");
  }

  // IRQ sanity (currently uses vblank IRQ as a safe trigger).
  if (timeBudgetExhausted) {
    PrintStep(L"irq", L"SKIP", L"time budget exhausted (increase --timeout-ms)");
  } else if (!haveFeatures) {
    PrintStep(L"irq", L"?", L"features unknown");
  } else if (!featureVblank) {
    PrintStep(L"irq", L"SKIP", L"requires vblank feature");
  } else if (scanoutKnown && !scanoutEnabled) {
    PrintStep(L"irq", L"SKIP", L"scanout disabled");
  } else if (q.passed || failedStage > STAGE_IRQ) {
    PrintStep(L"irq", L"PASS", L"IRQ_STATUS latch/ACK + ISR + DPC");
  } else if (failedStage == STAGE_IRQ) {
    PrintStep(L"irq", L"FAIL", SelftestErrorToString(q.error_code));
  } else {
    PrintStep(L"irq", L"SKIP", L"not reached");
  }

  // Cursor (optional, feature-gated).
  if (timeBudgetExhausted) {
    PrintStep(L"cursor", L"SKIP", L"time budget exhausted (increase --timeout-ms)");
  } else if (!haveFeatures) {
    PrintStep(L"cursor", L"?", L"features unknown");
  } else if (!featureCursor) {
    PrintStep(L"cursor", L"SKIP", L"AEROGPU_FEATURE_CURSOR not set");
  } else if (q.passed || failedStage > STAGE_CURSOR) {
    PrintStep(L"cursor", L"PASS", L"cursor reg RW");
  } else if (failedStage == STAGE_CURSOR) {
    PrintStep(L"cursor", L"FAIL", SelftestErrorToString(q.error_code));
  } else {
    PrintStep(L"cursor", L"SKIP", L"not reached");
  }

  wprintf(L"Selftest: %s\n", q.passed ? L"PASS" : L"FAIL");
  if (!q.passed) {
    wprintf(L"Error code: %lu (%s)\n", (unsigned long)q.error_code, SelftestErrorToString(q.error_code));
    if (q.error_code == AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED) {
      wprintf(L"Hint: increase --timeout-ms so all optional sub-checks can run.\n");
    }
    // Return the KMD-provided stable error code for automation (0 == PASS).
    // If a buggy/older KMD reports failure with error_code==0, fall back to 1.
    return (q.error_code != 0) ? (int)q.error_code : 1;
  }
  return 0;
}

static int DoReadGpa(const D3DKMT_FUNCS *f,
                     D3DKMT_HANDLE hAdapter,
                     uint64_t gpa,
                     uint32_t sizeBytes,
                     const wchar_t *outPath,
                     bool force) {
  if (outPath && *outPath) {
    if (!DumpGpaToFile(f, hAdapter, gpa, sizeBytes, outPath)) {
      return 2;
    }
    wprintf(L"Wrote %lu bytes from GPA 0x%I64x to %s\n", (unsigned long)sizeBytes, (unsigned long long)gpa, outPath);
    return 0;
  }

  if (sizeBytes == 0) {
    wprintf(L"Read GPA 0x%I64x (0 bytes)\n", (unsigned long long)gpa);
    return 0;
  }

  // Without --out, print a bounded prefix to avoid spamming stdout.
  const uint32_t kMaxPrintBytes = 256u;
  uint32_t want = sizeBytes;
  if (!force && want > kMaxPrintBytes) {
    want = kMaxPrintBytes;
  }
  if (want > AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) {
    want = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
  }

  aerogpu_escape_read_gpa_inout io;
  ZeroMemory(&io, sizeof(io));
  io.hdr.version = AEROGPU_ESCAPE_VERSION;
  io.hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
  io.hdr.size = sizeof(io);
  io.hdr.reserved0 = 0;
  io.gpa = (aerogpu_escape_u64)gpa;
  io.size_bytes = (aerogpu_escape_u32)want;
  io.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscapeDirect(f, hAdapter, &io, sizeof(io));
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"read-gpa failed", f, st);
    if (st == STATUS_NOT_SUPPORTED) {
      fwprintf(stderr, L"hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n");
    }
    return 2;
  }

  const NTSTATUS op = (NTSTATUS)io.status;
  uint32_t copied = io.bytes_copied;
  if (copied > want) {
    copied = want;
  }

  wprintf(L"read-gpa: gpa=0x%I64x req=%lu show=%lu status=0x%08lx copied=%lu\n",
          (unsigned long long)gpa,
          (unsigned long)sizeBytes,
          (unsigned long)want,
          (unsigned long)op,
          (unsigned long)copied);

  if (!NT_SUCCESS(op) && op != STATUS_PARTIAL_COPY) {
    PrintNtStatus(L"read-gpa operation failed", f, op);
    if (op == STATUS_NOT_SUPPORTED) {
      fwprintf(stderr, L"hint: the installed KMD does not support AEROGPU_ESCAPE_OP_READ_GPA\n");
    }
  } else if (op == STATUS_PARTIAL_COPY) {
    PrintNtStatus(L"read-gpa partial copy", f, op);
  }

  if (copied != 0) {
    HexDumpBytes(io.data, copied, gpa);
  }

  if (want < sizeBytes) {
    wprintf(L"(truncated; use --out to dump full range)\n");
  }

  if (op == STATUS_PARTIAL_COPY) {
    return 3;
  }
  return NT_SUCCESS(op) ? 0 : 2;
}

static void JsonWriteTopLevelErrno(std::string *out, const char *command, const char *message, int err) {
  if (!out) {
    return;
  }
  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String(command ? command : "");
  w.Key("ok");
  w.Bool(false);
  w.Key("error");
  w.BeginObject();
  w.Key("message");
  w.String(message ? message : "");
  w.Key("errno");
  w.Int32(err);
  const char *errStr = strerror(err);
  if (errStr) {
    w.Key("errno_message");
    w.String(errStr);
  }
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
}

static int DoReadGpaJson(const D3DKMT_FUNCS *f,
                         D3DKMT_HANDLE hAdapter,
                         uint64_t gpa,
                         uint32_t sizeBytes,
                         const wchar_t *outFile,
                         std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_read_gpa_inout io;
  ZeroMemory(&io, sizeof(io));
  io.hdr.version = AEROGPU_ESCAPE_VERSION;
  io.hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
  io.hdr.size = sizeof(io);
  io.hdr.reserved0 = 0;
  io.gpa = gpa;
  io.size_bytes = sizeBytes;
  io.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &io, sizeof(io));
  if (!NT_SUCCESS(st)) {
    if (outFile && *outFile) {
      BestEffortDeleteOutputFile(outFile);
    }
    JsonWriteTopLevelError(out, "read-gpa", f, "D3DKMTEscape(read-gpa) failed", st);
    return 2;
  }

  const NTSTATUS op = (NTSTATUS)io.status;
  uint32_t copied = io.bytes_copied;
  if (copied > sizeBytes) {
    copied = sizeBytes;
  }
  if (copied > AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) {
    copied = AEROGPU_DBGCTL_READ_GPA_MAX_BYTES;
  }

  const bool ok = (NT_SUCCESS(op) && op != STATUS_PARTIAL_COPY);

  bool wroteFile = false;
  if (outFile && *outFile) {
    if (!ok) {
      // Ensure callers do not see a stale/partial output file when the read failed.
      BestEffortDeleteOutputFile(outFile);
    } else {
      if (!WriteBinaryFile(outFile, io.data, copied)) {
        JsonWriteTopLevelError(out, "read-gpa", f, "Failed to write --out file", STATUS_UNSUCCESSFUL);
        return 2;
      }
      wroteFile = true;
    }
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("read-gpa");
  w.Key("ok");
  w.Bool(ok);

  w.Key("request");
  w.BeginObject();
  JsonWriteU64HexDec(w, "gpa", gpa);
  w.Key("size_bytes");
  w.Uint32(sizeBytes);
  w.EndObject();

  w.Key("response");
  w.BeginObject();
  w.Key("status");
  JsonWriteNtStatusError(w, f, op);
  w.Key("bytes_copied");
  w.Uint32(copied);
  w.Key("bytes_copied_reported");
  w.Uint32(io.bytes_copied);
  w.Key("partial_copy");
  w.Bool(op == STATUS_PARTIAL_COPY);
  if (outFile && *outFile) {
    w.Key("out_path");
    w.String(WideToUtf8(outFile));
    w.Key("out_written");
    w.Bool(wroteFile);
  }
  w.Key("data_hex");
  w.String(BytesToHex(io.data, copied));
  w.EndObject();

  w.EndObject();
  out->push_back('\n');

  if (op == STATUS_PARTIAL_COPY) {
    return 3;
  }
  return NT_SUCCESS(op) ? 0 : 2;
}

static int DoQueryFenceJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_query_fence_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "query-fence", f, "D3DKMTEscape(query-fence) failed", st);
    return 2;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-fence");
  w.Key("ok");
  w.Bool(true);
  w.Key("fences");
  w.BeginObject();
  JsonWriteU64HexDec(w, "last_submitted_fence", q.last_submitted_fence);
  JsonWriteU64HexDec(w, "last_completed_fence", q.last_completed_fence);
  JsonWriteU64HexDec(w, "error_irq_count", q.error_irq_count);
  JsonWriteU64HexDec(w, "last_error_fence", q.last_error_fence);
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoWatchFenceJson(const D3DKMT_FUNCS *f,
                            D3DKMT_HANDLE hAdapter,
                            uint32_t samples,
                            uint32_t intervalMs,
                            uint32_t overallTimeoutMs,
                            std::string *out) {
  // Stall threshold: warn after ~2 seconds of no completed-fence progress while work is pending.
  static const uint32_t kStallWarnTimeMs = 2000;
  // JSON mode builds the entire payload in memory; keep output bounded to avoid huge allocations.
  static const uint32_t kJsonMaxSamples = 10000;

  if (!out) {
    return 1;
  }
  if (samples == 0) {
    JsonWriteTopLevelError(out, "watch-fence", f, "--watch-fence requires --samples N", STATUS_INVALID_PARAMETER);
    return 1;
  }
  const uint32_t requestedSamples = samples;
  const uint32_t requestedIntervalMs = intervalMs;
  if (samples > kJsonMaxSamples) {
    samples = kJsonMaxSamples;
  }

  LARGE_INTEGER freq;
  if (!QueryPerformanceFrequency(&freq) || freq.QuadPart <= 0) {
    JsonWriteTopLevelError(out, "watch-fence", f, "QueryPerformanceFrequency failed", STATUS_INVALID_PARAMETER);
    return 1;
  }

  const uint32_t stallWarnIntervals =
      (intervalMs != 0) ? ((kStallWarnTimeMs + intervalMs - 1) / intervalMs) : 3;

  LARGE_INTEGER start;
  QueryPerformanceCounter(&start);

  bool havePrev = false;
  uint64_t prevSubmitted = 0;
  uint64_t prevCompleted = 0;
  LARGE_INTEGER prevTime;
  ZeroMemory(&prevTime, sizeof(prevTime));
  uint32_t stallIntervals = 0;

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("watch-fence");
  w.Key("samples_requested");
  w.Uint32(requestedSamples);
  w.Key("samples_effective");
  w.Uint32(samples);
  w.Key("interval_ms_requested");
  w.Uint32(requestedIntervalMs);
  w.Key("interval_ms");
  w.Uint32(intervalMs);
  w.Key("overall_timeout_ms");
  w.Uint32(overallTimeoutMs);
  w.Key("samples");
  w.BeginArray();

  for (uint32_t i = 0; i < samples; ++i) {
    LARGE_INTEGER before;
    QueryPerformanceCounter(&before);
    const double elapsedMs =
        (double)(before.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;

    if (overallTimeoutMs != 0 && elapsedMs >= (double)overallTimeoutMs) {
      w.EndArray();
      w.Key("ok");
      w.Bool(false);
      w.Key("error");
      w.BeginObject();
      w.Key("message");
      w.String("watch-fence: overall timeout");
      w.Key("sample_index");
      w.Uint32(i + 1);
      w.Key("status");
      JsonWriteNtStatusError(w, f, STATUS_TIMEOUT);
      w.EndObject();
      w.EndObject();
      out->push_back('\n');
      return 2;
    }

    aerogpu_escape_query_fence_out q;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;

    const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
    if (!NT_SUCCESS(st)) {
      w.EndArray();
      w.Key("ok");
      w.Bool(false);
      w.Key("error");
      w.BeginObject();
      w.Key("message");
      w.String("D3DKMTEscape(query-fence) failed");
      w.Key("status");
      JsonWriteNtStatusError(w, f, st);
      w.EndObject();
      w.EndObject();
      out->push_back('\n');
      return 2;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double tMs = (double)(now.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;

    aerogpu_fence_delta_stats delta;
    ZeroMemory(&delta, sizeof(delta));
    double dtMs = 0.0;
    if (havePrev) {
      const double dtSeconds = (double)(now.QuadPart - prevTime.QuadPart) / (double)freq.QuadPart;
      dtMs = dtSeconds * 1000.0;
      delta = aerogpu_fence_compute_delta(prevSubmitted, prevCompleted, q.last_submitted_fence, q.last_completed_fence,
                                          dtSeconds);
    } else {
      delta.delta_submitted = 0;
      delta.delta_completed = 0;
      delta.completed_per_s = 0.0;
      delta.reset = 0;
    }

    const bool hasPending =
        (q.last_submitted_fence > q.last_completed_fence) && (!delta.reset || !havePrev);
    if (havePrev && !delta.reset && hasPending && delta.delta_completed == 0) {
      stallIntervals += 1;
    } else {
      stallIntervals = 0;
    }

    const bool warnStall = (stallIntervals != 0 && stallIntervals >= stallWarnIntervals);
    const char *warn = "-";
    if (havePrev && delta.reset) {
      warn = "RESET";
    } else if (warnStall) {
      warn = "STALL";
    }

    const uint64_t pending =
        (q.last_submitted_fence >= q.last_completed_fence) ? (q.last_submitted_fence - q.last_completed_fence) : 0;

    w.BeginObject();
    w.Key("index");
    w.Uint32(i + 1);
    w.Key("t_ms");
    w.Double(tMs);
    w.Key("fences");
    w.BeginObject();
    JsonWriteU64HexDec(w, "submitted", q.last_submitted_fence);
    JsonWriteU64HexDec(w, "completed", q.last_completed_fence);
    w.Key("pending");
    w.String(DecU64(pending));
    JsonWriteU64HexDec(w, "error_irq_count", q.error_irq_count);
    JsonWriteU64HexDec(w, "last_error_fence", q.last_error_fence);
    w.EndObject();
    w.Key("delta");
    w.BeginObject();
    w.Key("d_submitted");
    w.String(DecU64(delta.delta_submitted));
    w.Key("d_completed");
    w.String(DecU64(delta.delta_completed));
    w.Key("dt_ms");
    w.Double(dtMs);
    w.Key("completed_per_s");
    w.Double(delta.completed_per_s);
    w.Key("reset");
    w.Bool(!!delta.reset);
    w.EndObject();
    w.Key("stall_intervals");
    w.Uint32(stallIntervals);
    w.Key("warn");
    w.String(warn);
    w.EndObject();

    prevSubmitted = q.last_submitted_fence;
    prevCompleted = q.last_completed_fence;
    prevTime = now;
    havePrev = true;

    if (i + 1 < samples && intervalMs != 0) {
      DWORD sleepMs = intervalMs;
      if (overallTimeoutMs != 0) {
        LARGE_INTEGER preSleep;
        QueryPerformanceCounter(&preSleep);
        const double elapsedMs2 =
            (double)(preSleep.QuadPart - start.QuadPart) * 1000.0 / (double)freq.QuadPart;
        if (elapsedMs2 >= (double)overallTimeoutMs) {
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("watch-fence: overall timeout");
          w.Key("sample_index");
          w.Uint32(i + 1);
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_TIMEOUT);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return 2;
        }
        const double remainingMs = (double)overallTimeoutMs - elapsedMs2;
        if (remainingMs < (double)sleepMs) {
          sleepMs = (DWORD)remainingMs;
        }
      }
      if (sleepMs != 0) {
        Sleep(sleepMs);
      }
    }
  }

  w.EndArray();
  w.Key("ok");
  w.Bool(true);
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryPerfJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_query_perf_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_PERF;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "query-perf", f, "D3DKMTEscape(query-perf) failed", st);
    return 2;
  }

  const bool errorLatched = (q.reserved0 & 0x80000000u) != 0;
  const uint32_t lastErrorTime10ms = (q.reserved0 & 0x7FFFFFFFu);

  bool haveErrorIrq = false;
  uint64_t errorIrqCount = 0;
  uint64_t lastErrorFence = 0;

  if (q.hdr.size >= offsetof(aerogpu_escape_query_perf_out, last_error_fence) + sizeof(q.last_error_fence)) {
    haveErrorIrq = true;
    errorIrqCount = (uint64_t)q.error_irq_count;
    lastErrorFence = (uint64_t)q.last_error_fence;
  } else {
    // Backward compatibility: older KMD builds may not include the appended error IRQ fields
    // in QUERY_PERF; fall back to QUERY_FENCE if available.
    aerogpu_escape_query_fence_out qf;
    ZeroMemory(&qf, sizeof(qf));
    qf.hdr.version = AEROGPU_ESCAPE_VERSION;
    qf.hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
    qf.hdr.size = sizeof(qf);
    qf.hdr.reserved0 = 0;
    const NTSTATUS stFence = SendAerogpuEscape(f, hAdapter, &qf, sizeof(qf));
    if (NT_SUCCESS(stFence)) {
      haveErrorIrq = true;
      errorIrqCount = (uint64_t)qf.error_irq_count;
      lastErrorFence = (uint64_t)qf.last_error_fence;
    }
  }

  const uint64_t submitted = (uint64_t)q.last_submitted_fence;
  const uint64_t completed = (uint64_t)q.last_completed_fence;
  const uint64_t pendingFences = (submitted >= completed) ? (submitted - completed) : 0;

  uint32_t ringPending = 0;
  if (q.ring0_entry_count != 0) {
    const uint32_t head = q.ring0_head;
    const uint32_t tail = q.ring0_tail;
    if (tail >= head) {
      ringPending = tail - head;
    } else {
      ringPending = tail + q.ring0_entry_count - head;
    }
    if (ringPending > q.ring0_entry_count) {
      ringPending = q.ring0_entry_count;
    }
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-perf");
  w.Key("ok");
  w.Bool(true);

  w.Key("fences");
  w.BeginObject();
  JsonWriteU64HexDec(w, "last_submitted_fence", submitted);
  JsonWriteU64HexDec(w, "last_completed_fence", completed);
  w.Key("pending");
  w.String(DecU64(pendingFences));
  if (haveErrorIrq) {
    JsonWriteU64HexDec(w, "error_irq_count", errorIrqCount);
    JsonWriteU64HexDec(w, "last_error_fence", lastErrorFence);
  }
  w.EndObject();

  w.Key("ring0");
  w.BeginObject();
  w.Key("head");
  w.Uint32(q.ring0_head);
  w.Key("tail");
  w.Uint32(q.ring0_tail);
  w.Key("pending");
  w.Uint32(ringPending);
  w.Key("entry_count");
  w.Uint32(q.ring0_entry_count);
  w.Key("size_bytes");
  w.Uint32(q.ring0_size_bytes);
  w.EndObject();

  w.Key("submits");
  w.BeginObject();
  JsonWriteU64HexDec(w, "total", q.total_submissions);
  JsonWriteU64HexDec(w, "render", q.total_render_submits);
  JsonWriteU64HexDec(w, "present", q.total_presents);
  JsonWriteU64HexDec(w, "internal", q.total_internal_submits);
  w.EndObject();

  w.Key("irqs");
  w.BeginObject();
  JsonWriteU64HexDec(w, "fence_delivered", q.irq_fence_delivered);
  JsonWriteU64HexDec(w, "vblank_delivered", q.irq_vblank_delivered);
  JsonWriteU64HexDec(w, "spurious", q.irq_spurious);
  JsonWriteU64HexDec(w, "error_irq_count", q.error_irq_count);
  JsonWriteU64HexDec(w, "last_error_fence", q.last_error_fence);
  w.EndObject();

  w.Key("resets");
  w.BeginObject();
  JsonWriteU64HexDec(w, "reset_from_timeout_count", q.reset_from_timeout_count);
  JsonWriteU64HexDec(w, "last_reset_time_100ns", q.last_reset_time_100ns);
  w.EndObject();

  w.Key("device_error");
  w.BeginObject();
  w.Key("latched");
  w.Bool(errorLatched);
  w.Key("last_time_10ms");
  w.Uint32(lastErrorTime10ms);
  JsonWriteU32Hex(w, "packed_u32_hex", q.reserved0);
  w.EndObject();

  w.Key("vblank");
  w.BeginObject();
  JsonWriteU64HexDec(w, "seq", q.vblank_seq);
  JsonWriteU64HexDec(w, "last_time_ns", q.last_vblank_time_ns);
  w.Key("period_ns");
  w.Uint32(q.vblank_period_ns);
  w.EndObject();

  // Last error snapshot (best-effort; may not be supported by older KMD/device builds).
  w.Key("last_error");
  w.BeginObject();
  aerogpu_escape_query_error_out qe;
  ZeroMemory(&qe, sizeof(qe));
  qe.hdr.version = AEROGPU_ESCAPE_VERSION;
  qe.hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
  qe.hdr.size = sizeof(qe);
  qe.hdr.reserved0 = 0;
  const NTSTATUS stErr = SendAerogpuEscape(f, hAdapter, &qe, sizeof(qe));
  if (!NT_SUCCESS(stErr)) {
    w.Key("supported");
    w.Bool(false);
    w.Key("error");
    JsonWriteNtStatusError(w, f, stErr);
  } else {
    bool supported = true;
    if ((qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID) != 0) {
      supported = (qe.flags & AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED) != 0;
    }
    w.Key("supported");
    w.Bool(supported);
    JsonWriteU32Hex(w, "flags_u32_hex", qe.flags);
    if (supported) {
      w.Key("error_code");
      w.Uint32(qe.error_code);
      w.Key("error_code_name");
      w.String(WideToUtf8(AerogpuErrorCodeName(qe.error_code)));
      JsonWriteU64HexDec(w, "error_fence", qe.error_fence);
      w.Key("error_count");
      w.Uint32(qe.error_count);
    }
  }
  w.EndObject();

  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryScanoutJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId,
                             std::string *out) {
  if (!out) {
    return 1;
  }

  const uint32_t requested = vidpnSourceId;
  bool fallbackToSource0 = false;

  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = requested;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && requested != 0) {
    fallbackToSource0 = true;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "query-scanout", f, "D3DKMTEscape(query-scanout) failed", st);
    return 2;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-scanout");
  w.Key("ok");
  w.Bool(true);
  w.Key("vidpn_source_id_requested");
  w.Uint32(requested);
  w.Key("vidpn_source_id");
  w.Uint32(q.vidpn_source_id);
  w.Key("fallback_to_source0");
  w.Bool(fallbackToSource0);
  w.Key("scanout");
  w.BeginObject();
  w.Key("cached");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.cached_enable);
  w.Key("width");
  w.Uint32(q.cached_width);
  w.Key("height");
  w.Uint32(q.cached_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.cached_format));
  w.Key("pitch_bytes");
  w.Uint32(q.cached_pitch_bytes);
  w.EndObject();
  w.Key("mmio");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.mmio_enable);
  w.Key("width");
  w.Uint32(q.mmio_width);
  w.Key("height");
  w.Uint32(q.mmio_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.mmio_format));
  w.Key("pitch_bytes");
  w.Uint32(q.mmio_pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.mmio_fb_gpa));
  w.EndObject();
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryCursorJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "query-cursor", f, "D3DKMTEscape(query-cursor) failed", st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }
  if (!supported) {
    // Surface a consistent machine-detectable failure.
    JsonWriteTopLevelError(out, "query-cursor", f, "Cursor not supported", STATUS_NOT_SUPPORTED);
    return 2;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-cursor");
  w.Key("ok");
  w.Bool(true);
  w.Key("cursor");
  w.BeginObject();
  JsonWriteU32Hex(w, "flags_u32_hex", q.flags);
  w.Key("enable");
  w.Uint32(q.enable);
  w.Key("x");
  w.Int32((int32_t)q.x);
  w.Key("y");
  w.Int32((int32_t)q.y);
  w.Key("hot_x");
  w.Uint32(q.hot_x);
  w.Key("hot_y");
  w.Uint32(q.hot_y);
  w.Key("width");
  w.Uint32(q.width);
  w.Key("height");
  w.Uint32(q.height);
  w.Key("format");
  w.String(AerogpuFormatName(q.format));
  w.Key("pitch_bytes");
  w.Uint32(q.pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.fb_gpa));
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoDumpScanoutBmpJson(const D3DKMT_FUNCS *f,
                                D3DKMT_HANDLE hAdapter,
                                uint32_t vidpnSourceId,
                                const wchar_t *path,
                                std::string *out) {
  if (!out) {
    return 1;
  }
  if (!path || path[0] == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-bmp", f, "--dump-scanout-bmp requires a non-empty path",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  const uint32_t requested = vidpnSourceId;
  bool fallbackToSource0 = false;

  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = requested;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && requested != 0) {
    fallbackToSource0 = true;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "dump-scanout-bmp", f, "D3DKMTEscape(query-scanout) failed", st);
    return 2;
  }

  // Prefer MMIO snapshot values (these reflect what the device is actually using).
  const uint32_t enable = (q.mmio_enable != 0) ? q.mmio_enable : q.cached_enable;
  const uint32_t width = (q.mmio_width != 0) ? q.mmio_width : q.cached_width;
  const uint32_t height = (q.mmio_height != 0) ? q.mmio_height : q.cached_height;
  const uint32_t format = (q.mmio_format != 0) ? q.mmio_format : q.cached_format;
  const uint32_t pitchBytes = (q.mmio_pitch_bytes != 0) ? q.mmio_pitch_bytes : q.cached_pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.mmio_fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-bmp", f, "Scanout has invalid mode (width/height/pitch is 0)",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }
  if (fbGpa == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-bmp", f, "Scanout MMIO framebuffer GPA is 0; cannot dump framebuffer",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }

  wchar_t label[32];
  swprintf_s(label, sizeof(label) / sizeof(label[0]), L"scanout%lu", (unsigned long)q.vidpn_source_id);
  const int rc = DumpLinearFramebufferToBmp(f, hAdapter, label, width, height, format, pitchBytes, fbGpa, path, true);
  if (rc != 0) {
    JsonWriteTopLevelError(out, "dump-scanout-bmp", f, "Failed to dump scanout framebuffer to BMP", STATUS_UNSUCCESSFUL);
    return rc;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-scanout-bmp");
  w.Key("ok");
  w.Bool(true);
  w.Key("vidpn_source_id_requested");
  w.Uint32(requested);
  w.Key("vidpn_source_id");
  w.Uint32(q.vidpn_source_id);
  w.Key("fallback_to_source0");
  w.Bool(fallbackToSource0);
  w.Key("scanout");
  w.BeginObject();
  w.Key("cached");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.cached_enable);
  w.Key("width");
  w.Uint32(q.cached_width);
  w.Key("height");
  w.Uint32(q.cached_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.cached_format));
  w.Key("pitch_bytes");
  w.Uint32(q.cached_pitch_bytes);
  w.EndObject();
  w.Key("mmio");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.mmio_enable);
  w.Key("width");
  w.Uint32(q.mmio_width);
  w.Key("height");
  w.Uint32(q.mmio_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.mmio_format));
  w.Key("pitch_bytes");
  w.Uint32(q.mmio_pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.mmio_fb_gpa));
  w.EndObject();
  w.Key("selected");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(enable);
  w.Key("width");
  w.Uint32(width);
  w.Key("height");
  w.Uint32(height);
  w.Key("format");
  w.String(AerogpuFormatName(format));
  w.Key("pitch_bytes");
  w.Uint32(pitchBytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(fbGpa));
  w.EndObject();
  w.EndObject();
  w.Key("output");
  w.BeginObject();
  w.Key("type");
  w.String("bmp");
  w.Key("path");
  w.String(WideToUtf8(path));
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoDumpScanoutPngJson(const D3DKMT_FUNCS *f,
                                D3DKMT_HANDLE hAdapter,
                                uint32_t vidpnSourceId,
                                const wchar_t *path,
                                std::string *out) {
  if (!out) {
    return 1;
  }
  if (!path || path[0] == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-png", f, "--dump-scanout-png requires a non-empty path",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  const uint32_t requested = vidpnSourceId;
  bool fallbackToSource0 = false;

  aerogpu_escape_query_scanout_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.vidpn_source_id = requested;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && requested != 0) {
    fallbackToSource0 = true;
    ZeroMemory(&q, sizeof(q));
    q.hdr.version = AEROGPU_ESCAPE_VERSION;
    q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    q.hdr.size = sizeof(q);
    q.hdr.reserved0 = 0;
    q.vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  }
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "dump-scanout-png", f, "D3DKMTEscape(query-scanout) failed", st);
    return 2;
  }

  // Prefer MMIO snapshot values (these reflect what the device is actually using).
  const uint32_t enable = (q.mmio_enable != 0) ? q.mmio_enable : q.cached_enable;
  const uint32_t width = (q.mmio_width != 0) ? q.mmio_width : q.cached_width;
  const uint32_t height = (q.mmio_height != 0) ? q.mmio_height : q.cached_height;
  const uint32_t format = (q.mmio_format != 0) ? q.mmio_format : q.cached_format;
  const uint32_t pitchBytes = (q.mmio_pitch_bytes != 0) ? q.mmio_pitch_bytes : q.cached_pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.mmio_fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-png", f, "Scanout has invalid mode (width/height/pitch is 0)",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }
  if (fbGpa == 0) {
    JsonWriteTopLevelError(out, "dump-scanout-png", f, "Scanout MMIO framebuffer GPA is 0; cannot dump framebuffer",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }

  wchar_t label[32];
  swprintf_s(label, sizeof(label) / sizeof(label[0]), L"scanout%lu", (unsigned long)q.vidpn_source_id);
  const int rc = DumpLinearFramebufferToPng(f, hAdapter, label, width, height, format, pitchBytes, fbGpa, path, true);
  if (rc != 0) {
    JsonWriteTopLevelError(out, "dump-scanout-png", f, "Failed to dump scanout framebuffer to PNG", STATUS_UNSUCCESSFUL);
    return rc;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-scanout-png");
  w.Key("ok");
  w.Bool(true);
  w.Key("vidpn_source_id_requested");
  w.Uint32(requested);
  w.Key("vidpn_source_id");
  w.Uint32(q.vidpn_source_id);
  w.Key("fallback_to_source0");
  w.Bool(fallbackToSource0);
  w.Key("scanout");
  w.BeginObject();
  w.Key("cached");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.cached_enable);
  w.Key("width");
  w.Uint32(q.cached_width);
  w.Key("height");
  w.Uint32(q.cached_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.cached_format));
  w.Key("pitch_bytes");
  w.Uint32(q.cached_pitch_bytes);
  w.EndObject();
  w.Key("mmio");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(q.mmio_enable);
  w.Key("width");
  w.Uint32(q.mmio_width);
  w.Key("height");
  w.Uint32(q.mmio_height);
  w.Key("format");
  w.String(AerogpuFormatName(q.mmio_format));
  w.Key("pitch_bytes");
  w.Uint32(q.mmio_pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.mmio_fb_gpa));
  w.EndObject();
  w.Key("selected");
  w.BeginObject();
  w.Key("enable");
  w.Uint32(enable);
  w.Key("width");
  w.Uint32(width);
  w.Key("height");
  w.Uint32(height);
  w.Key("format");
  w.String(AerogpuFormatName(format));
  w.Key("pitch_bytes");
  w.Uint32(pitchBytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(fbGpa));
  w.EndObject();
  w.EndObject();
  w.Key("output");
  w.BeginObject();
  w.Key("type");
  w.String("png");
  w.Key("path");
  w.String(WideToUtf8(path));
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoDumpCursorBmpJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, const wchar_t *path, std::string *out) {
  if (!out) {
    return 1;
  }
  if (!path || path[0] == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "--dump-cursor-bmp requires a non-empty path",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "D3DKMTEscape(query-cursor) failed", st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }
  if (!supported) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "Cursor not supported", STATUS_NOT_SUPPORTED);
    return 2;
  }

  const uint32_t width = (uint32_t)q.width;
  const uint32_t height = (uint32_t)q.height;
  const uint32_t format = (uint32_t)q.format;
  const uint32_t pitchBytes = (uint32_t)q.pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "Cursor has invalid mode (width/height/pitch is 0)",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }
  if (fbGpa == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "Cursor framebuffer GPA is 0; cannot dump cursor",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }

  const int rc = DumpLinearFramebufferToBmp(f, hAdapter, L"cursor", width, height, format, pitchBytes, fbGpa, path, true);
  if (rc != 0) {
    JsonWriteTopLevelError(out, "dump-cursor-bmp", f, "Failed to dump cursor framebuffer to BMP", STATUS_UNSUCCESSFUL);
    return rc;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-cursor-bmp");
  w.Key("ok");
  w.Bool(true);
  w.Key("cursor");
  w.BeginObject();
  JsonWriteU32Hex(w, "flags_u32_hex", q.flags);
  w.Key("enable");
  w.Uint32(q.enable);
  w.Key("x");
  w.Int32((int32_t)q.x);
  w.Key("y");
  w.Int32((int32_t)q.y);
  w.Key("hot_x");
  w.Uint32(q.hot_x);
  w.Key("hot_y");
  w.Uint32(q.hot_y);
  w.Key("width");
  w.Uint32(q.width);
  w.Key("height");
  w.Uint32(q.height);
  w.Key("format");
  w.String(AerogpuFormatName(q.format));
  w.Key("pitch_bytes");
  w.Uint32(q.pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.fb_gpa));
  w.EndObject();
  w.Key("output");
  w.BeginObject();
  w.Key("type");
  w.String("bmp");
  w.Key("path");
  w.String(WideToUtf8(path));
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoDumpCursorPngJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, const wchar_t *path, std::string *out) {
  if (!out) {
    return 1;
  }
  if (!path || path[0] == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "--dump-cursor-png requires a non-empty path",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  aerogpu_escape_query_cursor_out q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "D3DKMTEscape(query-cursor) failed", st);
    return 2;
  }

  bool supported = true;
  if ((q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID) != 0) {
    supported = (q.flags & AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED) != 0;
  }
  if (!supported) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "Cursor not supported", STATUS_NOT_SUPPORTED);
    return 2;
  }

  const uint32_t width = (uint32_t)q.width;
  const uint32_t height = (uint32_t)q.height;
  const uint32_t format = (uint32_t)q.format;
  const uint32_t pitchBytes = (uint32_t)q.pitch_bytes;
  const uint64_t fbGpa = (uint64_t)q.fb_gpa;

  if (width == 0 || height == 0 || pitchBytes == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "Cursor has invalid mode (width/height/pitch is 0)",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }
  if (fbGpa == 0) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "Cursor framebuffer GPA is 0; cannot dump cursor",
                           STATUS_INVALID_PARAMETER);
    return 2;
  }

  const int rc = DumpLinearFramebufferToPng(f, hAdapter, L"cursor", width, height, format, pitchBytes, fbGpa, path, true);
  if (rc != 0) {
    JsonWriteTopLevelError(out, "dump-cursor-png", f, "Failed to dump cursor framebuffer to PNG", STATUS_UNSUCCESSFUL);
    return rc;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-cursor-png");
  w.Key("ok");
  w.Bool(true);
  w.Key("cursor");
  w.BeginObject();
  JsonWriteU32Hex(w, "flags_u32_hex", q.flags);
  w.Key("enable");
  w.Uint32(q.enable);
  w.Key("x");
  w.Int32((int32_t)q.x);
  w.Key("y");
  w.Int32((int32_t)q.y);
  w.Key("hot_x");
  w.Uint32(q.hot_x);
  w.Key("hot_y");
  w.Uint32(q.hot_y);
  w.Key("width");
  w.Uint32(q.width);
  w.Key("height");
  w.Uint32(q.height);
  w.Key("format");
  w.String(AerogpuFormatName(q.format));
  w.Key("pitch_bytes");
  w.Uint32(q.pitch_bytes);
  w.Key("fb_gpa_hex");
  w.String(HexU64(q.fb_gpa));
  w.EndObject();
  w.Key("output");
  w.BeginObject();
  w.Key("type");
  w.String("png");
  w.Key("path");
  w.String(WideToUtf8(path));
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static bool WriteCreateAllocationCsvJson(const wchar_t *path, const aerogpu_escape_dump_createallocation_inout &q,
                                        int *errnoOut) {
  if (errnoOut) {
    *errnoOut = 0;
  }
  if (!path) {
    if (errnoOut) {
      *errnoOut = EINVAL;
    }
    return false;
  }

  FILE *fp = _wfopen(path, L"w");
  if (!fp) {
    if (errnoOut) {
      *errnoOut = errno;
    }
    return false;
  }

  fprintf(fp,
          "write_index,entry_count,entry_capacity,seq,call_seq,alloc_index,num_allocations,create_flags,alloc_id,"
          "priv_flags,pitch_bytes,share_token,size_bytes,flags_in,flags_out\n");

  for (uint32_t i = 0; i < q.entry_count && i < q.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    fprintf(fp,
            "%lu,%lu,%lu,%lu,%lu,%lu,%lu,0x%08lx,%lu,0x%08lx,%lu,0x%016I64x,%I64u,0x%08lx,0x%08lx\n",
            (unsigned long)q.write_index, (unsigned long)q.entry_count, (unsigned long)q.entry_capacity,
            (unsigned long)e.seq, (unsigned long)e.call_seq, (unsigned long)e.alloc_index, (unsigned long)e.num_allocations,
            (unsigned long)e.create_flags, (unsigned long)e.alloc_id, (unsigned long)e.priv_flags,
            (unsigned long)e.pitch_bytes, (unsigned long long)e.share_token, (unsigned long long)e.size_bytes,
            (unsigned long)e.flags_in, (unsigned long)e.flags_out);
  }

  fclose(fp);
  return true;
}

static int DoDumpCreateAllocationJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, const wchar_t *csvPath,
                                     std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_dump_createallocation_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.write_index = 0;
  q.entry_count = 0;
  q.entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
  q.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "dump-createalloc", f, "D3DKMTEscape(dump-createalloc) failed", st);
    return 2;
  }

  bool csvWritten = false;
  int csvErrno = 0;
  if (csvPath) {
    csvWritten = WriteCreateAllocationCsvJson(csvPath, q, &csvErrno);
    if (!csvWritten) {
      JsonWriteTopLevelErrno(out, "dump-createalloc", "Failed to write --csv output", csvErrno);
      return 2;
    }
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-createalloc");
  w.Key("ok");
  w.Bool(true);
  w.Key("write_index");
  w.Uint32(q.write_index);
  w.Key("entry_count");
  w.Uint32(q.entry_count);
  w.Key("entry_capacity");
  w.Uint32(q.entry_capacity);
  if (csvPath) {
    w.Key("csv_path");
    w.String(WideToUtf8(csvPath));
    w.Key("csv_written");
    w.Bool(csvWritten);
  }
  w.Key("entries");
  w.BeginArray();
  for (uint32_t i = 0; i < q.entry_count && i < q.entry_capacity && i < AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS; ++i) {
    const aerogpu_dbgctl_createallocation_desc &e = q.entries[i];
    w.BeginObject();
    w.Key("index");
    w.Uint32(i);
    w.Key("seq");
    w.Uint32(e.seq);
    w.Key("call_seq");
    w.Uint32(e.call_seq);
    JsonWriteU32Hex(w, "create_flags_u32_hex", e.create_flags);
    w.Key("alloc_index");
    w.Uint32(e.alloc_index);
    w.Key("num_allocations");
    w.Uint32(e.num_allocations);
    w.Key("alloc_id");
    w.Uint32(e.alloc_id);
    w.Key("share_token_hex");
    w.String(HexU64(e.share_token));
    JsonWriteU64HexDec(w, "size_bytes", e.size_bytes);
    JsonWriteU32Hex(w, "priv_flags_u32_hex", e.priv_flags);
    w.Key("pitch_bytes");
    w.Uint32(e.pitch_bytes);
    JsonWriteU32Hex(w, "flags_in_u32_hex", e.flags_in);
    JsonWriteU32Hex(w, "flags_out_u32_hex", e.flags_out);
    w.EndObject();
  }
  w.EndArray();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryUmdPrivateJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }
  if (!f->QueryAdapterInfo) {
    JsonWriter w(out);
    w.BeginObject();
    w.Key("schema_version");
    w.Uint32(1);
    w.Key("command");
    w.String("query-umd-private");
    w.Key("ok");
    w.Bool(false);
    w.Key("error");
    w.BeginObject();
    w.Key("message");
    w.String("D3DKMTQueryAdapterInfo not available (missing gdi32 export)");
    w.EndObject();
    w.EndObject();
    out->push_back('\n');
    return 1;
  }

  aerogpu_umd_private_v1 blob;
  ZeroMemory(&blob, sizeof(blob));

  UINT foundType = 0xFFFFFFFFu;
  NTSTATUS lastStatus = 0;
  for (UINT type = 0; type < 256; ++type) {
    ZeroMemory(&blob, sizeof(blob));
    const NTSTATUS st = QueryAdapterInfoWithTimeout(f, hAdapter, type, &blob, sizeof(blob));
    lastStatus = st;
    if (!NT_SUCCESS(st)) {
      if (st == STATUS_TIMEOUT) {
        break;
      }
      continue;
    }
    if (blob.size_bytes < sizeof(blob) || blob.struct_version != AEROGPU_UMDPRIV_STRUCT_VERSION_V1) {
      continue;
    }
    const uint32_t magic = blob.device_mmio_magic;
    if (magic != 0 && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP && magic != AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
      continue;
    }
    foundType = type;
    break;
  }

  if (foundType == 0xFFFFFFFFu) {
    JsonWriteTopLevelError(out, "query-umd-private", f, "D3DKMTQueryAdapterInfo(UMDRIVERPRIVATE) failed", lastStatus);
    return 2;
  }

  char magicStr[5] = {0, 0, 0, 0, 0};
  {
    const uint32_t m = blob.device_mmio_magic;
    magicStr[0] = (char)((m >> 0) & 0xFF);
    magicStr[1] = (char)((m >> 8) & 0xFF);
    magicStr[2] = (char)((m >> 16) & 0xFF);
    magicStr[3] = (char)((m >> 24) & 0xFF);
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-umd-private");
  w.Key("ok");
  w.Bool(true);
  w.Key("type");
  w.Uint32(foundType);
  w.Key("size_bytes");
  w.Uint32(blob.size_bytes);
  w.Key("struct_version");
  w.Uint32(blob.struct_version);
  w.Key("device_mmio_magic_u32_hex");
  w.String(HexU32(blob.device_mmio_magic));
  w.Key("device_mmio_magic_str");
  w.String(magicStr);
  w.Key("device_abi_version_u32_hex");
  w.String(HexU32(blob.device_abi_version_u32));
  w.Key("device_abi_version");
  w.BeginObject();
  w.Key("major");
  w.Uint32((uint32_t)(blob.device_abi_version_u32 >> 16));
  w.Key("minor");
  w.Uint32((uint32_t)(blob.device_abi_version_u32 & 0xFFFFu));
  w.EndObject();
  w.Key("device_features_u64_hex");
  w.String(HexU64(blob.device_features));
  const std::wstring decoded_features = aerogpu::FormatDeviceFeatureBits(blob.device_features, 0);
  w.Key("decoded_features");
  w.String(WideToUtf8(decoded_features));
  JsonWriteDecodedFeatureList(w, "decoded_features_list", decoded_features);
  w.Key("flags_u32_hex");
  w.String(HexU32(blob.flags));
  w.Key("flags");
  w.BeginObject();
  w.Key("is_legacy");
  w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_IS_LEGACY) != 0);
  w.Key("has_vblank");
  w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_VBLANK) != 0);
  w.Key("has_fence_page");
  w.Bool((blob.flags & AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE) != 0);
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQuerySegmentsJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, std::string *out) {
  if (!out) {
    return 1;
  }
  if (!f->QueryAdapterInfo) {
    JsonWriter w(out);
    w.BeginObject();
    w.Key("schema_version");
    w.Uint32(1);
    w.Key("command");
    w.String("query-segments");
    w.Key("ok");
    w.Bool(false);
    w.Key("error");
    w.BeginObject();
    w.Key("message");
    w.String("D3DKMTQueryAdapterInfo not available (missing gdi32 export)");
    w.EndObject();
    w.EndObject();
    out->push_back('\n');
    return 1;
  }

  UINT queryType = 0;
  DXGK_QUERYSEGMENTOUT *segments = NULL;
  if (!FindQuerySegmentTypeAndData(f, hAdapter, /*segmentCapacity=*/64, &queryType, &segments, NULL) || !segments) {
    JsonWriteTopLevelError(out, "query-segments", f,
                           "Failed to find a working KMTQAITYPE_QUERYSEGMENT value (probing range exhausted)",
                           STATUS_NOT_SUPPORTED);
    return 2;
  }

  UINT groupSizeType = 0;
  DXGK_SEGMENTGROUPSIZE groupSizes;
  const bool haveGroupSizes = FindSegmentGroupSizeTypeAndData(f, hAdapter, segments, &groupSizeType, &groupSizes);

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-segments");
  w.Key("ok");
  w.Bool(true);

  w.Key("query_segment_type");
  w.Uint32(queryType);
  w.Key("segment_count");
  w.Uint32(segments->NbSegments);

  w.Key("paging");
  w.BeginObject();
  w.Key("paging_buffer_private_data_size");
  w.Uint32(segments->PagingBufferPrivateDataSize);
  w.Key("paging_buffer_segment_id");
  w.Uint32(segments->PagingBufferSegmentId);
  JsonWriteBytesAndMiB(w, "paging_buffer_size", (uint64_t)segments->PagingBufferSize);
  w.EndObject();

  w.Key("segments");
  w.BeginArray();
  for (UINT i = 0; i < segments->NbSegments; ++i) {
    const DXGK_SEGMENTDESCRIPTOR &d = segments->pSegmentDescriptor[i];
    w.BeginObject();
    w.Key("index");
    w.Uint32((uint32_t)i);
    w.Key("base_address_hex");
    w.String(HexU64((uint64_t)d.BaseAddress.QuadPart));
    JsonWriteBytesAndMiB(w, "size", (uint64_t)d.Size);
    JsonWriteU32Hex(w, "flags_u32_hex", (uint32_t)d.Flags.Value);
    w.Key("flags");
    w.BeginObject();
    w.Key("aperture");
    w.Bool(d.Flags.Aperture != 0);
    w.Key("cpu_visible");
    w.Bool(d.Flags.CpuVisible != 0);
    w.Key("cache_coherent");
    w.Bool(d.Flags.CacheCoherent != 0);
    w.Key("use_banking");
    w.Bool(d.Flags.UseBanking != 0);
    w.EndObject();
    w.Key("memory_segment_group");
    w.BeginObject();
    w.Key("value");
    w.Uint32((uint32_t)d.MemorySegmentGroup);
    w.Key("name");
    w.String(WideToUtf8(DxgkMemorySegmentGroupToString(d.MemorySegmentGroup)));
    w.EndObject();
    w.EndObject();
  }
  w.EndArray();

  w.Key("segment_group_sizes");
  if (haveGroupSizes) {
    w.BeginObject();
    w.Key("type");
    w.Uint32(groupSizeType);
    JsonWriteBytesAndMiB(w, "local_memory_size", (uint64_t)groupSizes.LocalMemorySize);
    JsonWriteBytesAndMiB(w, "non_local_memory_size", (uint64_t)groupSizes.NonLocalMemorySize);
    w.EndObject();
  } else {
    w.Null();
  }

  w.EndObject();
  out->push_back('\n');

  HeapFree(GetProcessHeap(), 0, segments);
  return 0;
}

static int DoDumpRingJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t ringId, std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_dump_ring_v2_inout q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.ring_id = ringId;
  q2.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  const NTSTATUS st2 = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
  if (NT_SUCCESS(st2)) {
    uint32_t count = q2.desc_count;
    if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
    uint32_t window_start = 0;
    if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU && count != 0) {
      window_start = q2.tail - count;
    }

    const char *fmt = "unknown";
    switch (q2.ring_format) {
    case AEROGPU_DBGCTL_RING_FORMAT_LEGACY:
      fmt = "legacy";
      break;
    case AEROGPU_DBGCTL_RING_FORMAT_AGPU:
      fmt = "agpu";
      break;
    default:
      fmt = "unknown";
      break;
    }

    JsonWriter w(out);
    w.BeginObject();
    w.Key("schema_version");
    w.Uint32(1);
    w.Key("command");
    w.String("dump-ring");
    w.Key("ok");
    w.Bool(true);
    w.Key("ring_id");
    w.Uint32(q2.ring_id);
    w.Key("format");
    w.String(fmt);
    w.Key("ring_size_bytes");
    w.Uint32(q2.ring_size_bytes);
    w.Key("head_u32_hex");
    w.String(HexU32(q2.head));
    w.Key("tail_u32_hex");
    w.String(HexU32(q2.tail));
    w.Key("desc_count");
    w.Uint32(q2.desc_count);
    w.Key("descriptors");
    w.BeginArray();
    for (uint32_t i = 0; i < count; ++i) {
      const aerogpu_dbgctl_ring_desc_v2 *d = &q2.desc[i];
      w.BeginObject();
      w.Key("index");
      w.Uint32(i);
      if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
        w.Key("ring_index");
        w.Uint32(window_start + i);
      }
      JsonWriteU64HexDec(w, "fence", d->fence);
      w.Key("cmd_gpa_hex");
      w.String(HexU64(d->cmd_gpa));
      w.Key("cmd_size_bytes");
      w.Uint32(d->cmd_size_bytes);
      JsonWriteU32Hex(w, "flags_u32_hex", d->flags);
      w.Key("alloc_table_gpa_hex");
      w.String(HexU64(d->alloc_table_gpa));
      w.Key("alloc_table_size_bytes");
      w.Uint32(d->alloc_table_size_bytes);
      w.EndObject();
    }
    w.EndArray();
    w.EndObject();
    out->push_back('\n');
    return 0;
  }

  // Fallback to legacy packet.
  aerogpu_escape_dump_ring_inout q1;
  ZeroMemory(&q1, sizeof(q1));
  q1.hdr.version = AEROGPU_ESCAPE_VERSION;
  q1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
  q1.hdr.size = sizeof(q1);
  q1.hdr.reserved0 = 0;
  q1.ring_id = ringId;
  q1.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  const NTSTATUS st1 = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
  if (!NT_SUCCESS(st1)) {
    // Prefer surfacing the v2 error if it wasn't NOT_SUPPORTED.
    const NTSTATUS stOut = (st2 != STATUS_NOT_SUPPORTED) ? st2 : st1;
    JsonWriteTopLevelError(out, "dump-ring", f, "D3DKMTEscape(dump-ring) failed", stOut);
    return 2;
  }

  uint32_t count = q1.desc_count;
  if (count > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
    count = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-ring");
  w.Key("ok");
  w.Bool(true);
  w.Key("ring_id");
  w.Uint32(q1.ring_id);
  w.Key("format");
  w.String("legacy_v1");
  w.Key("ring_size_bytes");
  w.Uint32(q1.ring_size_bytes);
  w.Key("head_u32_hex");
  w.String(HexU32(q1.head));
  w.Key("tail_u32_hex");
  w.String(HexU32(q1.tail));
  w.Key("desc_count");
  w.Uint32(q1.desc_count);
  w.Key("descriptors");
  w.BeginArray();
  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_dbgctl_ring_desc *d = &q1.desc[i];
    w.BeginObject();
    w.Key("index");
    w.Uint32(i);
    JsonWriteU64HexDec(w, "fence", d->signal_fence);
    w.Key("cmd_gpa_hex");
    w.String(HexU64(d->cmd_gpa));
    w.Key("cmd_size_bytes");
    w.Uint32(d->cmd_size_bytes);
    JsonWriteU32Hex(w, "flags_u32_hex", d->flags);
    w.EndObject();
  }
  w.EndArray();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoWatchRingJson(const D3DKMT_FUNCS *f,
                           D3DKMT_HANDLE hAdapter,
                           uint32_t ringId,
                           uint32_t samples,
                           uint32_t intervalMs,
                           std::string *out) {
  // Stall threshold: warn after ~2 seconds of no observed pending-count change while work is pending.
  static const uint32_t kStallWarnTimeMs = 2000;
  // JSON mode builds the entire payload in memory; keep output bounded to avoid huge allocations.
  static const uint32_t kJsonMaxSamples = 10000;

  if (!out) {
    return 1;
  }
  if (samples == 0 || intervalMs == 0) {
    JsonWriteTopLevelError(out, "watch-ring", f, "--watch-ring requires --samples N and --interval-ms N",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  const uint32_t requestedSamples = samples;
  const uint32_t requestedIntervalMs = intervalMs;
  if (samples > kJsonMaxSamples) {
    samples = kJsonMaxSamples;
  }
  if (intervalMs > 60000u) {
    intervalMs = 60000u;
  }

  // sizeof(aerogpu_legacy_ring_entry) (see drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h).
  static const uint32_t kLegacyRingEntrySizeBytes = 24u;

  const auto TryComputeLegacyPending = [&](uint32_t ringSizeBytes, uint32_t head, uint32_t tail,
                                           uint64_t *pendingOut) -> bool {
    if (!pendingOut) {
      return false;
    }
    if (ringSizeBytes == 0 || (ringSizeBytes % kLegacyRingEntrySizeBytes) != 0) {
      return false;
    }
    const uint32_t entryCount = ringSizeBytes / kLegacyRingEntrySizeBytes;
    if (entryCount == 0 || head >= entryCount || tail >= entryCount) {
      return false;
    }
    if (tail >= head) {
      *pendingOut = (uint64_t)(tail - head);
    } else {
      *pendingOut = (uint64_t)(tail + entryCount - head);
    }
    return true;
  };

  bool decided = false;
  bool useV2 = false;
  uint32_t v2DescCapacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
  bool havePrevPending = false;
  uint64_t prevPending = 0;
  uint32_t stallIntervals = 0;
  const uint32_t stallWarnIntervals = (intervalMs != 0) ? ((kStallWarnTimeMs + intervalMs - 1) / intervalMs) : 3;

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("watch-ring");
  w.Key("ring_id");
  w.Uint32(ringId);
  w.Key("samples_requested");
  w.Uint32(requestedSamples);
  w.Key("samples_effective");
  w.Uint32(samples);
  w.Key("interval_ms_requested");
  w.Uint32(requestedIntervalMs);
  w.Key("interval_ms");
  w.Uint32(intervalMs);
  w.Key("samples");
  w.BeginArray();

  for (uint32_t i = 0; i < samples; ++i) {
    uint32_t head = 0;
    uint32_t tail = 0;
    uint64_t pending = 0;
    const wchar_t *fmtStr = L"unknown";

    bool haveLast = false;
    uint64_t lastFence = 0;
    uint32_t lastFlags = 0;

    if (!decided || useV2) {
      aerogpu_escape_dump_ring_v2_inout q2;
      ZeroMemory(&q2, sizeof(q2));
      q2.hdr.version = AEROGPU_ESCAPE_VERSION;
      q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
      q2.hdr.size = sizeof(q2);
      q2.hdr.reserved0 = 0;
      q2.ring_id = ringId;
      q2.desc_capacity = v2DescCapacity;

      const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));
      if (NT_SUCCESS(st)) {
        decided = true;
        useV2 = true;

        head = q2.head;
        tail = q2.tail;
        fmtStr = RingFormatToString(q2.ring_format);

        if (q2.ring_format == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
          // Monotonic indices (modulo u32 wrap).
          pending = (uint64_t)(uint32_t)(tail - head);

          // v2 AGPU dumps are a recent tail window; newest is last.
          if (q2.desc_count > 0 && q2.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
            const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
            lastFence = (uint64_t)d.fence;
            lastFlags = (uint32_t)d.flags;
            haveLast = true;
          }

          // For watch mode, only ask the KMD to return the newest descriptor.
          v2DescCapacity = 1;
        } else {
          // Legacy (masked indices) or unknown: compute pending best-effort using the legacy ring layout.
          if (!TryComputeLegacyPending(q2.ring_size_bytes, head, tail, &pending)) {
            pending = (uint64_t)(uint32_t)(tail - head);
          }

          // Only report the "last" descriptor if we know we captured the full pending region.
          if (pending != 0 && pending == (uint64_t)q2.desc_count && q2.desc_count > 0 &&
              q2.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
            const aerogpu_dbgctl_ring_desc_v2 &d = q2.desc[q2.desc_count - 1];
            lastFence = (uint64_t)d.fence;
            lastFlags = (uint32_t)d.flags;
            haveLast = true;
          }

          v2DescCapacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
        }
      } else if (st == STATUS_NOT_SUPPORTED) {
        decided = true;
        useV2 = false;
        // Fall through to legacy dump-ring below.
      } else {
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("D3DKMTEscape(dump-ring-v2) failed");
        w.Key("status");
        JsonWriteNtStatusError(w, f, st);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
    }

    if (decided && !useV2) {
      aerogpu_escape_dump_ring_inout q;
      ZeroMemory(&q, sizeof(q));
      q.hdr.version = AEROGPU_ESCAPE_VERSION;
      q.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
      q.hdr.size = sizeof(q);
      q.hdr.reserved0 = 0;
      q.ring_id = ringId;
      q.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

      const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
      if (!NT_SUCCESS(st)) {
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("D3DKMTEscape(dump-ring) failed");
        w.Key("status");
        JsonWriteNtStatusError(w, f, st);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }

      head = q.head;
      tail = q.tail;

      // Best-effort legacy detection (tail<head wrap requires knowing entry_count).
      bool assumedLegacy = false;
      if (TryComputeLegacyPending(q.ring_size_bytes, head, tail, &pending)) {
        assumedLegacy = true;
      } else {
        pending = (uint64_t)(uint32_t)(tail - head);
      }
      fmtStr = assumedLegacy ? L"legacy" : L"unknown";

      // Only report the "last" descriptor if we know we captured the full pending region.
      if (pending != 0 && pending == (uint64_t)q.desc_count && q.desc_count > 0 &&
          q.desc_count <= AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
        const aerogpu_dbgctl_ring_desc &d = q.desc[q.desc_count - 1];
        lastFence = (uint64_t)d.signal_fence;
        lastFlags = (uint32_t)d.flags;
        haveLast = true;
      }
    }

    const int64_t dPending = havePrevPending ? ((int64_t)pending - (int64_t)prevPending) : 0;
    if (havePrevPending && pending != 0 && pending == prevPending) {
      stallIntervals += 1;
    } else {
      stallIntervals = 0;
    }
    const bool warnStall = (stallIntervals != 0 && stallIntervals >= stallWarnIntervals);

    w.BeginObject();
    w.Key("index");
    w.Uint32(i + 1);
    w.Key("format");
    w.String(WideToUtf8(fmtStr));
    w.Key("head");
    w.Uint32(head);
    w.Key("tail");
    w.Uint32(tail);
    w.Key("pending");
    w.String(DecU64(pending));
    w.Key("d_pending");
    w.String(DecI64(dPending));
    w.Key("stall_intervals");
    w.Uint32(stallIntervals);
    w.Key("warn");
    w.String(warnStall ? "STALL" : "-");
    if (haveLast) {
      w.Key("last");
      w.BeginObject();
      JsonWriteU64HexDec(w, "fence", lastFence);
      JsonWriteU32Hex(w, "flags_u32_hex", lastFlags);
      w.EndObject();
    }
    w.EndObject();

    prevPending = pending;
    havePrevPending = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }

  w.EndArray();
  w.Key("ok");
  w.Bool(true);
  w.Key("used_v2");
  w.Bool(useV2);
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoDumpLastCmdJson(const D3DKMT_FUNCS *f,
                             D3DKMT_HANDLE hAdapter,
                             uint32_t ringId,
                             uint32_t indexFromTail,
                             uint32_t count,
                             const wchar_t *outPath,
                             const wchar_t *allocOutPath,
                             bool force,
                             std::string *out) {
  if (!out) {
    return 1;
  }
  if (!outPath || !outPath[0]) {
    JsonWriteTopLevelError(out,
                           "dump-last-cmd",
                           f,
                           "--dump-last-submit/--dump-last-cmd requires --cmd-out <path> (or --out <path>)",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }
  if (count == 0) {
    JsonWriteTopLevelError(out, "dump-last-cmd", f, "--count must be >= 1", STATUS_INVALID_PARAMETER);
    return 1;
  }

  // Prefer the v2 dump-ring packet (AGPU tail window + alloc_table fields).
  aerogpu_escape_dump_ring_v2_inout q2;
  ZeroMemory(&q2, sizeof(q2));
  q2.hdr.version = AEROGPU_ESCAPE_VERSION;
  q2.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
  q2.hdr.size = sizeof(q2);
  q2.hdr.reserved0 = 0;
  q2.ring_id = ringId;
  q2.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

  aerogpu_escape_dump_ring_inout q1;
  ZeroMemory(&q1, sizeof(q1));
  bool usedV2 = false;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q2, sizeof(q2));

  uint32_t ringFormat = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
  uint32_t head = 0;
  uint32_t tail = 0;
  uint32_t ringSizeBytes = 0;
  uint32_t descCount = 0;

  if (NT_SUCCESS(st)) {
    usedV2 = true;
    ringFormat = q2.ring_format;
    head = q2.head;
    tail = q2.tail;
    ringSizeBytes = q2.ring_size_bytes;
    descCount = q2.desc_count;
    if (descCount > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      descCount = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
  } else if (st == STATUS_NOT_SUPPORTED) {
    // Fallback to legacy dump-ring for older KMDs.
    q1.hdr.version = AEROGPU_ESCAPE_VERSION;
    q1.hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
    q1.hdr.size = sizeof(q1);
    q1.hdr.reserved0 = 0;
    q1.ring_id = ringId;
    q1.desc_capacity = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;

    st = SendAerogpuEscape(f, hAdapter, &q1, sizeof(q1));
    if (!NT_SUCCESS(st)) {
      JsonWriteTopLevelError(out, "dump-last-cmd", f, "D3DKMTEscape(dump-ring) failed", st);
      return 2;
    }

    ringFormat = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
    head = q1.head;
    tail = q1.tail;
    ringSizeBytes = q1.ring_size_bytes;
    descCount = q1.desc_count;
    if (descCount > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS) {
      descCount = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
    }
  } else {
    JsonWriteTopLevelError(out, "dump-last-cmd", f, "D3DKMTEscape(dump-ring-v2) failed", st);
    return 2;
  }

  if (descCount == 0) {
    // Match text-mode behavior: empty ring is not a failure.
    JsonWriter w(out);
    w.BeginObject();
    w.Key("schema_version");
    w.Uint32(1);
    w.Key("command");
    w.String("dump-last-cmd");
    w.Key("ok");
    w.Bool(true);
    w.Key("ring");
    w.BeginObject();
    w.Key("ring_id");
    w.Uint32(ringId);
    w.Key("used_v2");
    w.Bool(usedV2);
    w.Key("format");
    w.String(WideToUtf8(RingFormatToString(ringFormat)));
    w.Key("ring_size_bytes");
    w.Uint32(ringSizeBytes);
    w.Key("head_u32_hex");
    w.String(HexU32(head));
    w.Key("tail_u32_hex");
    w.String(HexU32(tail));
    w.Key("desc_count");
    w.Uint32(0);
    w.EndObject();
    w.Key("request");
    w.BeginObject();
    w.Key("index_from_tail");
    w.Uint32(indexFromTail);
    w.Key("count");
    w.Uint32(count);
    w.Key("count_actual");
    w.Uint32(0);
    w.Key("out_path");
    w.String(WideToUtf8(outPath));
    if (allocOutPath && allocOutPath[0]) {
      w.Key("alloc_out_path");
      w.String(WideToUtf8(allocOutPath));
    }
    w.Key("force");
    w.Bool(force);
    w.EndObject();
    w.Key("dumps");
    w.BeginArray();
    w.EndArray();
    w.Key("note");
    w.String("Ring has no descriptors available");
    w.EndObject();
    out->push_back('\n');
    return 0;
  }

  if (indexFromTail >= descCount) {
    JsonWriteTopLevelError(out, "dump-last-cmd", f, "--index-from-tail out of range", STATUS_INVALID_PARAMETER);
    return 1;
  }

  uint32_t actualCount = count;
  const uint32_t remaining = descCount - indexFromTail;
  if (actualCount > remaining) {
    actualCount = remaining;
  }

  if (allocOutPath && allocOutPath[0] && actualCount > 1) {
    JsonWriteTopLevelError(out, "dump-last-cmd", f, "--alloc-out is not supported with --count > 1",
                           STATUS_INVALID_PARAMETER);
    return 1;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-last-cmd");

  w.Key("ring");
  w.BeginObject();
  w.Key("ring_id");
  w.Uint32(ringId);
  w.Key("used_v2");
  w.Bool(usedV2);
  w.Key("format");
  w.String(WideToUtf8(RingFormatToString(ringFormat)));
  w.Key("ring_size_bytes");
  w.Uint32(ringSizeBytes);
  w.Key("head_u32_hex");
  w.String(HexU32(head));
  w.Key("tail_u32_hex");
  w.String(HexU32(tail));
  w.Key("desc_count");
  w.Uint32(descCount);
  w.EndObject();

  w.Key("request");
  w.BeginObject();
  w.Key("index_from_tail");
  w.Uint32(indexFromTail);
  w.Key("count");
  w.Uint32(count);
  w.Key("count_actual");
  w.Uint32(actualCount);
  w.Key("out_path");
  w.String(WideToUtf8(outPath));
  if (allocOutPath && allocOutPath[0]) {
    w.Key("alloc_out_path");
    w.String(WideToUtf8(allocOutPath));
  }
  w.Key("force");
  w.Bool(force);
  w.EndObject();

  w.Key("dumps");
  w.BeginArray();

  for (uint32_t dumpIndex = 0; dumpIndex < actualCount; ++dumpIndex) {
    const uint32_t curIndexFromTail = indexFromTail + dumpIndex;
    const uint32_t idx = (descCount - 1u) - curIndexFromTail;

    aerogpu_dbgctl_ring_desc_v2 d;
    ZeroMemory(&d, sizeof(d));
    if (usedV2) {
      d = q2.desc[idx];
    } else {
      const aerogpu_dbgctl_ring_desc &d1 = q1.desc[idx];
      d.fence = d1.signal_fence;
      d.cmd_gpa = d1.cmd_gpa;
      d.cmd_size_bytes = d1.cmd_size_bytes;
      d.flags = d1.flags;
      d.alloc_table_gpa = 0;
      d.alloc_table_size_bytes = 0;
      d.reserved0 = 0;
    }

    uint32_t selectedRingIndex = idx;
    if (usedV2 && ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU && tail >= descCount) {
      selectedRingIndex = (tail - descCount) + idx;
    }

    const wchar_t *curOutPath = outPath;
    wchar_t *curOutPathOwned = NULL;
    if (actualCount > 1) {
      curOutPathOwned = HeapBuildIndexedBinPath(outPath, curIndexFromTail);
      if (!curOutPathOwned) {
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Out of memory building output path");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_INSUFFICIENT_RESOURCES);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      curOutPath = curOutPathOwned;
    }

    bool cmdWritten = false;
    uint32_t cmdMagic = 0;
    bool cmdMagicValid = false;
    bool cmdMagicMatches = false;

    const uint64_t cmdGpa = (uint64_t)d.cmd_gpa;
    const uint64_t cmdSizeBytes = (uint64_t)d.cmd_size_bytes;
    if (cmdGpa == 0 && cmdSizeBytes == 0) {
      FILE *fp = NULL;
      errno_t ferr = _wfopen_s(&fp, curOutPath, L"wb");
      if (ferr != 0 || !fp) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Failed to create output file for empty cmd stream");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_UNSUCCESSFUL);
        w.Key("errno");
        w.Int32((int)ferr);
        const char *errStr = strerror((int)ferr);
        if (errStr) {
          w.Key("errno_message");
          w.String(errStr);
        }
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      fclose(fp);
      cmdWritten = true;
    } else {
      if (cmdGpa == 0 || cmdSizeBytes == 0) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Invalid cmd_gpa/cmd_size_bytes pair");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      if (cmdSizeBytes > kDumpLastCmdHardMaxBytes) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Refusing to dump cmd stream (hard cap exceeded)");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      if (cmdSizeBytes > kDumpLastCmdDefaultMaxBytes && !force) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Refusing to dump cmd stream (default cap exceeded; use --force)");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      if (!AddU64NoOverflow(cmdGpa, cmdSizeBytes, NULL)) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Invalid cmd range (overflow)");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }

      cmdMagic = 0;
      const int dumpRc = DumpGpaRangeToFile(f, hAdapter, cmdGpa, cmdSizeBytes, curOutPath, &cmdMagic);
      if (dumpRc != 0) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Failed to dump cmd stream bytes");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_UNSUCCESSFUL);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return dumpRc;
      }
      cmdWritten = true;
      if (cmdSizeBytes >= 4) {
        cmdMagicValid = true;
        cmdMagicMatches = (cmdMagic == AEROGPU_CMD_STREAM_MAGIC);
      }
    }

    std::string summaryPathUtf8;
    {
      wchar_t *summaryPath = HeapWcsCatSuffix(curOutPath, L".txt");
      if (summaryPath) {
        FILE *sf = NULL;
        errno_t serr = _wfopen_s(&sf, summaryPath, L"wt");
        if (serr == 0 && sf) {
          fwprintf(sf, L"ring_id=%lu\n", (unsigned long)ringId);
          fwprintf(sf, L"ring_format=%s\n", RingFormatToString(ringFormat));
          fwprintf(sf, L"head=0x%08lx\n", (unsigned long)head);
          fwprintf(sf, L"tail=0x%08lx\n", (unsigned long)tail);
          fwprintf(sf, L"selected_index_from_tail=%lu\n", (unsigned long)curIndexFromTail);
          fwprintf(sf, L"selected_ring_index=%lu\n", (unsigned long)selectedRingIndex);
          fwprintf(sf, L"fence=0x%I64x\n", (unsigned long long)d.fence);
          fwprintf(sf, L"flags=0x%08lx\n", (unsigned long)d.flags);
          fwprintf(sf, L"cmd_gpa=0x%I64x\n", (unsigned long long)d.cmd_gpa);
          fwprintf(sf, L"cmd_size_bytes=%lu\n", (unsigned long)d.cmd_size_bytes);
          if (ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
            fwprintf(sf, L"alloc_table_gpa=0x%I64x\n", (unsigned long long)d.alloc_table_gpa);
            fwprintf(sf, L"alloc_table_size_bytes=%lu\n", (unsigned long)d.alloc_table_size_bytes);
          }
          fclose(sf);
          summaryPathUtf8 = WideToUtf8(summaryPath);
        }
        HeapFree(GetProcessHeap(), 0, summaryPath);
      }
    }

    std::string allocPathUtf8;
    bool allocTablePresent = false;
    if (ringFormat == AEROGPU_DBGCTL_RING_FORMAT_AGPU) {
      const uint64_t allocGpa = (uint64_t)d.alloc_table_gpa;
      const uint64_t allocSizeBytes = (uint64_t)d.alloc_table_size_bytes;
      if (!(allocGpa == 0 && allocSizeBytes == 0)) {
        allocTablePresent = true;
        if (allocGpa == 0 || allocSizeBytes == 0) {
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("Invalid alloc_table_gpa/alloc_table_size_bytes pair");
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return 2;
        }
        if (allocSizeBytes > kDumpLastCmdHardMaxBytes) {
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("Refusing to dump alloc table (hard cap exceeded)");
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return 2;
        }
        if (allocSizeBytes > kDumpLastCmdDefaultMaxBytes && !force) {
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("Refusing to dump alloc table (default cap exceeded; use --force)");
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return 2;
        }
        if (!AddU64NoOverflow(allocGpa, allocSizeBytes, NULL)) {
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("Invalid alloc table range (overflow)");
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_INVALID_PARAMETER);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return 2;
        }

        const wchar_t *allocPath = NULL;
        wchar_t *allocPathOwned = NULL;
        if (allocOutPath && allocOutPath[0]) {
          allocPath = allocOutPath;
        } else {
          allocPathOwned = HeapWcsCatSuffix(curOutPath, L".alloc_table.bin");
          if (!allocPathOwned) {
            if (curOutPathOwned) {
              HeapFree(GetProcessHeap(), 0, curOutPathOwned);
            }
            w.EndArray();
            w.Key("ok");
            w.Bool(false);
            w.Key("error");
            w.BeginObject();
            w.Key("message");
            w.String("Out of memory building alloc table output path");
            w.Key("status");
            JsonWriteNtStatusError(w, f, STATUS_INSUFFICIENT_RESOURCES);
            w.EndObject();
            w.EndObject();
            out->push_back('\n');
            return 2;
          }
          allocPath = allocPathOwned;
        }

        const int dumpAllocRc = DumpGpaRangeToFile(f, hAdapter, allocGpa, allocSizeBytes, allocPath, NULL);
        if (dumpAllocRc != 0) {
          allocPathUtf8 = WideToUtf8(allocPath);
          if (allocPathOwned) {
            HeapFree(GetProcessHeap(), 0, allocPathOwned);
          }
          if (curOutPathOwned) {
            HeapFree(GetProcessHeap(), 0, curOutPathOwned);
          }
          w.EndArray();
          w.Key("ok");
          w.Bool(false);
          w.Key("error");
          w.BeginObject();
          w.Key("message");
          w.String("Failed to dump alloc table bytes");
          w.Key("status");
          JsonWriteNtStatusError(w, f, STATUS_UNSUCCESSFUL);
          w.EndObject();
          w.EndObject();
          out->push_back('\n');
          return dumpAllocRc;
        }
        allocPathUtf8 = WideToUtf8(allocPath);
        if (allocPathOwned) {
          HeapFree(GetProcessHeap(), 0, allocPathOwned);
        }
      }
    }

    /*
     * Script-friendly behavior: if the caller explicitly requested --alloc-out but this submission
     * has no alloc table (or the ring format doesn't expose one), still create an empty file.
     *
     * This matches the text-mode `DoDumpLastCmd` behavior and keeps decode pipelines simple
     * (callers can always open the file; a zero-length alloc table is a valid "no allocs" case).
     */
    if ((allocOutPath && allocOutPath[0]) && !allocTablePresent && allocPathUtf8.empty()) {
      if (!CreateEmptyFile(allocOutPath)) {
        if (curOutPathOwned) {
          HeapFree(GetProcessHeap(), 0, curOutPathOwned);
        }
        w.EndArray();
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        w.BeginObject();
        w.Key("message");
        w.String("Failed to create empty alloc_out file");
        w.Key("status");
        JsonWriteNtStatusError(w, f, STATUS_UNSUCCESSFUL);
        w.EndObject();
        w.EndObject();
        out->push_back('\n');
        return 2;
      }
      allocPathUtf8 = WideToUtf8(allocOutPath);
    }

    w.BeginObject();
    w.Key("index_from_tail");
    w.Uint32(curIndexFromTail);
    w.Key("ring_index");
    w.Uint32(selectedRingIndex);
    w.Key("descriptor");
    w.BeginObject();
    JsonWriteU64HexDec(w, "fence", (uint64_t)d.fence);
    w.Key("cmd_gpa_hex");
    w.String(HexU64((uint64_t)d.cmd_gpa));
    w.Key("cmd_size_bytes");
    w.Uint32(d.cmd_size_bytes);
    JsonWriteU32Hex(w, "flags_u32_hex", (uint32_t)d.flags);
    w.Key("alloc_table_gpa_hex");
    w.String(HexU64((uint64_t)d.alloc_table_gpa));
    w.Key("alloc_table_size_bytes");
    w.Uint32(d.alloc_table_size_bytes);
    w.EndObject();
    w.Key("output");
    w.BeginObject();
    w.Key("cmd_path");
    w.String(WideToUtf8(curOutPath));
    w.Key("cmd_written");
    w.Bool(cmdWritten);
    if (cmdMagicValid) {
      w.Key("cmd_magic_u32_hex");
      w.String(HexU32(cmdMagic));
      w.Key("cmd_magic_matches");
      w.Bool(cmdMagicMatches);
    }
    if (!summaryPathUtf8.empty()) {
      w.Key("summary_txt_path");
      w.String(summaryPathUtf8);
    }
    if (!allocPathUtf8.empty()) {
      w.Key("alloc_table_path");
      w.String(allocPathUtf8);
      w.Key("alloc_table_written");
      w.Bool(true);
      if (allocOutPath && allocOutPath[0]) {
        // User explicitly requested a path; surface whether the alloc table existed (it may be empty).
        w.Key("alloc_table_present");
        w.Bool(allocTablePresent);
      }
    } else if (allocOutPath && allocOutPath[0]) {
      // User explicitly requested a path; surface whether the alloc table existed.
      w.Key("alloc_table_present");
      w.Bool(allocTablePresent);
    }
    w.EndObject();
    w.EndObject();

    if (curOutPathOwned) {
      HeapFree(GetProcessHeap(), 0, curOutPathOwned);
    }
  }

  w.EndArray();
  w.Key("ok");
  w.Bool(true);
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static bool QueryVblankJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId,
                           aerogpu_escape_query_vblank_out *out, bool *supportedOut, bool *fallbackToSource0) {
  if (fallbackToSource0) {
    *fallbackToSource0 = false;
  }
  ZeroMemory(out, sizeof(*out));
  out->hdr.version = AEROGPU_ESCAPE_VERSION;
  out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
  out->hdr.size = sizeof(*out);
  out->hdr.reserved0 = 0;
  out->vidpn_source_id = vidpnSourceId;

  NTSTATUS st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  if (!NT_SUCCESS(st) && (st == STATUS_INVALID_PARAMETER || st == STATUS_NOT_SUPPORTED) && vidpnSourceId != 0) {
    if (fallbackToSource0) {
      *fallbackToSource0 = true;
    }
    ZeroMemory(out, sizeof(*out));
    out->hdr.version = AEROGPU_ESCAPE_VERSION;
    out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
    out->hdr.size = sizeof(*out);
    out->hdr.reserved0 = 0;
    out->vidpn_source_id = 0;
    st = SendAerogpuEscape(f, hAdapter, out, sizeof(*out));
  }
  if (!NT_SUCCESS(st)) {
    return false;
  }

  if (supportedOut) {
    bool supported = true;
    if ((out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0) {
      supported = (out->flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED) != 0;
    }
    *supportedOut = supported;
  }
  return true;
}

static int DoDumpVblankJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                           uint32_t intervalMs, std::string *out) {
  if (!out) {
    return 1;
  }
  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("dump-vblank");

  w.Key("vidpn_source_id_requested");
  w.Uint32(vidpnSourceId);
  w.Key("samples_requested");
  w.Uint32(samples);
  w.Key("interval_ms");
  w.Uint32(intervalMs);

  w.Key("samples");
  w.BeginArray();

  aerogpu_escape_query_vblank_out prev;
  bool prevSupported = false;
  bool havePrev = false;
  uint32_t stallCount = 0;
  uint64_t perVblankUsMin = 0;
  uint64_t perVblankUsMax = 0;
  uint64_t perVblankUsSum = 0;
  uint64_t perVblankUsSamples = 0;

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  bool scanlineFallbackToSource0 = false;

  for (uint32_t i = 0; i < samples; ++i) {
    aerogpu_escape_query_vblank_out q;
    bool supported = false;
    bool fallbackToSource0 = false;
    if (!QueryVblankJson(f, hAdapter, effectiveVidpnSourceId, &q, &supported, &fallbackToSource0)) {
      // We do not have the NTSTATUS at this point (SendAerogpuEscape already printed it in the text path).
      // Re-run once to capture the error code for JSON.
      aerogpu_escape_query_vblank_out tmp;
      ZeroMemory(&tmp, sizeof(tmp));
      tmp.hdr.version = AEROGPU_ESCAPE_VERSION;
      tmp.hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
      tmp.hdr.size = sizeof(tmp);
      tmp.hdr.reserved0 = 0;
      tmp.vidpn_source_id = effectiveVidpnSourceId;
      const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &tmp, sizeof(tmp));
      w.EndArray();
      w.Key("ok");
      w.Bool(false);
      w.Key("error");
      w.BeginObject();
      w.Key("message");
      w.String("D3DKMTEscape(query-vblank) failed");
      w.Key("status");
      JsonWriteNtStatusError(w, f, st);
      w.EndObject();
      w.EndObject();
      out->push_back('\n');
      return 2;
    }

    effectiveVidpnSourceId = q.vidpn_source_id;

    w.BeginObject();
    w.Key("index");
    w.Uint32(i + 1);
    w.Key("vidpn_source_id");
    w.Uint32(q.vidpn_source_id);
    w.Key("fallback_to_source0");
    w.Bool(fallbackToSource0);
    w.Key("supported");
    w.Bool(supported);
    JsonWriteU32Hex(w, "flags_u32_hex", q.flags);
    JsonWriteU32Hex(w, "irq_enable_u32_hex", q.irq_enable);
    JsonWriteU32Hex(w, "irq_status_u32_hex", q.irq_status);
    JsonWriteU32Hex(w, "irq_active_u32_hex", (uint32_t)(q.irq_enable & q.irq_status));
    if ((q.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID) != 0 &&
        (q.flags & AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID) != 0) {
      w.Key("vblank_interrupt_type");
      w.Uint32(q.vblank_interrupt_type);
    }
    if (supported) {
      w.Key("vblank_period_ns");
      w.Uint32(q.vblank_period_ns);
      JsonWriteU64HexDec(w, "vblank_seq", q.vblank_seq);
      JsonWriteU64HexDec(w, "last_vblank_time_ns", q.last_vblank_time_ns);
    }

    // Optional scanline snapshot.
    if (f->GetScanLine) {
      D3DKMT_GETSCANLINE s;
      ZeroMemory(&s, sizeof(s));
      s.hAdapter = hAdapter;
      s.VidPnSourceId = scanlineFallbackToSource0 ? 0 : effectiveVidpnSourceId;
      NTSTATUS stScan = f->GetScanLine(&s);
      if (!NT_SUCCESS(stScan) && stScan == STATUS_INVALID_PARAMETER && s.VidPnSourceId != 0) {
        scanlineFallbackToSource0 = true;
        s.VidPnSourceId = 0;
        stScan = f->GetScanLine(&s);
      }
      w.Key("scanline");
      w.BeginObject();
      w.Key("vidpn_source_id");
      w.Uint32(s.VidPnSourceId);
      if (NT_SUCCESS(stScan)) {
        w.Key("ok");
        w.Bool(true);
        w.Key("scanline");
        w.Uint32((uint32_t)s.ScanLine);
        w.Key("in_vblank");
        w.Bool(!!s.InVerticalBlank);
      } else {
        w.Key("ok");
        w.Bool(false);
        w.Key("error");
        JsonWriteNtStatusError(w, f, stScan);
      }
      w.EndObject();
    }

    // Delta stats (best-effort; raw counters are already reported).
    if (havePrev && supported && prevSupported) {
      if (q.vblank_seq >= prev.vblank_seq && q.last_vblank_time_ns >= prev.last_vblank_time_ns) {
        const uint64_t dseq = q.vblank_seq - prev.vblank_seq;
        const uint64_t dt = q.last_vblank_time_ns - prev.last_vblank_time_ns;
        w.Key("delta");
        w.BeginObject();
        w.Key("dseq");
        w.String(DecU64(dseq));
        w.Key("dt_ns");
        w.String(DecU64(dt));
        w.EndObject();
        if (dseq != 0 && dt != 0) {
          const uint64_t perVblankUs = (dt / dseq) / 1000ull;
          if (perVblankUsSamples == 0) {
            perVblankUsMin = perVblankUs;
            perVblankUsMax = perVblankUs;
          } else {
            if (perVblankUs < perVblankUsMin) {
              perVblankUsMin = perVblankUs;
            }
            if (perVblankUs > perVblankUsMax) {
              perVblankUsMax = perVblankUs;
            }
          }
          perVblankUsSum += perVblankUs;
          perVblankUsSamples += 1;
        } else if (dseq == 0) {
          stallCount += 1;
        }
      }
    }

    w.EndObject();

    if (!supported) {
      // Match text-mode behavior: fail immediately when vblank isn't supported.
      w.EndArray();
      w.Key("ok");
      w.Bool(false);
      w.Key("error");
      w.BeginObject();
      w.Key("message");
      w.String("Vblank not supported by device/KMD");
      w.Key("status");
      JsonWriteNtStatusError(w, f, STATUS_NOT_SUPPORTED);
      w.EndObject();
      w.EndObject();
      out->push_back('\n');
      return 2;
    }

    prev = q;
    prevSupported = supported;
    havePrev = true;

    if (i + 1 < samples) {
      Sleep(intervalMs);
    }
  }

  w.EndArray();
  w.Key("ok");
  w.Bool(true);

  if (samples > 1 && perVblankUsSamples != 0) {
    w.Key("summary");
    w.BeginObject();
    w.Key("delta_samples");
    w.Uint32((uint32_t)perVblankUsSamples);
    w.Key("per_vblank_us_min");
    w.String(DecU64(perVblankUsMin));
    w.Key("per_vblank_us_max");
    w.String(DecU64(perVblankUsMax));
    w.Key("per_vblank_us_avg");
    w.String(DecU64(perVblankUsSum / perVblankUsSamples));
    w.Key("stalls");
    w.Uint32(stallCount);
    w.EndObject();
  }

  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoWaitVblankJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                            uint32_t timeoutMs, bool *skipCloseAdapter, std::string *out) {
  if (skipCloseAdapter) {
    *skipCloseAdapter = false;
  }
  if (!out) {
    return 1;
  }
  if (!f->WaitForVerticalBlankEvent) {
    JsonWriteTopLevelError(out, "wait-vblank", f, "D3DKMTWaitForVerticalBlankEvent not available (missing gdi32 export)",
                           STATUS_NOT_SUPPORTED);
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }
  if (timeoutMs == 0) {
    timeoutMs = 1;
  }

  LARGE_INTEGER freq;
  if (!QueryPerformanceFrequency(&freq) || freq.QuadPart <= 0) {
    JsonWriteTopLevelError(out, "wait-vblank", f, "QueryPerformanceFrequency failed", STATUS_INVALID_PARAMETER);
    return 1;
  }

  // Allocate on heap so we can safely leak on timeout (the wait thread may be
  // blocked inside the kernel thunk; tearing it down can deadlock).
  WaitThreadCtx *waiter = (WaitThreadCtx *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(WaitThreadCtx));
  if (!waiter) {
    JsonWriteTopLevelError(out, "wait-vblank", f, "HeapAlloc failed", STATUS_INSUFFICIENT_RESOURCES);
    return 1;
  }

  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  bool fallbackToSource0 = false;
  if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
    JsonWriteTopLevelError(out, "wait-vblank", f, "Failed to start wait thread", STATUS_INSUFFICIENT_RESOURCES);
    HeapFree(GetProcessHeap(), 0, waiter);
    return 1;
  }

  DWORD w = 0;
  NTSTATUS st = 0;
  for (;;) {
    // Prime: perform one wait so subsequent deltas represent full vblank periods.
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      if (skipCloseAdapter) {
        // The wait thread may be blocked inside the kernel thunk. Avoid calling
        // D3DKMTCloseAdapter in this case; just exit the process.
        *skipCloseAdapter = true;
      }
      JsonWriteTopLevelError(out, "wait-vblank", f, "vblank wait timed out (sample 1)", STATUS_TIMEOUT);
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      JsonWriteTopLevelError(out, "wait-vblank", f, "WaitForSingleObject failed", STATUS_INVALID_PARAMETER);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      // Retry with source 0 for older KMDs / single-source implementations.
      StopWaitThread(waiter);
      effectiveVidpnSourceId = 0;
      fallbackToSource0 = true;
      if (!StartWaitThread(waiter, f, hAdapter, effectiveVidpnSourceId)) {
        JsonWriteTopLevelError(out, "wait-vblank", f, "Failed to restart wait thread", STATUS_INSUFFICIENT_RESOURCES);
        HeapFree(GetProcessHeap(), 0, waiter);
        return 1;
      }
      continue;
    }
    if (!NT_SUCCESS(st)) {
      JsonWriteTopLevelError(out, "wait-vblank", f, "D3DKMTWaitForVerticalBlankEvent failed", st);
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }
    break;
  }

  LARGE_INTEGER last;
  QueryPerformanceCounter(&last);

  double min_ms = 1e9;
  double max_ms = 0.0;
  double sum_ms = 0.0;
  uint32_t deltas = 0;

  JsonWriter jw(out);
  jw.BeginObject();
  jw.Key("schema_version");
  jw.Uint32(1);
  jw.Key("command");
  jw.String("wait-vblank");
  jw.Key("vidpn_source_id_requested");
  jw.Uint32(vidpnSourceId);
  jw.Key("vidpn_source_id");
  jw.Uint32(effectiveVidpnSourceId);
  jw.Key("fallback_to_source0");
  jw.Bool(fallbackToSource0);
  jw.Key("samples_requested");
  jw.Uint32(samples);
  jw.Key("timeout_ms");
  jw.Uint32(timeoutMs);
  jw.Key("samples");
  jw.BeginArray();

  for (uint32_t i = 1; i < samples; ++i) {
    SetEvent(waiter->request_event);
    w = WaitForSingleObject(waiter->done_event, timeoutMs);
    if (w == WAIT_TIMEOUT) {
      jw.EndArray();
      jw.Key("ok");
      jw.Bool(false);
      jw.Key("error");
      jw.BeginObject();
      jw.Key("message");
      jw.String("vblank wait timed out");
      jw.Key("sample_index");
      jw.Uint32(i + 1);
      jw.Key("status");
      JsonWriteNtStatusError(jw, f, STATUS_TIMEOUT);
      jw.EndObject();
      jw.EndObject();
      out->push_back('\n');
      if (skipCloseAdapter) {
        *skipCloseAdapter = true;
      }
      return 2;
    }
    if (w != WAIT_OBJECT_0) {
      jw.EndArray();
      jw.Key("ok");
      jw.Bool(false);
      jw.Key("error");
      jw.BeginObject();
      jw.Key("message");
      jw.String("WaitForSingleObject failed");
      jw.Key("status");
      JsonWriteNtStatusError(jw, f, STATUS_INVALID_PARAMETER);
      jw.EndObject();
      jw.EndObject();
      out->push_back('\n');
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    st = (NTSTATUS)InterlockedCompareExchange(&waiter->last_status, 0, 0);
    if (!NT_SUCCESS(st)) {
      jw.EndArray();
      jw.Key("ok");
      jw.Bool(false);
      jw.Key("error");
      jw.BeginObject();
      jw.Key("message");
      jw.String("D3DKMTWaitForVerticalBlankEvent failed");
      jw.Key("status");
      JsonWriteNtStatusError(jw, f, st);
      jw.EndObject();
      jw.EndObject();
      out->push_back('\n');
      StopWaitThread(waiter);
      HeapFree(GetProcessHeap(), 0, waiter);
      return 2;
    }

    LARGE_INTEGER now;
    QueryPerformanceCounter(&now);
    const double dt_ms = (double)(now.QuadPart - last.QuadPart) * 1000.0 / (double)freq.QuadPart;
    last = now;

    if (dt_ms < min_ms) {
      min_ms = dt_ms;
    }
    if (dt_ms > max_ms) {
      max_ms = dt_ms;
    }
    sum_ms += dt_ms;
    deltas += 1;
    jw.BeginObject();
    jw.Key("index");
    jw.Uint32(i + 1);
    jw.Key("dt_ms");
    jw.Double(dt_ms);
    jw.EndObject();
  }

  StopWaitThread(waiter);
  HeapFree(GetProcessHeap(), 0, waiter);

  jw.EndArray();
  jw.Key("ok");
  jw.Bool(true);
  if (deltas != 0) {
    const double avg_ms = sum_ms / (double)deltas;
    const double hz = (avg_ms > 0.0) ? (1000.0 / avg_ms) : 0.0;
    jw.Key("summary");
    jw.BeginObject();
    jw.Key("waits");
    jw.Uint32(samples);
    jw.Key("deltas");
    jw.Uint32(deltas);
    jw.Key("avg_ms");
    jw.Double(avg_ms);
    jw.Key("min_ms");
    jw.Double(min_ms);
    jw.Key("max_ms");
    jw.Double(max_ms);
    jw.Key("hz");
    jw.Double(hz);
    jw.EndObject();
  }
  jw.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoQueryScanlineJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t vidpnSourceId, uint32_t samples,
                               uint32_t intervalMs, std::string *out) {
  if (!out) {
    return 1;
  }
  if (!f->GetScanLine) {
    JsonWriteTopLevelError(out, "query-scanline", f, "D3DKMTGetScanLine not available (missing gdi32 export)",
                           STATUS_NOT_SUPPORTED);
    return 1;
  }

  if (samples == 0) {
    samples = 1;
  }
  if (samples > 10000) {
    samples = 10000;
  }

  uint32_t inVblank = 0;
  uint32_t outVblank = 0;
  uint32_t minLine = 0xFFFFFFFFu;
  uint32_t maxLine = 0;
  uint32_t effectiveVidpnSourceId = vidpnSourceId;
  bool fallbackToSource0 = false;

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("query-scanline");
  w.Key("vidpn_source_id_requested");
  w.Uint32(vidpnSourceId);
  w.Key("samples_requested");
  w.Uint32(samples);
  w.Key("interval_ms");
  w.Uint32(intervalMs);

  w.Key("samples");
  w.BeginArray();

  for (uint32_t i = 0; i < samples; ++i) {
    D3DKMT_GETSCANLINE s;
    ZeroMemory(&s, sizeof(s));
    s.hAdapter = hAdapter;
    s.VidPnSourceId = effectiveVidpnSourceId;

    NTSTATUS st = f->GetScanLine(&s);
    if (!NT_SUCCESS(st) && st == STATUS_INVALID_PARAMETER && effectiveVidpnSourceId != 0) {
      fallbackToSource0 = true;
      effectiveVidpnSourceId = 0;
      s.VidPnSourceId = 0;
      st = f->GetScanLine(&s);
    }
    if (!NT_SUCCESS(st)) {
      w.EndArray();
      w.Key("ok");
      w.Bool(false);
      w.Key("error");
      w.BeginObject();
      w.Key("message");
      w.String("D3DKMTGetScanLine failed");
      w.Key("status");
      JsonWriteNtStatusError(w, f, st);
      w.EndObject();
      w.EndObject();
      out->push_back('\n');
      return 2;
    }

    w.BeginObject();
    w.Key("index");
    w.Uint32(i + 1);
    w.Key("vidpn_source_id");
    w.Uint32(s.VidPnSourceId);
    w.Key("scanline");
    w.Uint32((uint32_t)s.ScanLine);
    w.Key("in_vblank");
    w.Bool(!!s.InVerticalBlank);
    w.EndObject();

    if (s.InVerticalBlank) {
      inVblank += 1;
    } else {
      outVblank += 1;
      if ((uint32_t)s.ScanLine < minLine) {
        minLine = (uint32_t)s.ScanLine;
      }
      if ((uint32_t)s.ScanLine > maxLine) {
        maxLine = (uint32_t)s.ScanLine;
      }
    }

    if (i + 1 < samples && intervalMs != 0) {
      Sleep(intervalMs);
    }
  }

  w.EndArray();
  w.Key("ok");
  w.Bool(true);
  w.Key("vidpn_source_id");
  w.Uint32(effectiveVidpnSourceId);
  w.Key("fallback_to_source0");
  w.Bool(fallbackToSource0);
  w.Key("summary");
  w.BeginObject();
  w.Key("in_vblank");
  w.Uint32(inVblank);
  w.Key("out_vblank");
  w.Uint32(outVblank);
  if (outVblank != 0) {
    w.Key("out_scanline_range");
    w.BeginObject();
    w.Key("min");
    w.Uint32(minLine);
    w.Key("max");
    w.Uint32(maxLine);
    w.EndObject();
  }
  w.EndObject();

  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoMapSharedHandleJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint64_t sharedHandle,
                                std::string *out) {
  if (!out) {
    return 1;
  }

  aerogpu_escape_map_shared_handle_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.shared_handle = sharedHandle;
  q.debug_token = 0;
  q.reserved0 = 0;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "map-shared-handle", f, "D3DKMTEscape(map-shared-handle) failed", st);
    return 2;
  }

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("map-shared-handle");
  w.Key("ok");
  w.Bool(true);
  w.Key("shared_handle_hex");
  w.String(HexU64(sharedHandle));
  w.Key("debug_token");
  w.BeginObject();
  w.Key("hex");
  w.String(HexU32(q.debug_token));
  w.Key("dec");
  w.Uint32(q.debug_token);
  w.EndObject();
  w.EndObject();
  out->push_back('\n');
  return 0;
}

static int DoSelftestJson(const D3DKMT_FUNCS *f, D3DKMT_HANDLE hAdapter, uint32_t timeoutMs, std::string *out) {
  if (!out) {
    return 1;
  }

  // Best-effort: feature bits + scanout enable (helps interpret skipped vblank/IRQ checks).
  uint64_t features = 0;
  bool haveFeatures = false;
  {
    aerogpu_escape_query_device_v2_out dev;
    ZeroMemory(&dev, sizeof(dev));
    dev.hdr.version = AEROGPU_ESCAPE_VERSION;
    dev.hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
    dev.hdr.size = sizeof(dev);
    dev.hdr.reserved0 = 0;
    const NTSTATUS stDev = SendAerogpuEscape(f, hAdapter, &dev, sizeof(dev));
    if (NT_SUCCESS(stDev)) {
      features = dev.features_lo;
      haveFeatures = true;
    }
  }

  bool scanoutKnown = false;
  bool scanoutEnabled = false;
  {
    aerogpu_escape_query_scanout_out qs;
    ZeroMemory(&qs, sizeof(qs));
    qs.hdr.version = AEROGPU_ESCAPE_VERSION;
    qs.hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
    qs.hdr.size = sizeof(qs);
    qs.hdr.reserved0 = 0;
    qs.vidpn_source_id = 0;
    const NTSTATUS stScanout = SendAerogpuEscape(f, hAdapter, &qs, sizeof(qs));
    if (NT_SUCCESS(stScanout)) {
      scanoutKnown = true;
      scanoutEnabled = (qs.mmio_enable != 0);
    }
  }

  const bool featureVblank = haveFeatures && ((features & AEROGPU_FEATURE_VBLANK) != 0);
  const bool featureCursor = haveFeatures && ((features & AEROGPU_FEATURE_CURSOR) != 0);

  aerogpu_escape_selftest_inout q;
  ZeroMemory(&q, sizeof(q));
  q.hdr.version = AEROGPU_ESCAPE_VERSION;
  q.hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
  q.hdr.size = sizeof(q);
  q.hdr.reserved0 = 0;
  q.timeout_ms = timeoutMs;

  const NTSTATUS st = SendAerogpuEscape(f, hAdapter, &q, sizeof(q));
  if (!NT_SUCCESS(st)) {
    JsonWriteTopLevelError(out, "selftest", f, "D3DKMTEscape(selftest) failed", st);
    // Preserve stable selftest exit codes: use an out-of-band nonzero value for
    // transport failures so it won't be confused with a KMD-reported selftest
    // error_code.
    return 254;
  }

  // Exit code semantics:
  // - PASS: 0
  // - FAIL: KMD-provided stable error_code (fallback to 1 if a buggy/older KMD
  //   reports failure with error_code==0)
  const int rc = q.passed ? 0 : ((q.error_code != 0) ? (int)q.error_code : 1);

  enum SelftestStage {
    STAGE_RING = 0,
    STAGE_VBLANK = 1,
    STAGE_IRQ = 2,
    STAGE_CURSOR = 3,
    STAGE_DONE = 4,
  };

  const bool timeBudgetExhausted =
      (!q.passed && q.error_code == AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED);

  SelftestStage failedStage = STAGE_DONE;
  if (!q.passed) {
    switch (q.error_code) {
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK:
      failedStage = STAGE_VBLANK;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED:
    case AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED:
      failedStage = STAGE_IRQ;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE:
    case AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH:
      failedStage = STAGE_CURSOR;
      break;
    case AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED:
      failedStage = STAGE_VBLANK;
      break;
    default:
      failedStage = STAGE_RING;
      break;
    }
  }

  const auto SubcheckStatus = [&](SelftestStage stage,
                                  bool featureKnown,
                                  bool featureEnabled,
                                  bool requireScanout) -> const char * {
    if (timeBudgetExhausted) {
      // Ring head advancement completed, but the KMD ran out of time budget during optional checks.
      // Mark optional checks as skipped/incomplete rather than attributing failure to a specific stage.
      return (stage == STAGE_RING) ? "pass" : "skip";
    }
    if (!featureKnown) {
      return "unknown";
    }
    if (!featureEnabled) {
      return "skip";
    }
    if (requireScanout && scanoutKnown && !scanoutEnabled) {
      return "skip";
    }
    if (q.passed || failedStage > stage) {
      return "pass";
    }
    if (failedStage == stage) {
      return "fail";
    }
    return "skip";
  };

  JsonWriter w(out);
  w.BeginObject();
  w.Key("schema_version");
  w.Uint32(1);
  w.Key("command");
  w.String("selftest");
  w.Key("ok");
  w.Bool(q.passed ? true : false);
  w.Key("passed");
  w.Bool(q.passed ? true : false);
  w.Key("timeout_ms");
  w.Uint32(timeoutMs);

  w.Key("features_known");
  w.Bool(haveFeatures ? true : false);
  if (haveFeatures) {
    w.Key("features_lo_hex");
    w.String(HexU64(features));
  }

  w.Key("scanout_known");
  w.Bool(scanoutKnown ? true : false);
  if (scanoutKnown) {
    w.Key("scanout_enabled");
    w.Bool(scanoutEnabled ? true : false);
  }

  w.Key("subchecks");
  w.BeginObject();
  w.Key("ring");
  w.String(SubcheckStatus(STAGE_RING, true, true, false));
  w.Key("vblank");
  w.String(SubcheckStatus(STAGE_VBLANK, haveFeatures, featureVblank, true));
  w.Key("irq");
  w.String(SubcheckStatus(STAGE_IRQ, haveFeatures, featureVblank, true));
  w.Key("cursor");
  w.String(SubcheckStatus(STAGE_CURSOR, haveFeatures, featureCursor, false));
  w.EndObject();

  if (!q.passed) {
    w.Key("error_code");
    w.Uint32(q.error_code);
    w.Key("error_code_str");
    w.String(WideToUtf8(SelftestErrorToString(q.error_code)));
  }
  w.EndObject();
  out->push_back('\n');
  return rc;
}

int wmain(int argc, wchar_t **argv) {
  const wchar_t *displayNameOpt = NULL;
  uint32_t ringId = 0;
  uint32_t timeoutMs = 2000;
  bool timeoutMsSet = false;
  uint32_t vblankSamples = 1;
  uint32_t vblankIntervalMs = 250;
  uint32_t watchSamples = 0;
  uint32_t watchIntervalMs = 0;
  bool watchSamplesSet = false;
  bool watchIntervalSet = false;
  uint64_t mapSharedHandle = 0;
  const wchar_t *createAllocCsvPath = NULL;
  const wchar_t *dumpScanoutBmpPath = NULL;
  const wchar_t *dumpScanoutPngPath = NULL;
  const wchar_t *dumpCursorBmpPath = NULL;
  const wchar_t *dumpCursorPngPath = NULL;
  uint64_t readGpa = 0;
  uint32_t readGpaSizeBytes = 0;
  const wchar_t *readGpaOutPath = NULL;
  bool readGpaForce = false;
  const wchar_t *dumpLastCmdOutPath = NULL;
  const wchar_t *dumpLastCmdAllocOutPath = NULL;
  uint32_t dumpLastCmdIndexFromTail = 0;
  uint32_t dumpLastCmdCount = 1;
  bool dumpLastCmdForce = false;
  bool dumpLastCmdOutExplicit = false;
  enum {
    CMD_NONE = 0,
    CMD_LIST_DISPLAYS,
    CMD_QUERY_VERSION,
    CMD_QUERY_UMD_PRIVATE,
    CMD_QUERY_SEGMENTS,
    CMD_QUERY_FENCE,
    CMD_WATCH_FENCE,
    CMD_QUERY_PERF,
    CMD_QUERY_SCANOUT,
    CMD_DUMP_SCANOUT_BMP,
    CMD_DUMP_SCANOUT_PNG,
    CMD_QUERY_CURSOR,
    CMD_DUMP_CURSOR_BMP,
    CMD_DUMP_CURSOR_PNG,
    CMD_DUMP_RING,
    CMD_WATCH_RING,
    CMD_DUMP_LAST_CMD,
    CMD_DUMP_CREATEALLOCATION,
    CMD_DUMP_VBLANK,
    CMD_WAIT_VBLANK,
    CMD_QUERY_SCANLINE,
    CMD_MAP_SHARED_HANDLE,
    CMD_READ_GPA,
    CMD_SELFTEST
  } cmd = CMD_NONE;

  // Pre-scan argv for global JSON flags so we can still emit machine-readable
  // JSON even if argument parsing fails before we reach `--json`/`--pretty` in
  // the main parse loop.
  for (int i = 1; i < argc; ++i) {
    const wchar_t *a = argv[i];
    if (!a) {
      continue;
    }
    if (wcscmp(a, L"--pretty") == 0) {
      g_json_output = true;
      g_json_pretty = true;
      continue;
    }
    if (wcscmp(a, L"--json") == 0) {
      g_json_output = true;
      // Allow "--json <path>" as a convenience/compat form in addition to "--json=<path>".
      if (i + 1 < argc) {
        const wchar_t *next = argv[i + 1];
        // Disambiguate between JSON output path and the next option:
        // - paths typically start with a drive letter or '\\'
        // - options use '-' or '/' prefixes
        if (next && next[0] != L'-' && next[0] != L'/') {
          g_json_path = next;
          i += 1;
        }
      }
      continue;
    }
    if (wcsncmp(a, L"--json=", 7) == 0) {
      g_json_output = true;
      const wchar_t *path = a + 7;
      if (path && *path) {
        g_json_path = path;
      }
      continue;
    }

    // Skip payload arguments for options that take a single argument. This avoids falsely treating
    // literal argument values like "--json" / "--pretty" as global output flags during the pre-scan.
    if (wcscmp(a, L"--display") == 0 || wcscmp(a, L"--ring-id") == 0 || wcscmp(a, L"--timeout-ms") == 0 ||
        wcscmp(a, L"--size") == 0 || wcscmp(a, L"--out") == 0 || wcscmp(a, L"--cmd-out") == 0 ||
        wcscmp(a, L"--alloc-out") == 0 || wcscmp(a, L"--map-shared-handle") == 0 || wcscmp(a, L"--read-gpa") == 0 ||
        wcscmp(a, L"--vblank-samples") == 0 || wcscmp(a, L"--vblank-interval-ms") == 0 || wcscmp(a, L"--samples") == 0 ||
        wcscmp(a, L"--interval-ms") == 0 || wcscmp(a, L"--csv") == 0 || wcscmp(a, L"--index-from-tail") == 0 ||
        wcscmp(a, L"--count") == 0 || wcscmp(a, L"--dump-scanout-bmp") == 0 || wcscmp(a, L"--dump-scanout-png") == 0 ||
        wcscmp(a, L"--dump-cursor-bmp") == 0 || wcscmp(a, L"--dump-cursor-png") == 0) {
      if (i + 1 < argc) {
        i += 1;
      }
      continue;
    }
  }

  const auto SetCommand = [&](int newCmd) -> bool {
    if (cmd != CMD_NONE) {
      fwprintf(stderr, L"Multiple commands specified.\n");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, "parse-args", NULL, "Multiple commands specified", STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return false;
    }
    cmd = (decltype(cmd))newCmd;
    return true;
  };

  for (int i = 1; i < argc; ++i) {
    const wchar_t *a = argv[i];
    if (wcscmp(a, L"--help") == 0 || wcscmp(a, L"-h") == 0 || wcscmp(a, L"/?") == 0) {
      PrintUsage();
      return 0;
    }

    if (wcscmp(a, L"--pretty") == 0) {
      g_json_output = true;
      g_json_pretty = true;
      continue;
    }

    if (wcscmp(a, L"--json") == 0) {
      g_json_output = true;
      // Allow "--json <path>" as a convenience/compat form in addition to "--json=<path>".
      if (i + 1 < argc) {
        const wchar_t *next = argv[i + 1];
        // Disambiguate between JSON output path and the next option:
        // - paths typically start with a drive letter or '\\'
        // - options use '-' or '/' prefixes
        if (next && next[0] != L'-' && next[0] != L'/') {
          g_json_path = next;
          i += 1;
        }
      }
      continue;
    }
    if (wcsncmp(a, L"--json=", 7) == 0) {
      const wchar_t *path = a + 7;
      if (!path || *path == 0) {
        fwprintf(stderr, L"--json=PATH requires a non-empty PATH\n");
        PrintUsage();
        std::string json;
        JsonWriteTopLevelError(&json, "parse-args", NULL, "--json=PATH requires a non-empty PATH",
                               STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
        return 1;
      }
      g_json_output = true;
      g_json_path = path;
      continue;
    }

    if (wcscmp(a, L"--display") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--display requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--display requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      displayNameOpt = argv[++i];
      continue;
    }

    if (wcscmp(a, L"--ring-id") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--ring-id requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--ring-id requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --ring-id value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --ring-id value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      ringId = (uint32_t)v;
      continue;
    }

    if (wcscmp(a, L"--timeout-ms") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--timeout-ms requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--timeout-ms requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --timeout-ms value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --timeout-ms value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      timeoutMs = (uint32_t)v;
      timeoutMsSet = true;
      continue;
    }

    if (wcscmp(a, L"--size") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--size requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--size requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (readGpaSizeBytes != 0) {
        fwprintf(stderr, L"--size specified multiple times\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--size specified multiple times", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --size value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --size value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      readGpaSizeBytes = (uint32_t)v;
      continue;
    }

    if (wcscmp(a, L"--out") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--out requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--out requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (readGpaOutPath || dumpLastCmdOutExplicit) {
        fwprintf(stderr, L"--out specified multiple times\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--out specified multiple times", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *out = argv[++i];
      readGpaOutPath = out;
      dumpLastCmdOutPath = out;
      continue;
    }

    if (wcscmp(a, L"--cmd-out") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--cmd-out requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--cmd-out requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (dumpLastCmdOutExplicit || dumpLastCmdOutPath) {
        fwprintf(stderr, L"--cmd-out specified multiple times (or conflicts with --out)\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL,
                                 "--cmd-out specified multiple times (or conflicts with --out)",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      dumpLastCmdOutPath = argv[++i];
      dumpLastCmdOutExplicit = true;
      continue;
    }

    if (wcscmp(a, L"--alloc-out") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--alloc-out requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--alloc-out requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      dumpLastCmdAllocOutPath = argv[++i];
      continue;
    }

    if (wcscmp(a, L"--force") == 0) {
      readGpaForce = true;
      dumpLastCmdForce = true;
      continue;
    }

    if (wcscmp(a, L"--map-shared-handle") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--map-shared-handle requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "map-shared-handle", NULL, "--map-shared-handle requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_MAP_SHARED_HANDLE)) {
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      mapSharedHandle = (uint64_t)_wcstoui64(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --map-shared-handle value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --map-shared-handle value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "map-shared-handle", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      continue;
    }

    if (wcscmp(a, L"--read-gpa") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--read-gpa requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "read-gpa", NULL, "--read-gpa requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_READ_GPA)) {
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      readGpa = (uint64_t)_wcstoui64(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --read-gpa value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --read-gpa value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "read-gpa", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }

      // Also support positional size: `--read-gpa <gpa> <size_bytes>`.
      if (i + 1 < argc) {
        const wchar_t *maybeSize = argv[i + 1];
        if (maybeSize[0] != L'-' && maybeSize[0] != L'/') {
          if (readGpaSizeBytes != 0) {
            fwprintf(stderr, L"--read-gpa size specified multiple times\n");
            PrintUsage();
            if (g_json_output) {
              std::string json;
              JsonWriteTopLevelError(&json, "read-gpa", NULL, "--read-gpa size specified multiple times",
                                     STATUS_INVALID_PARAMETER);
              WriteJsonToDestination(json);
            }
            return 1;
          }

          wchar_t *endSize = NULL;
          const unsigned long sizeUl = wcstoul(maybeSize, &endSize, 0);
          if (!endSize || endSize == maybeSize || *endSize != 0) {
            fwprintf(stderr, L"Invalid size value: %s\n", maybeSize);
            if (g_json_output) {
              std::string json;
              const std::string msg = std::string("Invalid size value: ") + WideToUtf8(maybeSize);
              JsonWriteTopLevelError(&json, "read-gpa", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
              WriteJsonToDestination(json);
            }
            return 1;
          }
          readGpaSizeBytes = (uint32_t)sizeUl;
          ++i;
        }
      }
      continue;
    }

    if (wcscmp(a, L"--vblank-samples") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--vblank-samples requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--vblank-samples requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --vblank-samples value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --vblank-samples value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      vblankSamples = (uint32_t)v;
      continue;
    }

    if (wcscmp(a, L"--vblank-interval-ms") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--vblank-interval-ms requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--vblank-interval-ms requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --vblank-interval-ms value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --vblank-interval-ms value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      vblankIntervalMs = (uint32_t)v;
      continue;
    }

    if (wcscmp(a, L"--samples") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--samples requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--samples requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --samples value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --samples value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      watchSamples = (uint32_t)v;
      watchSamplesSet = true;
      continue;
    }

    if (wcscmp(a, L"--interval-ms") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--interval-ms requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--interval-ms requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --interval-ms value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --interval-ms value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      watchIntervalMs = (uint32_t)v;
      watchIntervalSet = true;
      continue;
    }

    if (wcscmp(a, L"--csv") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--csv requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--csv requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (createAllocCsvPath) {
        fwprintf(stderr, L"--csv specified multiple times\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--csv specified multiple times", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      createAllocCsvPath = argv[++i];
      continue;
    }

    if (wcscmp(a, L"--index-from-tail") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--index-from-tail requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--index-from-tail requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      dumpLastCmdIndexFromTail = (uint32_t)wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0) {
        fwprintf(stderr, L"Invalid --index-from-tail value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --index-from-tail value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      continue;
    }

    if (wcscmp(a, L"--count") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--count requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "parse-args", NULL, "--count requires an argument", STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      const wchar_t *arg = argv[++i];
      wchar_t *end = NULL;
      const unsigned long v = wcstoul(arg, &end, 0);
      if (!end || end == arg || *end != 0 || v == 0) {
        fwprintf(stderr, L"Invalid --count value: %s\n", arg);
        if (g_json_output) {
          std::string json;
          const std::string msg = std::string("Invalid --count value: ") + WideToUtf8(arg);
          JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      dumpLastCmdCount = (uint32_t)v;
      continue;
    }

    if (wcscmp(a, L"--query-version") == 0 || wcscmp(a, L"--query-device") == 0) {
      if (!SetCommand(CMD_QUERY_VERSION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--status") == 0) {
      if (!SetCommand(CMD_QUERY_VERSION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-umd-private") == 0) {
      if (!SetCommand(CMD_QUERY_UMD_PRIVATE)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-segments") == 0) {
      if (!SetCommand(CMD_QUERY_SEGMENTS)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-fence") == 0) {
      if (!SetCommand(CMD_QUERY_FENCE)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--watch-fence") == 0) {
      if (!SetCommand(CMD_WATCH_FENCE)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-perf") == 0 || wcscmp(a, L"--perf") == 0) {
      if (!SetCommand(CMD_QUERY_PERF)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-scanout") == 0) {
      if (!SetCommand(CMD_QUERY_SCANOUT)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-scanout-bmp") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--dump-scanout-bmp requires an output path\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "dump-scanout-bmp", NULL, "--dump-scanout-bmp requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_DUMP_SCANOUT_BMP)) {
        return 1;
      }
      dumpScanoutBmpPath = argv[++i];
      continue;
    }
    if (wcscmp(a, L"--dump-scanout-png") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--dump-scanout-png requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "dump-scanout-png", NULL, "--dump-scanout-png requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_DUMP_SCANOUT_PNG)) {
        return 1;
      }
      dumpScanoutPngPath = argv[++i];
      continue;
    }
    if (wcscmp(a, L"--query-cursor") == 0 || wcscmp(a, L"--dump-cursor") == 0) {
      if (!SetCommand(CMD_QUERY_CURSOR)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-cursor-bmp") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--dump-cursor-bmp requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "dump-cursor-bmp", NULL, "--dump-cursor-bmp requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_DUMP_CURSOR_BMP)) {
        return 1;
      }
      dumpCursorBmpPath = argv[++i];
      continue;
    }
    if (wcscmp(a, L"--dump-cursor-png") == 0) {
      if (i + 1 >= argc) {
        fwprintf(stderr, L"--dump-cursor-png requires an argument\n");
        PrintUsage();
        if (g_json_output) {
          std::string json;
          JsonWriteTopLevelError(&json, "dump-cursor-png", NULL, "--dump-cursor-png requires an argument",
                                 STATUS_INVALID_PARAMETER);
          WriteJsonToDestination(json);
        }
        return 1;
      }
      if (!SetCommand(CMD_DUMP_CURSOR_PNG)) {
        return 1;
      }
      dumpCursorPngPath = argv[++i];
      continue;
    }
    if (wcscmp(a, L"--dump-ring") == 0) {
      if (!SetCommand(CMD_DUMP_RING)) {
        return 1;
      }
      continue;
    }

    if (wcscmp(a, L"--watch-ring") == 0) {
      if (!SetCommand(CMD_WATCH_RING)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-last-cmd") == 0 || wcscmp(a, L"--dump-last-submit") == 0) {
      if (!SetCommand(CMD_DUMP_LAST_CMD)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-createalloc") == 0 || wcscmp(a, L"--dump-createallocation") == 0 ||
        wcscmp(a, L"--dump-allocations") == 0) {
      if (!SetCommand(CMD_DUMP_CREATEALLOCATION)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--dump-vblank") == 0) {
      if (!SetCommand(CMD_DUMP_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-vblank") == 0) {
      if (!SetCommand(CMD_DUMP_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--wait-vblank") == 0) {
      if (!SetCommand(CMD_WAIT_VBLANK)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--query-scanline") == 0) {
      if (!SetCommand(CMD_QUERY_SCANLINE)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--selftest") == 0) {
      if (!SetCommand(CMD_SELFTEST)) {
        return 1;
      }
      continue;
    }
    if (wcscmp(a, L"--list-displays") == 0) {
      if (!SetCommand(CMD_LIST_DISPLAYS)) {
        return 1;
      }
      continue;
    }

    fwprintf(stderr, L"Unknown argument: %s\n", a);
    PrintUsage();
    if (g_json_output) {
      std::string json;
      const std::string msg = std::string("Unknown argument: ") + WideToUtf8(a);
      JsonWriteTopLevelError(&json, "parse-args", NULL, msg.c_str(), STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  if (cmd == CMD_NONE) {
    PrintUsage();
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "parse-args", NULL, "No command specified", STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  if (createAllocCsvPath && cmd != CMD_DUMP_CREATEALLOCATION) {
    fwprintf(stderr, L"--csv is only supported with --dump-createalloc\n");
    PrintUsage();
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "parse-args", NULL, "--csv is only supported with --dump-createalloc",
                             STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  if (readGpaOutPath && cmd != CMD_READ_GPA && cmd != CMD_DUMP_LAST_CMD) {
    fwprintf(stderr, L"--out is only supported with --read-gpa and --dump-last-submit/--dump-last-cmd\n");
    PrintUsage();
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "parse-args", NULL,
                             "--out is only supported with --read-gpa and --dump-last-submit/--dump-last-cmd",
                             STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  // `--cmd-out` and `--alloc-out` are used by `--dump-last-submit` (alias: `--dump-last-cmd`).
  // Note: `--out` is also accepted by `--dump-last-cmd` for backward compatibility.
  if (dumpLastCmdOutExplicit && cmd != CMD_DUMP_LAST_CMD) {
    fwprintf(stderr, L"--cmd-out is only supported with --dump-last-submit/--dump-last-cmd\n");
    PrintUsage();
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "parse-args", NULL,
                             "--cmd-out is only supported with --dump-last-submit/--dump-last-cmd",
                             STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }
  if (dumpLastCmdAllocOutPath && cmd != CMD_DUMP_LAST_CMD) {
    fwprintf(stderr, L"--alloc-out is only supported with --dump-last-submit/--dump-last-cmd\n");
    PrintUsage();
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "parse-args", NULL,
                             "--alloc-out is only supported with --dump-last-submit/--dump-last-cmd",
                             STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  if (cmd == CMD_LIST_DISPLAYS) {
    if (!g_json_output) {
      return ListDisplays();
    }
    std::string json;
    const int rc = ListDisplaysJson(&json);
    const int writeRc = WriteJsonToDestination(json);
    return (rc != 0) ? rc : writeRc;
  }

  if (cmd == CMD_WATCH_FENCE || cmd == CMD_WATCH_RING) {
    const char *jsonCmd = (cmd == CMD_WATCH_RING) ? "watch-ring" : "watch-fence";
    if (!watchSamplesSet) {
      fwprintf(stderr, L"%s requires --samples N\n", (cmd == CMD_WATCH_RING) ? L"--watch-ring" : L"--watch-fence");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, jsonCmd, NULL, "--samples is required", STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
    if (!watchIntervalSet) {
      fwprintf(stderr, L"%s requires --interval-ms M\n", (cmd == CMD_WATCH_RING) ? L"--watch-ring" : L"--watch-fence");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, jsonCmd, NULL, "--interval-ms is required", STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
  }
  if (cmd == CMD_WATCH_RING) {
    if (!watchSamplesSet) {
      fwprintf(stderr, L"--watch-ring requires --samples N\n");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, "watch-ring", NULL, "--watch-ring requires --samples N", STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
    if (!watchIntervalSet) {
      fwprintf(stderr, L"--watch-ring requires --interval-ms M\n");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, "watch-ring", NULL, "--watch-ring requires --interval-ms M",
                               STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
  }
  if (cmd == CMD_DUMP_LAST_CMD) {
    if (!dumpLastCmdOutPath || !dumpLastCmdOutPath[0]) {
      fwprintf(stderr, L"--dump-last-submit/--dump-last-cmd requires --cmd-out <path> (or --out <path>)\n");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, "dump-last-cmd", NULL,
                               "--dump-last-submit/--dump-last-cmd requires --cmd-out <path> (or --out <path>)",
                               STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
  }

  if (cmd == CMD_READ_GPA) {
    if (readGpaSizeBytes == 0) {
      fwprintf(stderr, L"--read-gpa requires a size (--size N or positional)\n");
      PrintUsage();
      if (g_json_output) {
        std::string json;
        JsonWriteTopLevelError(&json, "read-gpa", NULL, "--read-gpa requires --size N", STATUS_INVALID_PARAMETER);
        WriteJsonToDestination(json);
      }
      return 1;
    }
  }

  D3DKMT_FUNCS f;
  if (!LoadD3DKMT(&f)) {
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "init", NULL, "Failed to load D3DKMT entrypoints", STATUS_NOT_SUPPORTED);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  // Use the user-provided timeout for escapes as well (prevents hangs on buggy KMD escape paths).
  g_escape_timeout_ms = timeoutMs;

  wchar_t displayName[CCHDEVICENAME];
  if (displayNameOpt) {
    wcsncpy(displayName, displayNameOpt, CCHDEVICENAME - 1);
    displayName[CCHDEVICENAME - 1] = 0;
  } else {
    GetPrimaryDisplayName(displayName);
  }

  HDC hdc = CreateDCW(L"DISPLAY", displayName, NULL, NULL);
  if (!hdc) {
    fwprintf(stderr, L"CreateDCW failed for %s (GetLastError=%lu)\n", displayName, (unsigned long)GetLastError());
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "open-adapter", &f, "CreateDCW failed", STATUS_INVALID_PARAMETER);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  D3DKMT_OPENADAPTERFROMHDC open;
  ZeroMemory(&open, sizeof(open));
  open.hDc = hdc;
  NTSTATUS st = f.OpenAdapterFromHdc(&open);
  DeleteDC(hdc);
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTOpenAdapterFromHdc failed", &f, st);
    if (g_json_output) {
      std::string json;
      JsonWriteTopLevelError(&json, "open-adapter", &f, "D3DKMTOpenAdapterFromHdc failed", st);
      WriteJsonToDestination(json);
    }
    return 1;
  }

  int rc = 0;
  bool skipCloseAdapter = false;
  std::string json;
  if (g_json_output) {
    switch (cmd) {
    case CMD_QUERY_VERSION:
      rc = DoStatusJson(&f, open.hAdapter, &json);
      break;
    case CMD_QUERY_UMD_PRIVATE:
      rc = DoQueryUmdPrivateJson(&f, open.hAdapter, &json);
      break;
    case CMD_QUERY_FENCE:
      rc = DoQueryFenceJson(&f, open.hAdapter, &json);
      break;
    case CMD_QUERY_SEGMENTS:
      rc = DoQuerySegmentsJson(&f, open.hAdapter, &json);
      break;
    case CMD_QUERY_PERF:
      rc = DoQueryPerfJson(&f, open.hAdapter, &json);
      break;
    case CMD_QUERY_SCANOUT:
      rc = DoQueryScanoutJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, &json);
      break;
    case CMD_QUERY_CURSOR:
      rc = DoQueryCursorJson(&f, open.hAdapter, &json);
      break;
    case CMD_DUMP_CURSOR_BMP:
      rc = DoDumpCursorBmpJson(&f, open.hAdapter, dumpCursorBmpPath, &json);
      break;
    case CMD_DUMP_CURSOR_PNG:
      rc = DoDumpCursorPngJson(&f, open.hAdapter, dumpCursorPngPath, &json);
      break;
    case CMD_DUMP_RING:
      rc = DoDumpRingJson(&f, open.hAdapter, ringId, &json);
      break;
    case CMD_DUMP_CREATEALLOCATION:
      rc = DoDumpCreateAllocationJson(&f, open.hAdapter, createAllocCsvPath, &json);
      break;
    case CMD_DUMP_VBLANK:
      rc = DoDumpVblankJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs, &json);
      break;
    case CMD_MAP_SHARED_HANDLE:
      rc = DoMapSharedHandleJson(&f, open.hAdapter, mapSharedHandle, &json);
      break;
    case CMD_SELFTEST:
      rc = DoSelftestJson(&f, open.hAdapter, timeoutMs, &json);
      break;
    case CMD_DUMP_SCANOUT_BMP:
      rc = DoDumpScanoutBmpJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, dumpScanoutBmpPath, &json);
      break;
    case CMD_DUMP_SCANOUT_PNG:
      rc = DoDumpScanoutPngJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, dumpScanoutPngPath, &json);
      break;
    case CMD_DUMP_LAST_CMD:
      rc = DoDumpLastCmdJson(&f, open.hAdapter, ringId, dumpLastCmdIndexFromTail, dumpLastCmdCount, dumpLastCmdOutPath,
                             dumpLastCmdAllocOutPath, dumpLastCmdForce, &json);
      break;
    case CMD_READ_GPA:
      rc = DoReadGpaJson(&f, open.hAdapter, readGpa, readGpaSizeBytes, readGpaOutPath, &json);
      break;
    case CMD_WATCH_FENCE:
      rc = DoWatchFenceJson(&f, open.hAdapter, watchSamples, watchIntervalMs, timeoutMsSet ? timeoutMs : 0, &json);
      break;
    case CMD_WATCH_RING:
      rc = DoWatchRingJson(&f, open.hAdapter, ringId, watchSamples, watchIntervalMs, &json);
      break;
    case CMD_WAIT_VBLANK:
      rc = DoWaitVblankJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, timeoutMs, &skipCloseAdapter, &json);
      break;
    case CMD_QUERY_SCANLINE:
      rc = DoQueryScanlineJson(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs, &json);
      break;
    default:
      JsonWriteTopLevelError(&json, "unknown", &f, "Unknown command", STATUS_INVALID_PARAMETER);
      rc = 1;
      break;
    }

    const int writeRc = WriteJsonToDestination(json);
    if (rc == 0 && writeRc != 0) {
      rc = writeRc;
    }
  } else {
    switch (cmd) {
    case CMD_QUERY_VERSION:
      rc = DoQueryVersion(&f, open.hAdapter);
      break;
    case CMD_QUERY_UMD_PRIVATE:
      rc = DoQueryUmdPrivate(&f, open.hAdapter);
      break;
    case CMD_QUERY_SEGMENTS:
      rc = DoQuerySegments(&f, open.hAdapter);
      break;
    case CMD_QUERY_FENCE:
      rc = DoQueryFence(&f, open.hAdapter);
      break;
    case CMD_WATCH_FENCE:
      rc = DoWatchFence(&f, open.hAdapter, watchSamples, watchIntervalMs, timeoutMsSet ? timeoutMs : 0);
      break;
    case CMD_QUERY_PERF:
      rc = DoQueryPerf(&f, open.hAdapter);
      break;
    case CMD_QUERY_SCANOUT:
      rc = DoQueryScanout(&f, open.hAdapter, (uint32_t)open.VidPnSourceId);
      break;
    case CMD_DUMP_SCANOUT_BMP:
      rc = DoDumpScanoutBmp(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, dumpScanoutBmpPath);
      break;
    case CMD_DUMP_SCANOUT_PNG:
      rc = DoDumpScanoutPng(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, dumpScanoutPngPath);
      break;
    case CMD_QUERY_CURSOR:
      rc = DoQueryCursor(&f, open.hAdapter);
      break;
    case CMD_DUMP_CURSOR_BMP:
      rc = DoDumpCursorBmp(&f, open.hAdapter, dumpCursorBmpPath);
      break;
    case CMD_DUMP_CURSOR_PNG:
      rc = DoDumpCursorPng(&f, open.hAdapter, dumpCursorPngPath);
      break;
    case CMD_DUMP_RING:
      rc = DoDumpRing(&f, open.hAdapter, ringId);
      break;
    case CMD_WATCH_RING:
      rc = DoWatchRing(&f, open.hAdapter, ringId, watchSamples, watchIntervalMs);
      break;
    case CMD_DUMP_LAST_CMD:
      rc = DoDumpLastCmd(&f, open.hAdapter, ringId, dumpLastCmdIndexFromTail, dumpLastCmdCount, dumpLastCmdOutPath,
                         dumpLastCmdAllocOutPath, dumpLastCmdForce);
      break;
    case CMD_DUMP_CREATEALLOCATION:
      rc = DoDumpCreateAllocation(&f, open.hAdapter, createAllocCsvPath, NULL);
      break;
    case CMD_DUMP_VBLANK:
      rc = DoDumpVblank(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs);
      break;
    case CMD_WAIT_VBLANK:
      rc = DoWaitVblank(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, timeoutMs, &skipCloseAdapter);
      break;
    case CMD_QUERY_SCANLINE:
      rc = DoQueryScanline(&f, open.hAdapter, (uint32_t)open.VidPnSourceId, vblankSamples, vblankIntervalMs);
      break;
    case CMD_MAP_SHARED_HANDLE:
      rc = DoMapSharedHandle(&f, open.hAdapter, mapSharedHandle);
      break;
    case CMD_READ_GPA:
      rc = DoReadGpa(&f, open.hAdapter, readGpa, readGpaSizeBytes, readGpaOutPath, readGpaForce);
      break;
    case CMD_SELFTEST:
      rc = DoSelftest(&f, open.hAdapter, timeoutMs, (uint32_t)open.VidPnSourceId);
      break;
    default:
      rc = 1;
      break;
    }
  }

  if (skipCloseAdapter || InterlockedCompareExchange(&g_skip_close_adapter, 0, 0) != 0) {
    // Avoid deadlock-prone cleanup when the vblank wait thread is potentially
    // stuck inside a kernel thunk (or when an escape call timed out).
    return rc;
  }

  D3DKMT_CLOSEADAPTER close;
  ZeroMemory(&close, sizeof(close));
  close.hAdapter = open.hAdapter;
  st = f.CloseAdapter(&close);
  if (!NT_SUCCESS(st)) {
    PrintNtStatus(L"D3DKMTCloseAdapter failed", &f, st);
    if (rc == 0) {
      // Preserve stable selftest exit codes: use an out-of-band nonzero value
      // for tool/transport failures so it won't be confused with a KMD-reported
      // selftest error_code.
      rc = (cmd == CMD_SELFTEST) ? 254 : 4;
    }
  }
  return rc;
}
