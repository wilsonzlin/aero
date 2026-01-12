#include "aerogpu_trace.h"

#include <algorithm>
#include <atomic>
#include <cctype>
#include <chrono>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <thread>

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
#endif

namespace aerogpu {
namespace {

constexpr HRESULT kTraceHrPending = static_cast<HRESULT>(0x7FFFFFFF);

// Keep the buffer small enough for dwm.exe but large enough to capture bring-up
// sequences (OpenAdapter -> CreateDevice -> Present / queries / surfaces).
constexpr uint32_t kTraceCapacity = 512;

std::atomic<bool> g_trace_inited{false};
std::atomic<bool> g_trace_enabled{false};

// Config is written once during init (DLL_PROCESS_ATTACH) before enabling the
// trace. Hot-path reads are gated on `g_trace_enabled`.
bool g_trace_unique_only = true;
uint32_t g_trace_max_records = kTraceCapacity;
uint32_t g_trace_dump_present_count = 0;
bool g_trace_dump_on_detach = false;
bool g_trace_dump_on_fail = false;

std::atomic<uint32_t> g_trace_write_index{0};
D3d9TraceRecord g_trace_records[kTraceCapacity]{};

constexpr uint32_t kFuncCount = static_cast<uint32_t>(D3d9TraceFunc::kCount);
constexpr uint32_t kSeenWordCount = (kFuncCount + 31) / 32;
std::atomic<uint32_t> g_trace_seen[kSeenWordCount]{};
uint32_t g_trace_filter[kSeenWordCount]{};
bool g_trace_filter_enabled = false;
uint32_t g_trace_filter_count = 0;

std::atomic<bool> g_trace_dumped{false};

uint32_t popcount_u32(uint32_t v) {
  uint32_t count = 0;
  while (v) {
    v &= (v - 1);
    ++count;
  }
  return count;
}

bool trace_icontains(const char* s, const char* needle_lower) {
  if (!s || !needle_lower) {
    return false;
  }
  if (*needle_lower == '\0') {
    return true;
  }
  const size_t needle_len = std::strlen(needle_lower);
  for (const char* p = s; *p; ++p) {
    size_t i = 0;
    while (i < needle_len && p[i] &&
           std::tolower(static_cast<unsigned char>(p[i])) == static_cast<unsigned char>(needle_lower[i])) {
      ++i;
    }
    if (i == needle_len) {
      return true;
    }
  }
  return false;
}

bool filter_allows(D3d9TraceFunc func) {
  if (!g_trace_filter_enabled) {
    return true;
  }
  const uint32_t id = static_cast<uint32_t>(func);
  if (id >= kFuncCount) {
    return true;
  }
  const uint32_t word_index = id / 32;
  const uint32_t bit = 1u << (id % 32);
  return (g_trace_filter[word_index] & bit) != 0;
}

uint64_t trace_timestamp() {
#if defined(_WIN32)
  LARGE_INTEGER li;
  QueryPerformanceCounter(&li);
  return static_cast<uint64_t>(li.QuadPart);
#else
  using namespace std::chrono;
  return static_cast<uint64_t>(duration_cast<nanoseconds>(steady_clock::now().time_since_epoch()).count());
#endif
}

uint32_t trace_thread_id() {
#if defined(_WIN32)
  return static_cast<uint32_t>(GetCurrentThreadId());
#else
  return static_cast<uint32_t>(std::hash<std::thread::id>{}(std::this_thread::get_id()));
#endif
}

void trace_out(const char* s) {
  if (!s) {
    return;
  }
#if defined(_WIN32)
  OutputDebugStringA(s);
#else
  fputs(s, stderr);
#endif
}

void trace_outf(const char* fmt, ...) {
  if (!fmt) {
    return;
  }

  char buf[512];
  va_list args;
  va_start(args, fmt);
  const int n = vsnprintf(buf, sizeof(buf), fmt, args);
  va_end(args);
  if (n < 0) {
    return;
  }
  trace_out(buf);
}

bool env_get(const char* name, char* out, size_t out_size) {
  if (!name || !out || out_size == 0) {
    return false;
  }
#if defined(_WIN32)
  const DWORD n = GetEnvironmentVariableA(name, out, static_cast<DWORD>(out_size));
  if (n == 0 || n >= out_size) {
    return false;
  }
  return true;
#else
  const char* v = std::getenv(name);
  if (!v || !*v) {
    return false;
  }
  std::snprintf(out, out_size, "%s", v);
  return true;
#endif
}

bool env_bool(const char* name) {
  char buf[32] = {};
  if (!env_get(name, buf, sizeof(buf))) {
    return false;
  }

  // Normalize.
  for (char& c : buf) {
    c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
  }

  return std::strcmp(buf, "1") == 0 || std::strcmp(buf, "true") == 0 || std::strcmp(buf, "yes") == 0 || std::strcmp(buf, "on") == 0;
}

uint32_t env_u32(const char* name, uint32_t default_value) {
  char buf[64] = {};
  if (!env_get(name, buf, sizeof(buf))) {
    return default_value;
  }

  char* end = nullptr;
  const unsigned long parsed = std::strtoul(buf, &end, 0);
  if (end == buf) {
    return default_value;
  }
  if (parsed > 0xFFFFFFFFul) {
    return 0xFFFFFFFFu;
  }
  return static_cast<uint32_t>(parsed);
}

const char* func_name(D3d9TraceFunc func) {
  switch (func) {
    case D3d9TraceFunc::OpenAdapter:
      return "OpenAdapter";
    case D3d9TraceFunc::OpenAdapter2:
      return "OpenAdapter2";
    case D3d9TraceFunc::OpenAdapterFromHdc:
      return "OpenAdapterFromHdc";
    case D3d9TraceFunc::OpenAdapterFromLuid:
      return "OpenAdapterFromLuid";
    case D3d9TraceFunc::AdapterClose:
      return "Adapter::CloseAdapter";
    case D3d9TraceFunc::AdapterGetCaps:
      return "Adapter::GetCaps";
    case D3d9TraceFunc::AdapterQueryAdapterInfo:
      return "Adapter::QueryAdapterInfo";
    case D3d9TraceFunc::AdapterCreateDevice:
      return "Adapter::CreateDevice";
    case D3d9TraceFunc::DeviceDestroy:
      return "Device::DestroyDevice";
    case D3d9TraceFunc::DeviceCreateResource:
      return "Device::CreateResource";
    case D3d9TraceFunc::DeviceOpenResource:
      return "Device::OpenResource";
    case D3d9TraceFunc::DeviceOpenResource2:
      return "Device::OpenResource2";
    case D3d9TraceFunc::DeviceDestroyResource:
      return "Device::DestroyResource";
    case D3d9TraceFunc::DeviceCreateSwapChain:
      return "Device::CreateSwapChain";
    case D3d9TraceFunc::DeviceDestroySwapChain:
      return "Device::DestroySwapChain";
    case D3d9TraceFunc::DeviceGetSwapChain:
      return "Device::GetSwapChain";
    case D3d9TraceFunc::DeviceSetSwapChain:
      return "Device::SetSwapChain";
    case D3d9TraceFunc::DeviceReset:
      return "Device::Reset";
    case D3d9TraceFunc::DeviceResetEx:
      return "Device::ResetEx";
    case D3d9TraceFunc::DeviceCheckDeviceState:
      return "Device::CheckDeviceState";
    case D3d9TraceFunc::DeviceRotateResourceIdentities:
      return "Device::RotateResourceIdentities";
    case D3d9TraceFunc::DeviceLock:
      return "Device::Lock";
    case D3d9TraceFunc::DeviceUnlock:
      return "Device::Unlock";
    case D3d9TraceFunc::DeviceGetRenderTargetData:
      return "Device::GetRenderTargetData";
    case D3d9TraceFunc::DeviceCopyRects:
      return "Device::CopyRects";
    case D3d9TraceFunc::DeviceSetRenderTarget:
      return "Device::SetRenderTarget";
    case D3d9TraceFunc::DeviceSetDepthStencil:
      return "Device::SetDepthStencil";
    case D3d9TraceFunc::DeviceSetViewport:
      return "Device::SetViewport";
    case D3d9TraceFunc::DeviceSetScissorRect:
      return "Device::SetScissorRect";
    case D3d9TraceFunc::DeviceSetTexture:
      return "Device::SetTexture";
    case D3d9TraceFunc::DeviceSetSamplerState:
      return "Device::SetSamplerState";
    case D3d9TraceFunc::DeviceSetRenderState:
      return "Device::SetRenderState";
    case D3d9TraceFunc::DeviceCreateVertexDecl:
      return "Device::CreateVertexDecl";
    case D3d9TraceFunc::DeviceSetVertexDecl:
      return "Device::SetVertexDecl";
    case D3d9TraceFunc::DeviceDestroyVertexDecl:
      return "Device::DestroyVertexDecl";
    case D3d9TraceFunc::DeviceCreateShader:
      return "Device::CreateShader";
    case D3d9TraceFunc::DeviceSetShader:
      return "Device::SetShader";
    case D3d9TraceFunc::DeviceDestroyShader:
      return "Device::DestroyShader";
    case D3d9TraceFunc::DeviceSetShaderConstF:
      return "Device::SetShaderConstF";
    case D3d9TraceFunc::DeviceBlt:
      return "Device::Blt";
    case D3d9TraceFunc::DeviceColorFill:
      return "Device::ColorFill";
    case D3d9TraceFunc::DeviceUpdateSurface:
      return "Device::UpdateSurface";
    case D3d9TraceFunc::DeviceUpdateTexture:
      return "Device::UpdateTexture";
    case D3d9TraceFunc::DeviceSetStreamSource:
      return "Device::SetStreamSource";
    case D3d9TraceFunc::DeviceSetIndices:
      return "Device::SetIndices";
    case D3d9TraceFunc::DeviceClear:
      return "Device::Clear";
    case D3d9TraceFunc::DeviceDrawPrimitive:
      return "Device::DrawPrimitive";
    case D3d9TraceFunc::DeviceDrawPrimitiveUP:
      return "Device::DrawPrimitiveUP";
    case D3d9TraceFunc::DeviceDrawIndexedPrimitive:
      return "Device::DrawIndexedPrimitive";
    case D3d9TraceFunc::DevicePresent:
      return "Device::Present";
    case D3d9TraceFunc::DevicePresentEx:
      return "Device::PresentEx";
    case D3d9TraceFunc::DeviceSetMaximumFrameLatency:
      return "Device::SetMaximumFrameLatency";
    case D3d9TraceFunc::DeviceGetMaximumFrameLatency:
      return "Device::GetMaximumFrameLatency";
    case D3d9TraceFunc::DeviceGetPresentStats:
      return "Device::GetPresentStats";
    case D3d9TraceFunc::DeviceGetLastPresentCount:
      return "Device::GetLastPresentCount";
    case D3d9TraceFunc::DeviceFlush:
      return "Device::Flush";
    case D3d9TraceFunc::DeviceWaitForVBlank:
      return "Device::WaitForVBlank";
    case D3d9TraceFunc::DeviceSetGPUThreadPriority:
      return "Device::SetGPUThreadPriority";
    case D3d9TraceFunc::DeviceGetGPUThreadPriority:
      return "Device::GetGPUThreadPriority";
    case D3d9TraceFunc::DeviceCheckResourceResidency:
      return "Device::CheckResourceResidency";
    case D3d9TraceFunc::DeviceQueryResourceResidency:
      return "Device::QueryResourceResidency";
    case D3d9TraceFunc::DeviceGetDisplayModeEx:
      return "Device::GetDisplayModeEx";
    case D3d9TraceFunc::DeviceComposeRects:
      return "Device::ComposeRects";
    case D3d9TraceFunc::DeviceCreateQuery:
      return "Device::CreateQuery";
    case D3d9TraceFunc::DeviceDestroyQuery:
      return "Device::DestroyQuery";
    case D3d9TraceFunc::DeviceIssueQuery:
      return "Device::IssueQuery";
    case D3d9TraceFunc::DeviceGetQueryData:
      return "Device::GetQueryData";
    case D3d9TraceFunc::DeviceWaitForIdle:
      return "Device::WaitForIdle";
    case D3d9TraceFunc::DeviceSetFVF:
      return "Device::SetFVF";
    case D3d9TraceFunc::DeviceSetTextureStageState:
      return "Device::SetTextureStageState (stub)";
    case D3d9TraceFunc::DeviceSetTransform:
      return "Device::SetTransform (stub)";
    case D3d9TraceFunc::DeviceMultiplyTransform:
      return "Device::MultiplyTransform (stub)";
    case D3d9TraceFunc::DeviceSetClipPlane:
      return "Device::SetClipPlane (stub)";
    case D3d9TraceFunc::DeviceSetShaderConstI:
      return "Device::SetShaderConstI (stub)";
    case D3d9TraceFunc::DeviceSetShaderConstB:
      return "Device::SetShaderConstB (stub)";
    case D3d9TraceFunc::DeviceSetMaterial:
      return "Device::SetMaterial (stub)";
    case D3d9TraceFunc::DeviceSetLight:
      return "Device::SetLight (stub)";
    case D3d9TraceFunc::DeviceLightEnable:
      return "Device::LightEnable (stub)";
    case D3d9TraceFunc::DeviceSetNPatchMode:
      return "Device::SetNPatchMode (stub)";
    case D3d9TraceFunc::DeviceSetStreamSourceFreq:
      return "Device::SetStreamSourceFreq (stub)";
    case D3d9TraceFunc::DeviceSetGammaRamp:
      return "Device::SetGammaRamp (stub)";
    case D3d9TraceFunc::DeviceCreateStateBlock:
      return "Device::CreateStateBlock";
    case D3d9TraceFunc::DeviceDeleteStateBlock:
      return "Device::DeleteStateBlock";
    case D3d9TraceFunc::DeviceCaptureStateBlock:
      return "Device::CaptureStateBlock";
    case D3d9TraceFunc::DeviceApplyStateBlock:
      return "Device::ApplyStateBlock";
    case D3d9TraceFunc::DeviceValidateDevice:
      return "Device::ValidateDevice";
    case D3d9TraceFunc::DeviceSetSoftwareVertexProcessing:
      return "Device::SetSoftwareVertexProcessing (stub)";
    case D3d9TraceFunc::DeviceSetCursorProperties:
      return "Device::SetCursorProperties (stub)";
    case D3d9TraceFunc::DeviceSetCursorPosition:
      return "Device::SetCursorPosition (stub)";
    case D3d9TraceFunc::DeviceShowCursor:
      return "Device::ShowCursor (stub)";
    case D3d9TraceFunc::DeviceSetPaletteEntries:
      return "Device::SetPaletteEntries (stub)";
    case D3d9TraceFunc::DeviceSetCurrentTexturePalette:
      return "Device::SetCurrentTexturePalette (stub)";
    case D3d9TraceFunc::DeviceSetClipStatus:
      return "Device::SetClipStatus (stub)";
    case D3d9TraceFunc::DeviceGetClipStatus:
      return "Device::GetClipStatus (stub)";
    case D3d9TraceFunc::DeviceGetGammaRamp:
      return "Device::GetGammaRamp (stub)";
    case D3d9TraceFunc::DeviceDrawRectPatch:
      return "Device::DrawRectPatch (stub)";
    case D3d9TraceFunc::DeviceDrawTriPatch:
      return "Device::DrawTriPatch (stub)";
    case D3d9TraceFunc::DeviceDeletePatch:
      return "Device::DeletePatch (stub)";
    case D3d9TraceFunc::DeviceProcessVertices:
      return "Device::ProcessVertices (stub)";
    case D3d9TraceFunc::DeviceGetRasterStatus:
      return "Device::GetRasterStatus";
    case D3d9TraceFunc::DeviceSetDialogBoxMode:
      return "Device::SetDialogBoxMode (stub)";
    case D3d9TraceFunc::DeviceDrawIndexedPrimitiveUP:
      return "Device::DrawIndexedPrimitiveUP";
    case D3d9TraceFunc::DeviceGetSoftwareVertexProcessing:
      return "Device::GetSoftwareVertexProcessing (stub)";
    case D3d9TraceFunc::DeviceGetTransform:
      return "Device::GetTransform (stub)";
    case D3d9TraceFunc::DeviceGetClipPlane:
      return "Device::GetClipPlane (stub)";
    case D3d9TraceFunc::DeviceGetViewport:
      return "Device::GetViewport";
    case D3d9TraceFunc::DeviceGetScissorRect:
      return "Device::GetScissorRect";
    case D3d9TraceFunc::DeviceBeginStateBlock:
      return "Device::BeginStateBlock";
    case D3d9TraceFunc::DeviceEndStateBlock:
      return "Device::EndStateBlock";
    case D3d9TraceFunc::DeviceGetMaterial:
      return "Device::GetMaterial (stub)";
    case D3d9TraceFunc::DeviceGetLight:
      return "Device::GetLight (stub)";
    case D3d9TraceFunc::DeviceGetLightEnable:
      return "Device::GetLightEnable (stub)";
    case D3d9TraceFunc::DeviceGetRenderTarget:
      return "Device::GetRenderTarget";
    case D3d9TraceFunc::DeviceGetDepthStencil:
      return "Device::GetDepthStencil";
    case D3d9TraceFunc::DeviceGetTexture:
      return "Device::GetTexture";
    case D3d9TraceFunc::DeviceGetTextureStageState:
      return "Device::GetTextureStageState (stub)";
    case D3d9TraceFunc::DeviceGetSamplerState:
      return "Device::GetSamplerState";
    case D3d9TraceFunc::DeviceGetRenderState:
      return "Device::GetRenderState";
    case D3d9TraceFunc::DeviceGetPaletteEntries:
      return "Device::GetPaletteEntries (stub)";
    case D3d9TraceFunc::DeviceGetCurrentTexturePalette:
      return "Device::GetCurrentTexturePalette (stub)";
    case D3d9TraceFunc::DeviceGetNPatchMode:
      return "Device::GetNPatchMode (stub)";
    case D3d9TraceFunc::DeviceGetFVF:
      return "Device::GetFVF";
    case D3d9TraceFunc::DeviceGetVertexDecl:
      return "Device::GetVertexDecl";
    case D3d9TraceFunc::DeviceGetStreamSource:
      return "Device::GetStreamSource";
    case D3d9TraceFunc::DeviceGetStreamSourceFreq:
      return "Device::GetStreamSourceFreq (stub)";
    case D3d9TraceFunc::DeviceGetIndices:
      return "Device::GetIndices";
    case D3d9TraceFunc::DeviceGetShader:
      return "Device::GetShader";
    case D3d9TraceFunc::DeviceGetShaderConstF:
      return "Device::GetShaderConstF";
    case D3d9TraceFunc::DeviceGetShaderConstI:
      return "Device::GetShaderConstI (stub)";
    case D3d9TraceFunc::DeviceGetShaderConstB:
      return "Device::GetShaderConstB (stub)";
    case D3d9TraceFunc::DeviceSetConvolutionMonoKernel:
      return "Device::SetConvolutionMonoKernel (stub)";
    case D3d9TraceFunc::DeviceSetAutoGenFilterType:
      return "Device::SetAutoGenFilterType (stub)";
    case D3d9TraceFunc::DeviceGetAutoGenFilterType:
      return "Device::GetAutoGenFilterType (stub)";
    case D3d9TraceFunc::DeviceGenerateMipSubLevels:
      return "Device::GenerateMipSubLevels (stub)";
    case D3d9TraceFunc::DeviceSetPriority:
      return "Device::SetPriority (stub)";
    case D3d9TraceFunc::DeviceGetPriority:
      return "Device::GetPriority (stub)";
    case D3d9TraceFunc::kCount:
      break;
  }
  return "Unknown";
}

bool should_log(D3d9TraceFunc func) {
  if (!g_trace_unique_only) {
    return true;
  }

  const uint32_t id = static_cast<uint32_t>(func);
  if (id >= kFuncCount) {
    return true;
  }

  const uint32_t word_index = id / 32;
  const uint32_t bit = 1u << (id % 32);

  const uint32_t word = g_trace_seen[word_index].load(std::memory_order_relaxed);
  if (word & bit) {
    return false;
  }

  const uint32_t prev = g_trace_seen[word_index].fetch_or(bit, std::memory_order_relaxed);
  return (prev & bit) == 0;
}

D3d9TraceRecord* alloc_record(D3d9TraceFunc func, uint64_t arg0, uint64_t arg1, uint64_t arg2, uint64_t arg3) {
  if (!g_trace_enabled.load(std::memory_order_acquire)) {
    return nullptr;
  }

  if (!filter_allows(func)) {
    return nullptr;
  }

  if (!should_log(func)) {
    return nullptr;
  }

  const uint32_t index = g_trace_write_index.fetch_add(1, std::memory_order_relaxed);
  if (index >= std::min(g_trace_max_records, kTraceCapacity)) {
    return nullptr;
  }

  D3d9TraceRecord* rec = &g_trace_records[index];
  rec->timestamp = trace_timestamp();
  rec->thread_id = trace_thread_id();
  rec->func_id = static_cast<uint32_t>(func);
  rec->arg0 = arg0;
  rec->arg1 = arg1;
  rec->arg2 = arg2;
  rec->arg3 = arg3;
  rec->hr = kTraceHrPending;
  return rec;
}

void dump_trace(const char* reason) {
  if (!g_trace_enabled.load(std::memory_order_acquire)) {
    return;
  }

  bool expected = false;
  if (!g_trace_dumped.compare_exchange_strong(expected, true, std::memory_order_acq_rel)) {
    return;
  }

  const uint32_t max_entries = std::min(g_trace_max_records, kTraceCapacity);
  const uint32_t recorded = std::min(g_trace_write_index.load(std::memory_order_relaxed), max_entries);

  trace_outf("aerogpu-d3d9-trace: dump reason=%s entries=%u mode=%s max=%u filter_on=%u filter_count=%u\n",
             reason ? reason : "(null)",
             static_cast<unsigned>(recorded),
             g_trace_unique_only ? "unique" : "all",
             static_cast<unsigned>(max_entries),
             static_cast<unsigned>(g_trace_filter_enabled ? 1u : 0u),
             static_cast<unsigned>(g_trace_filter_enabled ? g_trace_filter_count : kFuncCount));

  for (uint32_t i = 0; i < recorded; i++) {
    const D3d9TraceRecord& rec = g_trace_records[i];
    const auto func = static_cast<D3d9TraceFunc>(rec.func_id);
    const char* name = func_name(func);
    trace_outf("aerogpu-d3d9-trace: #%03u t=%llu tid=%u %s a0=0x%llx a1=0x%llx a2=0x%llx a3=0x%llx hr=0x%08x\n",
               static_cast<unsigned>(i),
               static_cast<unsigned long long>(rec.timestamp),
               static_cast<unsigned>(rec.thread_id),
               name,
               static_cast<unsigned long long>(rec.arg0),
               static_cast<unsigned long long>(rec.arg1),
               static_cast<unsigned long long>(rec.arg2),
               static_cast<unsigned long long>(rec.arg3),
               static_cast<unsigned>(rec.hr));
  }
}

} // namespace

bool d3d9_trace_enabled() {
  return g_trace_enabled.load(std::memory_order_acquire);
}

void d3d9_trace_init_from_env() {
  bool expected = false;
  if (!g_trace_inited.compare_exchange_strong(expected, true, std::memory_order_acq_rel)) {
    return;
  }

  const bool enabled = env_bool("AEROGPU_D3D9_TRACE");

  // Configure before publishing `enabled`.
  g_trace_unique_only = true;
  g_trace_filter_enabled = false;
  g_trace_filter_count = kFuncCount;
  std::memset(g_trace_filter, 0, sizeof(g_trace_filter));
  char mode[32] = {};
  if (env_get("AEROGPU_D3D9_TRACE_MODE", mode, sizeof(mode))) {
    for (char& c : mode) {
      c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    }
    if (std::strcmp(mode, "all") == 0) {
      g_trace_unique_only = false;
    }
  }

  g_trace_max_records = std::min(env_u32("AEROGPU_D3D9_TRACE_MAX", kTraceCapacity), kTraceCapacity);
  if (g_trace_max_records == 0) {
    g_trace_max_records = kTraceCapacity;
  }

  g_trace_dump_present_count = env_u32("AEROGPU_D3D9_TRACE_DUMP_PRESENT", 0);
  g_trace_dump_on_detach = env_bool("AEROGPU_D3D9_TRACE_DUMP_ON_DETACH");
  g_trace_dump_on_fail = env_bool("AEROGPU_D3D9_TRACE_DUMP_ON_FAIL");

  char filter[512] = {};
  if (env_get("AEROGPU_D3D9_TRACE_FILTER", filter, sizeof(filter))) {
    g_trace_filter_enabled = true;
    g_trace_filter_count = 0;
    std::memset(g_trace_filter, 0, sizeof(g_trace_filter));

    // Split on commas. Tokens are matched case-insensitively as substrings of the
    // `func_name()` string (e.g. `StateBlock` matches all stateblock DDIs).
    char* p = filter;
    while (p && *p) {
      while (*p == ',' || std::isspace(static_cast<unsigned char>(*p))) {
        ++p;
      }
      if (!*p) {
        break;
      }

      char* token = p;
      while (*p && *p != ',') {
        ++p;
      }
      if (*p == ',') {
        *p = '\0';
        ++p;
      }

      // Trim trailing whitespace.
      char* end = token + std::strlen(token);
      while (end > token && std::isspace(static_cast<unsigned char>(end[-1]))) {
        --end;
      }
      *end = '\0';

      // Lowercase the token in-place for matching.
      for (char* c = token; *c; ++c) {
        *c = static_cast<char>(std::tolower(static_cast<unsigned char>(*c)));
      }
      if (!*token) {
        continue;
      }

      for (uint32_t id = 0; id < kFuncCount; ++id) {
        const auto func = static_cast<D3d9TraceFunc>(id);
        const char* name = func_name(func);
        if (trace_icontains(name, token)) {
          const uint32_t word_index = id / 32;
          const uint32_t bit = 1u << (id % 32);
          g_trace_filter[word_index] |= bit;
        }
      }
    }

    for (uint32_t i = 0; i < kSeenWordCount; ++i) {
      g_trace_filter_count += popcount_u32(g_trace_filter[i]);
    }
  }

  if (!enabled) {
    return;
  }

  g_trace_enabled.store(true, std::memory_order_release);

  trace_outf(
      "aerogpu-d3d9-trace: enabled mode=%s max=%u dump_present=%u dump_on_detach=%u dump_on_fail=%u filter_on=%u filter_count=%u\n",
      g_trace_unique_only ? "unique" : "all",
      static_cast<unsigned>(g_trace_max_records),
      static_cast<unsigned>(g_trace_dump_present_count),
      static_cast<unsigned>(g_trace_dump_on_detach ? 1u : 0u),
      static_cast<unsigned>(g_trace_dump_on_fail ? 1u : 0u),
      static_cast<unsigned>(g_trace_filter_enabled ? 1u : 0u),
      static_cast<unsigned>(g_trace_filter_count));
}

void d3d9_trace_on_process_detach() {
  if (g_trace_dump_on_detach) {
    dump_trace("DLL_PROCESS_DETACH");
  }
}

void d3d9_trace_maybe_dump_on_present(uint32_t present_count) {
  if (g_trace_dump_present_count != 0 && present_count == g_trace_dump_present_count) {
    dump_trace("present_count");
  }
}

D3d9TraceCall::D3d9TraceCall(D3d9TraceFunc func, uint64_t arg0, uint64_t arg1, uint64_t arg2, uint64_t arg3) {
  record_ = alloc_record(func, arg0, arg1, arg2, arg3);
  if (record_) {
    hr_ = kTraceHrPending;
  }
}

D3d9TraceCall::~D3d9TraceCall() {
  if (record_) {
    record_->hr = hr_;
    if (g_trace_dump_on_fail && FAILED(hr_)) {
      dump_trace(func_name(static_cast<D3d9TraceFunc>(record_->func_id)));
    }
  }
}

} // namespace aerogpu
