// AeroGPU D3D10/11 UMD - shared internal encoder/state tracker.
//
// This header intentionally contains no WDK-specific types so it can be reused by
// both the repository "portable" build (minimal ABI subset) and the real Win7
// WDK build (`d3d10umddi.h` / `d3d11umddi.h`).
//
// The D3D10 and D3D11 DDIs are translated into the same AeroGPU command stream
// defined in `drivers/aerogpu/protocol/aerogpu_cmd.h`.
#pragma once

#include <algorithm>
#include <array>
#include <atomic>
#include <cassert>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <mutex>
#include <new>
#include <type_traits>
#include <unordered_map>
#include <utility>
#include <vector>

#include "aerogpu_cmd_writer.h"
#include "aerogpu_dxgi_format.h"
#include "../../common/aerogpu_win32_security.h"
#include "aerogpu_d3d10_11_log.h"
#include "aerogpu_d3d10_11_wddm_submit_alloc.h"
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include "aerogpu_d3d10_11_wddm_submit.h"
#endif
#include "../../../protocol/aerogpu_wddm_alloc.h"
#include "../../../protocol/aerogpu_umd_private.h"

#if defined(_WIN32)
  #include <windows.h>
#endif

namespace aerogpu::d3d10_11 {

#if defined(_WIN32)
// Some WDK/SDK revisions omit the NT_SUCCESS helper macro in user-mode header
// configurations. Prefer a local constexpr helper so WDK-only translation units
// don't need to carry their own fallback macros.
constexpr bool NtSuccess(NTSTATUS st) {
  return st >= 0;
}

// NTSTATUS constants commonly used by WDDM callbacks/thunks. Keep these numeric
// values centralized so WDK and portable Win32 builds remain consistent even
// when a given SDK/WDK revision doesn't expose a particular status macro in
// user-mode header configurations.
constexpr NTSTATUS kStatusTimeout = static_cast<NTSTATUS>(0x00000102L); // STATUS_TIMEOUT
constexpr NTSTATUS kStatusInvalidParameter = static_cast<NTSTATUS>(0xC000000DL); // STATUS_INVALID_PARAMETER
#endif

template <typename T>
inline void ResetObject(T* obj) {
  if (!obj) {
    return;
  }
  obj->~T();
  new (obj) T();
}

inline void LogModulePathOnce() {
#if defined(_WIN32)
  // Emit the exact DLL path once so bring-up on Win7 x64 can quickly confirm the
  // correct UMD bitness was loaded (System32 vs SysWOW64).
  static std::once_flag once;
  std::call_once(once, [] {
    HMODULE module = NULL;
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS |
                               GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCSTR>(&LogModulePathOnce),
                           &module)) {
      char path[MAX_PATH] = {};
      if (GetModuleFileNameA(module, path, static_cast<DWORD>(sizeof(path))) != 0) {
        OutputDebugStringA("aerogpu-d3d10_11: module_path=");
        OutputDebugStringA(path);
        OutputDebugStringA("\n");
      }
    }
  });
#endif
}

constexpr aerogpu_handle_t kInvalidHandle = 0;
// Driver-private "live cookie" values stamped into the first 4 bytes of device
// objects so we can quickly validate handle->pDrvPrivate pointers.
constexpr uint32_t kD3D10DeviceLiveCookie = 0xA3E0D310u;
constexpr uint32_t kD3D10_1DeviceLiveCookie = 0xA3E0D301u;
constexpr uint32_t kD3D11DeviceLiveCookie = 0xA3E0D311u;
// Back-compat alias used by existing D3D11/portable codepaths.
constexpr uint32_t kDeviceDestroyLiveCookie = kD3D11DeviceLiveCookie;

inline bool HasLiveCookie(const void* pDrvPrivate, uint32_t expected_cookie) {
  if (!pDrvPrivate) {
    return false;
  }
#if defined(_WIN32) && defined(_MSC_VER)
  __try {
    uint32_t cookie = 0;
    std::memcpy(&cookie, pDrvPrivate, sizeof(cookie));
    return cookie == expected_cookie;
  } __except (EXCEPTION_EXECUTE_HANDLER) {
    return false;
  }
#else
  uint32_t cookie = 0;
  std::memcpy(&cookie, pDrvPrivate, sizeof(cookie));
  return cookie == expected_cookie;
#endif
}

// Decodes a WDDM allocation-private-data blob into the latest (v2) struct layout.
//
// Older binaries may have emitted the v1 layout; this helper normalizes those to
// a v2-shaped struct for easier handling by UMD codepaths.
inline bool ConsumeWddmAllocPrivV2(const void* priv_data, size_t priv_data_size, aerogpu_wddm_alloc_priv_v2* out) {
  if (out) {
    std::memset(out, 0, sizeof(*out));
  }
  if (!out || !priv_data || priv_data_size < sizeof(aerogpu_wddm_alloc_priv)) {
    return false;
  }

  aerogpu_wddm_alloc_priv header{};
  std::memcpy(&header, priv_data, sizeof(header));
  if (header.magic != AEROGPU_WDDM_ALLOC_PRIV_MAGIC) {
    return false;
  }

  if (header.version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
    if (priv_data_size < sizeof(aerogpu_wddm_alloc_priv_v2)) {
      return false;
    }
    std::memcpy(out, priv_data, sizeof(*out));
    return true;
  }

  if (header.version == AEROGPU_WDDM_ALLOC_PRIV_VERSION) {
    out->magic = header.magic;
    out->version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
    out->alloc_id = header.alloc_id;
    out->flags = header.flags;
    out->share_token = header.share_token;
    out->size_bytes = header.size_bytes;
    out->reserved0 = header.reserved0;
    out->kind = AEROGPU_WDDM_ALLOC_KIND_UNKNOWN;
    out->width = 0;
    out->height = 0;
    out->format = 0;
    out->row_pitch_bytes = 0;
    out->reserved1 = 0;
    return true;
  }

  return false;
}

// Validates that a packed DDI function table contains no NULL entries.
//
// The Win7 D3D runtimes treat NULL function pointers as fatal; for bring-up we
// prefer failing early at device creation time instead of crashing later inside
// the runtime when it attempts to call through a missing entrypoint.
inline bool ValidateNoNullDdiTable(const char* name, const void* table, size_t bytes) {
  if (!table || bytes == 0) {
    return false;
  }
  if ((bytes % sizeof(void*)) != 0) {
    return false;
  }

  const auto* raw = reinterpret_cast<const unsigned char*>(table);
  const size_t count = bytes / sizeof(void*);
  for (size_t i = 0; i < count; ++i) {
    const size_t offset = i * sizeof(void*);
    bool all_zero = true;
    for (size_t j = 0; j < sizeof(void*); ++j) {
      if (raw[offset + j] != 0) {
        all_zero = false;
        break;
      }
    }
    if (!all_zero) {
      continue;
    }

#if defined(_WIN32)
    char buf[256] = {};
    std::snprintf(buf, sizeof(buf), "aerogpu-d3d10_11: NULL DDI entry in %s at index=%zu\n", name ? name : "?", i);
    OutputDebugStringA(buf);
#endif

#if !defined(NDEBUG)
    assert(false && "NULL DDI function pointer");
#endif
    return false;
  }
  return true;
}

template <typename T, typename = void>
struct has_member_pDrvPrivate : std::false_type {};
template <typename T>
struct has_member_pDrvPrivate<T, std::void_t<decltype(((T*)nullptr)->pDrvPrivate)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_backing_offset_bytes : std::false_type {};
template <typename T>
struct has_member_backing_offset_bytes<T, std::void_t<decltype(((T*)nullptr)->backing_offset_bytes)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_alloc_offset_bytes : std::false_type {};
template <typename T>
struct has_member_alloc_offset_bytes<T, std::void_t<decltype(((T*)nullptr)->alloc_offset_bytes)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_x : std::false_type {};
template <typename T>
struct has_member_viewport_x<T, std::void_t<decltype(((T*)nullptr)->viewport_x)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_y : std::false_type {};
template <typename T>
struct has_member_viewport_y<T, std::void_t<decltype(((T*)nullptr)->viewport_y)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_width : std::false_type {};
template <typename T>
struct has_member_viewport_width<T, std::void_t<decltype(((T*)nullptr)->viewport_width)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_height : std::false_type {};
template <typename T>
struct has_member_viewport_height<T, std::void_t<decltype(((T*)nullptr)->viewport_height)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_min_depth : std::false_type {};
template <typename T>
struct has_member_viewport_min_depth<T, std::void_t<decltype(((T*)nullptr)->viewport_min_depth)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_viewport_max_depth : std::false_type {};
template <typename T>
struct has_member_viewport_max_depth<T, std::void_t<decltype(((T*)nullptr)->viewport_max_depth)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_scissor_valid : std::false_type {};
template <typename T>
struct has_member_scissor_valid<T, std::void_t<decltype(((T*)nullptr)->scissor_valid)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_scissor_left : std::false_type {};
template <typename T>
struct has_member_scissor_left<T, std::void_t<decltype(((T*)nullptr)->scissor_left)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_scissor_top : std::false_type {};
template <typename T>
struct has_member_scissor_top<T, std::void_t<decltype(((T*)nullptr)->scissor_top)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_scissor_right : std::false_type {};
template <typename T>
struct has_member_scissor_right<T, std::void_t<decltype(((T*)nullptr)->scissor_right)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_scissor_bottom : std::false_type {};
template <typename T>
struct has_member_scissor_bottom<T, std::void_t<decltype(((T*)nullptr)->scissor_bottom)>> : std::true_type {};

// Shared resources can be opened multiple times (distinct Resource objects) yet
// refer to the same underlying allocation. Treat those as aliasing for SRV/RTV
// hazard mitigation.
template <typename ResourceT>
inline bool ResourcesAlias(const ResourceT* a, const ResourceT* b) {
  if (!a || !b) {
    return false;
  }
  if (a == b) {
    return true;
  }
  if (a->share_token != 0 && a->share_token == b->share_token) {
    return true;
  }

  uint32_t a_offset = 0;
  uint32_t b_offset = 0;
  if constexpr (has_member_backing_offset_bytes<ResourceT>::value) {
    a_offset = a->backing_offset_bytes;
    b_offset = b->backing_offset_bytes;
  } else if constexpr (has_member_alloc_offset_bytes<ResourceT>::value) {
    a_offset = a->alloc_offset_bytes;
    b_offset = b->alloc_offset_bytes;
  } else {
    static_assert(has_member_backing_offset_bytes<ResourceT>::value || has_member_alloc_offset_bytes<ResourceT>::value,
                  "ResourceT must expose backing_offset_bytes or alloc_offset_bytes");
  }

  if (a->backing_alloc_id != 0 &&
      a->backing_alloc_id == b->backing_alloc_id &&
      a_offset == b_offset) {
    return true;
  }
  return false;
}

// Generic "does this struct have member X?" helpers used by WDK-compat shims.
template <typename T, typename = void>
struct has_member_Desc : std::false_type {};
template <typename T>
struct has_member_Desc<T, std::void_t<decltype(((T*)nullptr)->Desc)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SamplerDesc : std::false_type {};
template <typename T>
struct has_member_SamplerDesc<T, std::void_t<decltype(((T*)nullptr)->SamplerDesc)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Filter : std::false_type {};
template <typename T>
struct has_member_Filter<T, std::void_t<decltype(((T*)nullptr)->Filter)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressU : std::false_type {};
template <typename T>
struct has_member_AddressU<T, std::void_t<decltype(((T*)nullptr)->AddressU)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressV : std::false_type {};
template <typename T>
struct has_member_AddressV<T, std::void_t<decltype(((T*)nullptr)->AddressV)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AddressW : std::false_type {};
template <typename T>
struct has_member_AddressW<T, std::void_t<decltype(((T*)nullptr)->AddressW)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Flags : std::false_type {};
template <typename T>
struct has_member_Flags<T, std::void_t<decltype(((T*)nullptr)->Flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_WriteOnly : std::false_type {};
template <typename T>
struct has_member_WriteOnly<T, std::void_t<decltype(((T*)nullptr)->WriteOnly)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Write : std::false_type {};
template <typename T>
struct has_member_Write<T, std::void_t<decltype(((T*)nullptr)->Write)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ReadOnly : std::false_type {};
template <typename T>
struct has_member_ReadOnly<T, std::void_t<decltype(((T*)nullptr)->ReadOnly)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Discard : std::false_type {};
template <typename T>
struct has_member_Discard<T, std::void_t<decltype(((T*)nullptr)->Discard)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NoOverwrite : std::false_type {};
template <typename T>
struct has_member_NoOverwrite<T, std::void_t<decltype(((T*)nullptr)->NoOverwrite)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NoOverWrite : std::false_type {};
template <typename T>
struct has_member_NoOverWrite<T, std::void_t<decltype(((T*)nullptr)->NoOverWrite)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DoNotWait : std::false_type {};
template <typename T>
struct has_member_DoNotWait<T, std::void_t<decltype(((T*)nullptr)->DoNotWait)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DonotWait : std::false_type {};
template <typename T>
struct has_member_DonotWait<T, std::void_t<decltype(((T*)nullptr)->DonotWait)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SubresourceIndex : std::false_type {};
template <typename T>
struct has_member_SubresourceIndex<T, std::void_t<decltype(((T*)nullptr)->SubresourceIndex)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SubResourceIndex : std::false_type {};
template <typename T>
struct has_member_SubResourceIndex<T, std::void_t<decltype(((T*)nullptr)->SubResourceIndex)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Offset : std::false_type {};
template <typename T>
struct has_member_Offset<T, std::void_t<decltype(((T*)nullptr)->Offset)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Size : std::false_type {};
template <typename T>
struct has_member_Size<T, std::void_t<decltype(((T*)nullptr)->Size)>> : std::true_type {};

template <typename THandle>
inline bool AnyNonNullHandles(const THandle* handles, size_t count) {
  if (!handles || count == 0) {
    return false;
  }
  if constexpr (!has_member_pDrvPrivate<THandle>::value) {
    return false;
  }
  for (size_t i = 0; i < count; ++i) {
    if (handles[i].pDrvPrivate) {
      return true;
    }
  }
  return false;
}

// D3D view descriptor sentinel values.
//
// D3D10/11 commonly use UINT(-1) for "all / keep existing" sentinel values. Some
// codepaths (including our portable ABI subset) also use 0 to mean "all
// remaining".
constexpr uint32_t kD3DUintAll = 0xFFFFFFFFu;
// Back-compat alias used by existing code when interpreting SRV MipLevels.
constexpr uint32_t kD3DMipLevelsAll = kD3DUintAll;
// D3D10/D3D11 append-aligned-element sentinel (AlignedByteOffset).
constexpr uint32_t kD3DAppendAlignedElement = kD3DUintAll;
// D3D11 UAV initial-count sentinel (keep existing counter value).
constexpr uint32_t kD3DUavInitialCountNoChange = kD3DUintAll;

// View dimension values for the portable AeroGPU ABI (and common WDDM/DDI view
// enums) used by our minimal view validation helpers.
constexpr uint32_t kD3DViewDimensionTexture2D = 3u;
constexpr uint32_t kD3DViewDimensionTexture2DArray = 4u;

inline bool D3dSrvMipLevelsIsAll(uint32_t view_mip_levels, uint32_t resource_mip_levels) {
  if (view_mip_levels == 0 || view_mip_levels == kD3DMipLevelsAll) {
    return true;
  }
  return view_mip_levels == resource_mip_levels;
}

// Normalizes a view descriptor count field (MipLevels/ArraySize) that uses
// 0/UINT(-1) to indicate "all remaining" into an explicit count value.
inline uint32_t D3dViewCountToRemaining(uint32_t base, uint32_t count, uint32_t total) {
  if (count == 0 || count == kD3DUintAll) {
    return (total > base) ? (total - base) : 0;
  }
  return count;
}

inline bool D3dViewDimensionIsTexture2D(uint32_t view_dimension) {
  bool ok = false;
  bool have_enum = false;
#if defined(_MSC_VER)
  // Prefer DDI-specific enumerators when available (varies across WDK revisions).
  __if_exists(D3D10DDIRESOURCE_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIRESOURCE_TEXTURE2D));
  }
  __if_exists(D3D11DDIRESOURCE_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIRESOURCE_TEXTURE2D));
  }
  __if_exists(D3D10DDIRESOURCE_VIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIRESOURCE_VIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D10_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11DDIRESOURCE_VIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIRESOURCE_VIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2D));
  }
  __if_exists(D3D11_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2D) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2D));
  }
#endif

  if (!have_enum) {
    ok = (view_dimension == kD3DViewDimensionTexture2D);
  }
  return ok;
}

inline bool D3dViewDimensionIsTexture2DArray(uint32_t view_dimension) {
  bool ok = false;
  bool have_enum = false;
#if defined(_MSC_VER)
  __if_exists(D3D10DDIRESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIRESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D10_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D10_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11DDIRESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIRESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_RESOURCE_VIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIRENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_RENDERTARGETVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDIDEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_DEPTHSTENCILVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11DDISHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY));
  }
  __if_exists(D3D11_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY) {
    have_enum = true;
    ok = ok || (view_dimension == static_cast<uint32_t>(D3D11_DDI_SHADERRESOURCEVIEW_DIMENSION_TEXTURE2DARRAY));
  }
#endif

  if (!have_enum) {
    ok = (view_dimension == kD3DViewDimensionTexture2DArray);
  }
  return ok;
}
constexpr uint32_t kMaxConstantBufferSlots = 14;
constexpr uint32_t kMaxShaderResourceSlots = 128;
constexpr uint32_t kMaxSamplerSlots = 16;
constexpr uint32_t kMaxUavSlots = 8;
// Back-compat alias: older code used this name for the compute UAV buffer slot count.
constexpr uint32_t kMaxUnorderedAccessBufferSlots = kMaxUavSlots;

// Common D3D10/D3D11 default mask values.
constexpr uint32_t kD3DSampleMaskAll = 0xFFFFFFFFu; // D3D11_DEFAULT_SAMPLE_MASK
constexpr uint32_t kD3DColorWriteMaskAll = 0xFu; // D3D11_COLOR_WRITE_ENABLE_ALL
constexpr uint8_t kD3DStencilMaskAll = 0xFFu; // default StencilReadMask/StencilWriteMask

// DXBC shader version token helper used by some DDI caps queries.
//
// The Windows D3D10/11 DDIs expose shader model support via a packed version
// token format:
//   (program_type << 16) | (major << 4) | minor
//
// Program type values are stable across shader models (see d3dcommon.h).
constexpr uint32_t kD3DDxbcProgramTypePixel = 0;
constexpr uint32_t kD3DDxbcProgramTypeVertex = 1;
constexpr uint32_t kD3DDxbcProgramTypeGeometry = 2;
constexpr uint32_t kD3DDxbcProgramTypeCompute = 5;

constexpr uint32_t DxbcShaderVersionToken(uint32_t program_type, uint32_t major, uint32_t minor) {
  return (program_type << 16) | (major << 4) | minor;
}

// D3D10/D3D11 Map type subset (numeric values from d3d10.h/d3d11.h).
constexpr uint32_t kD3DMapRead = 1;
constexpr uint32_t kD3DMapWrite = 2;
constexpr uint32_t kD3DMapReadWrite = 3;
constexpr uint32_t kD3DMapWriteDiscard = 4;
constexpr uint32_t kD3DMapWriteNoOverwrite = 5;
// Back-compat aliases used by older portable code.
constexpr uint32_t kD3D11MapRead = kD3DMapRead;
constexpr uint32_t kD3D11MapWrite = kD3DMapWrite;
constexpr uint32_t kD3D11MapReadWrite = kD3DMapReadWrite;
constexpr uint32_t kD3D11MapWriteDiscard = kD3DMapWriteDiscard;
constexpr uint32_t kD3D11MapWriteNoOverwrite = kD3DMapWriteNoOverwrite;

// D3D10/D3D11 Map flag subset (numeric values from d3d10.h/d3d11.h).
constexpr uint32_t kD3DMapFlagDoNotWait = 0x100000;
// Back-compat alias used by older portable code.
constexpr uint32_t kD3D11MapFlagDoNotWait = kD3DMapFlagDoNotWait;

// Sentinel timeout values used by AeroGPU fence wait helpers.
constexpr uint32_t kAeroGpuTimeoutMsInfinite = ~0u;
constexpr uint64_t kAeroGpuTimeoutU64Infinite = ~0ull;

// Common HRESULT values used by D3D10/11 map/unmap + WDDM waits.
constexpr HRESULT kDxgiErrorWasStillDrawing = static_cast<HRESULT>(0x887A000Au); // DXGI_ERROR_WAS_STILL_DRAWING
constexpr HRESULT kHrPending = static_cast<HRESULT>(0x8000000Au); // E_PENDING
constexpr HRESULT kHrWaitTimeout = static_cast<HRESULT>(0x80070102u); // HRESULT_FROM_WIN32(WAIT_TIMEOUT)
constexpr HRESULT kHrErrorTimeout = static_cast<HRESULT>(0x800705B4u); // HRESULT_FROM_WIN32(ERROR_TIMEOUT)
constexpr HRESULT kHrNtStatusTimeout = static_cast<HRESULT>(0x10000102u); // HRESULT_FROM_NT(STATUS_TIMEOUT)
constexpr HRESULT kHrNtStatusGraphicsGpuBusy =
    static_cast<HRESULT>(0xD01E0102L); // HRESULT_FROM_NT(STATUS_GRAPHICS_GPU_BUSY)

// D3D11_BIND_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;
constexpr uint32_t kD3D11BindDepthStencil = 0x40;
constexpr uint32_t kD3D11BindUnorderedAccess = 0x80;

// D3D10_BIND_* subset (numeric values from d3d10.h). These share values with the
// corresponding D3D11 bind flags for the overlapping subset we care about.
constexpr uint32_t kD3D10BindVertexBuffer = kD3D11BindVertexBuffer;
constexpr uint32_t kD3D10BindIndexBuffer = kD3D11BindIndexBuffer;
constexpr uint32_t kD3D10BindConstantBuffer = kD3D11BindConstantBuffer;
constexpr uint32_t kD3D10BindShaderResource = kD3D11BindShaderResource;
constexpr uint32_t kD3D10BindRenderTarget = kD3D11BindRenderTarget;
constexpr uint32_t kD3D10BindDepthStencil = kD3D11BindDepthStencil;

// D3D10-class IA supports 16 vertex buffer slots (D3D10_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT).
constexpr uint32_t kD3D10IaVertexInputResourceSlotCount = 16;
// D3D11-class IA supports 32 vertex buffer slots (D3D11_IA_VERTEX_INPUT_RESOURCE_SLOT_COUNT).
//
// This constant is stable across Windows versions and is used in the Win7 WDK
// D3D11 UMD without relying on WDK headers here.
constexpr uint32_t kD3D11IaVertexInputResourceSlotCount = 32;

// D3D11_CPU_ACCESS_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11CpuAccessWrite = 0x10000;
constexpr uint32_t kD3D11CpuAccessRead = 0x20000;

// D3D10_CPU_ACCESS_* subset (numeric values from d3d10.h). These share values
// with the corresponding D3D11 CPU access flags.
constexpr uint32_t kD3D10CpuAccessWrite = kD3D11CpuAccessWrite;
constexpr uint32_t kD3D10CpuAccessRead = kD3D11CpuAccessRead;

// D3D11_USAGE subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11UsageDefault = 0;
constexpr uint32_t kD3D11UsageImmutable = 1;
constexpr uint32_t kD3D11UsageDynamic = 2;
constexpr uint32_t kD3D11UsageStaging = 3;

// D3D10_USAGE subset (numeric values from d3d10.h). These share values with the
// corresponding D3D11 usage constants.
constexpr uint32_t kD3D10UsageDefault = kD3D11UsageDefault;
constexpr uint32_t kD3D10UsageImmutable = kD3D11UsageImmutable;
constexpr uint32_t kD3D10UsageDynamic = kD3D11UsageDynamic;
constexpr uint32_t kD3D10UsageStaging = kD3D11UsageStaging;

// D3D_FEATURE_LEVEL subset (numeric values from d3dcommon.h).
constexpr uint32_t kD3DFeatureLevel10_0 = 0xA000;

// D3D11DDICAPS_TYPE subset (numeric values from d3d11umddi.h).
//
// The Win7 D3D11 runtime routes common CheckFeatureSupport queries through the
// DDI `GetCaps` hook using these numeric values (which intentionally match the
// D3D11_FEATURE enum values for the overlapping subset).
//
// Keep these constants centralized so the portable (non-WDK) build and the WDK
// build stay consistent.
constexpr uint32_t kD3D11DdiCapsTypeThreading = 0;
constexpr uint32_t kD3D11DdiCapsTypeDoubles = 1;
constexpr uint32_t kD3D11DdiCapsTypeFormatSupport = 2;
constexpr uint32_t kD3D11DdiCapsTypeFormatSupport2 = 3;
constexpr uint32_t kD3D11DdiCapsTypeD3D10XHardwareOptions = 4;
constexpr uint32_t kD3D11DdiCapsTypeD3D11Options = 5;
constexpr uint32_t kD3D11DdiCapsTypeArchitectureInfo = 6;
constexpr uint32_t kD3D11DdiCapsTypeD3D9Options = 7;
// Win7-specific additions:
constexpr uint32_t kD3D11DdiCapsTypeFeatureLevels = 8;
constexpr uint32_t kD3D11DdiCapsTypeMultisampleQualityLevels = 9;

// D3D11_FORMAT_SUPPORT subset (numeric values from d3d11.h).
// These values are stable across Windows versions and are used by
// ID3D11Device::CheckFormatSupport.
constexpr uint32_t kD3D11FormatSupportBuffer = 0x1;
constexpr uint32_t kD3D11FormatSupportIaVertexBuffer = 0x2;
constexpr uint32_t kD3D11FormatSupportIaIndexBuffer = 0x4;
constexpr uint32_t kD3D11FormatSupportTexture2D = 0x20;
constexpr uint32_t kD3D11FormatSupportShaderLoad = 0x100;
constexpr uint32_t kD3D11FormatSupportShaderSample = 0x200;
constexpr uint32_t kD3D11FormatSupportRenderTarget = 0x4000;
constexpr uint32_t kD3D11FormatSupportBlendable = 0x8000;
constexpr uint32_t kD3D11FormatSupportDepthStencil = 0x10000;
constexpr uint32_t kD3D11FormatSupportCpuLockable = 0x20000;
constexpr uint32_t kD3D11FormatSupportDisplay = 0x80000;

// D3D11_RESOURCE_MISC_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11ResourceMiscShared = 0x2;
// Back-compat alias used by D3D10 paths (D3D10_RESOURCE_MISC_SHARED).
constexpr uint32_t kD3D10ResourceMiscShared = kD3D11ResourceMiscShared;
constexpr uint32_t kD3D11ResourceMiscSharedKeyedMutex = 0x100;
// Back-compat alias used by D3D10 paths (D3D10_RESOURCE_MISC_SHARED_KEYEDMUTEX).
constexpr uint32_t kD3D10ResourceMiscSharedKeyedMutex = kD3D11ResourceMiscSharedKeyedMutex;

inline uint32_t D3D11FormatSupportFlagsFromDxgiCapsMask(uint32_t caps) {
  uint32_t support = 0;
  if (caps & kAerogpuDxgiFormatCapTexture2D) {
    support |= kD3D11FormatSupportTexture2D;
  }
  if (caps & kAerogpuDxgiFormatCapRenderTarget) {
    support |= kD3D11FormatSupportRenderTarget;
  }
  if (caps & kAerogpuDxgiFormatCapDepthStencil) {
    support |= kD3D11FormatSupportDepthStencil;
  }
  if (caps & kAerogpuDxgiFormatCapShaderSample) {
    support |= kD3D11FormatSupportShaderSample;
  }
  if (caps & kAerogpuDxgiFormatCapDisplay) {
    support |= kD3D11FormatSupportDisplay;
  }
  if (caps & kAerogpuDxgiFormatCapBlendable) {
    support |= kD3D11FormatSupportBlendable;
  }
  if (caps & kAerogpuDxgiFormatCapCpuLockable) {
    support |= kD3D11FormatSupportCpuLockable;
  }
  if (caps & kAerogpuDxgiFormatCapBuffer) {
    // Buffers are accessed via shader-load operations (not sampling). Report
    // SHADER_LOAD for the buffer formats we expose so the runtime can validate
    // Buffer/BufferEx SRVs (including RAW views).
    support |= kD3D11FormatSupportBuffer | kD3D11FormatSupportShaderLoad;
  }
  if (caps & kAerogpuDxgiFormatCapIaVertexBuffer) {
    support |= kD3D11FormatSupportIaVertexBuffer;
  }
  if (caps & kAerogpuDxgiFormatCapIaIndexBuffer) {
    support |= kD3D11FormatSupportIaIndexBuffer;
  }
  return support;
}

template <typename T>
inline uint32_t D3D11FormatSupportFlags(const T* dev_or_adapter, uint32_t dxgi_format) {
  return D3D11FormatSupportFlagsFromDxgiCapsMask(AerogpuDxgiFormatCapsMask(dev_or_adapter, dxgi_format));
}

// D3D11 supports up to 128 shader-resource view slots per stage. We track the
// currently bound SRV resources so RotateResourceIdentities can re-emit bindings
// when swapchain backbuffer handles are rotated.
constexpr uint32_t kAeroGpuD3D11MaxSrvSlots = 128;

inline uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
//
// D3D semantic matching is case-insensitive. The AeroGPU ILAY protocol only stores a 32-bit hash
// (not the original string), so we must canonicalize the semantic name prior to hashing to preserve
// D3D semantics across the guest→host boundary.
//
// Canonical form: ASCII uppercase.
inline uint32_t HashSemanticName(const char* s) {
  if (!s) {
    return 0;
  }
  uint32_t hash = 2166136261u;
  for (const unsigned char* p = reinterpret_cast<const unsigned char*>(s); *p; ++p) {
    unsigned char c = *p;
    if (c >= 'a' && c <= 'z') {
      c = static_cast<unsigned char>(c - 'a' + 'A');
    }
    hash ^= c;
    hash *= 16777619u;
  }
  return hash;
}

// Aligns `value` up to the next multiple of `alignment`. `alignment` must be a
// power of two.
constexpr uint64_t AlignUpU64(uint64_t value, uint64_t alignment) {
  if (alignment == 0) {
    return value;
  }
  return (value + alignment - 1) & ~(alignment - 1);
}

// Aligns `value` down to the previous multiple of `alignment`. `alignment` must
// be a power of two.
constexpr uint64_t AlignDownU64(uint64_t value, uint64_t alignment) {
  if (alignment == 0) {
    return value;
  }
  return value & ~(alignment - 1);
}

// Aligns `value` up to the next multiple of `alignment`. `alignment` must be a
// power of two.
constexpr uint32_t AlignUpU32(uint32_t value, uint32_t alignment) {
  if (alignment == 0) {
    return value;
  }
  return static_cast<uint32_t>((value + alignment - 1) & ~(alignment - 1));
}

// Aligns `value` down to the previous multiple of `alignment`. `alignment` must
// be a power of two.
constexpr uint32_t AlignDownU32(uint32_t value, uint32_t alignment) {
  if (alignment == 0) {
    return value;
  }
  return static_cast<uint32_t>(value & ~(alignment - 1));
}

constexpr uint32_t ClampU64ToU32(uint64_t value) {
  if (value > static_cast<uint64_t>(std::numeric_limits<uint32_t>::max())) {
    return std::numeric_limits<uint32_t>::max();
  }
  return static_cast<uint32_t>(value);
}

struct AerogpuTextureFormatLayout {
  // For linear formats, block_width/block_height are 1 and bytes_per_block is
  // the bytes-per-texel value.
  //
  // For BC formats, block_width/block_height are 4 and bytes_per_block is the
  // bytes-per-4x4-block value.
  uint32_t block_width = 0;
  uint32_t block_height = 0;
  uint32_t bytes_per_block = 0;
  bool valid = false;
};

inline AerogpuTextureFormatLayout aerogpu_texture_format_layout(uint32_t aerogpu_format) {
  switch (aerogpu_format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
    case AEROGPU_FORMAT_D24_UNORM_S8_UINT:
    case AEROGPU_FORMAT_D32_FLOAT:
      return AerogpuTextureFormatLayout{1, 1, 4, true};
    case AEROGPU_FORMAT_B5G6R5_UNORM:
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
      return AerogpuTextureFormatLayout{1, 1, 2, true};
    case AEROGPU_FORMAT_BC1_RGBA_UNORM:
    case AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB:
      return AerogpuTextureFormatLayout{4, 4, 8, true};
    case AEROGPU_FORMAT_BC2_RGBA_UNORM:
    case AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB:
    case AEROGPU_FORMAT_BC3_RGBA_UNORM:
    case AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB:
    case AEROGPU_FORMAT_BC7_RGBA_UNORM:
    case AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB:
      return AerogpuTextureFormatLayout{4, 4, 16, true};
    default:
      return AerogpuTextureFormatLayout{};
  }
}

inline bool aerogpu_format_is_block_compressed(uint32_t aerogpu_format) {
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  return layout.valid && (layout.block_width != 1 || layout.block_height != 1);
}

inline uint32_t aerogpu_div_round_up_u32(uint32_t value, uint32_t divisor) {
  return (value + divisor - 1) / divisor;
}

inline uint32_t aerogpu_texture_min_row_pitch_bytes(uint32_t aerogpu_format, uint32_t width) {
  if (width == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width == 0 || layout.bytes_per_block == 0) {
    return 0;
  }

  const uint64_t blocks_w =
      static_cast<uint64_t>(aerogpu_div_round_up_u32(width, layout.block_width));
  const uint64_t row_bytes = blocks_w * static_cast<uint64_t>(layout.bytes_per_block);
  if (row_bytes == 0 || row_bytes > UINT32_MAX) {
    return 0;
  }
  return static_cast<uint32_t>(row_bytes);
}

inline uint32_t aerogpu_texture_num_rows(uint32_t aerogpu_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return aerogpu_div_round_up_u32(height, layout.block_height);
}

inline uint64_t aerogpu_texture_required_size_bytes(uint32_t aerogpu_format,
                                                    uint32_t row_pitch_bytes,
                                                    uint32_t height) {
  if (row_pitch_bytes == 0) {
    return 0;
  }
  const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, height);
  return static_cast<uint64_t>(row_pitch_bytes) * static_cast<uint64_t>(rows);
}

inline uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
  // Note: BC formats are block-compressed and do not have a bytes-per-texel
  // representation. Callers that operate on Texture2D memory layouts should use
  // `aerogpu_texture_format_layout` / `aerogpu_texture_min_row_pitch_bytes`.
  const AerogpuTextureFormatLayout layout = aerogpu_texture_format_layout(aerogpu_format);
  if (!layout.valid || layout.block_width != 1 || layout.block_height != 1) {
    return 0;
  }
  return layout.bytes_per_block;
}

inline uint32_t dxgi_index_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR32Uint:
      return AEROGPU_INDEX_FORMAT_UINT32;
    case kDxgiFormatR16Uint:
    default:
      return AEROGPU_INDEX_FORMAT_UINT16;
  }
}

inline uint32_t bind_flags_to_usage_flags_for_buffer(uint32_t bind_flags) {
  uint32_t usage = AEROGPU_RESOURCE_USAGE_NONE;
  if (bind_flags & kD3D11BindVertexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
  }
  if (bind_flags & kD3D11BindIndexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
  }
  if (bind_flags & kD3D11BindConstantBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER;
  }
  if (bind_flags & (kD3D11BindShaderResource | kD3D11BindUnorderedAccess)) {
    usage |= AEROGPU_RESOURCE_USAGE_STORAGE;
  }
  if (bind_flags & kD3D11BindRenderTarget) {
    usage |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (bind_flags & kD3D11BindDepthStencil) {
    usage |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return usage;
}

inline uint32_t bind_flags_to_usage_flags_for_texture(uint32_t bind_flags) {
  // Textures must always advertise TEXTURE usage regardless of bind flags.
  uint32_t usage = AEROGPU_RESOURCE_USAGE_TEXTURE;
  if (bind_flags & kD3D11BindRenderTarget) {
    usage |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (bind_flags & kD3D11BindDepthStencil) {
    usage |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return usage;
}

// Legacy helper used by older portable D3D10/11 UMD codepaths.
//
// Historically, these UMDs set `AEROGPU_RESOURCE_USAGE_TEXTURE` for textures
// explicitly when emitting CREATE_TEXTURE2D. Keep this helper as "buffer-style"
// usage mapping so buffers do not pick up TEXTURE usage when `bind_flags`
// contains D3D11_BIND_SHADER_RESOURCE.
inline uint32_t bind_flags_to_usage_flags(uint32_t bind_flags) {
  return bind_flags_to_usage_flags_for_buffer(bind_flags);
}

// Back-compat alias used by older call sites (e.g. portable UMD tests).
inline uint32_t bind_flags_to_buffer_usage_flags(uint32_t bind_flags) {
  return bind_flags_to_usage_flags_for_buffer(bind_flags);
}

inline uint32_t aerogpu_sampler_filter_from_d3d_filter(uint32_t filter) {
  // D3D10/11 point filtering is encoded as 0 for MIN_MAG_MIP_POINT. For the MVP
  // bring-up path, treat all non-point filters as linear.
  return filter == 0 ? AEROGPU_SAMPLER_FILTER_NEAREST : AEROGPU_SAMPLER_FILTER_LINEAR;
}

inline uint32_t aerogpu_sampler_address_from_d3d_mode(uint32_t mode) {
  // D3D10/11 numeric values: 1=WRAP, 2=MIRROR, 3=CLAMP, 4=BORDER, 5=MIRROR_ONCE.
  // The AeroGPU protocol currently supports REPEAT/MIRROR_REPEAT/CLAMP_TO_EDGE.
  switch (mode) {
    case 1:
      return AEROGPU_SAMPLER_ADDRESS_REPEAT;
    case 2:
      return AEROGPU_SAMPLER_ADDRESS_MIRROR_REPEAT;
    default:
      return AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  }
}

template <typename SamplerT, typename DescT>
inline void InitSamplerFromDesc(SamplerT* sampler, const DescT& desc) {
  if (!sampler) {
    return;
  }

  // D3D10/11 numeric defaults (MIN_MAG_POINT_MIP_LINEAR + CLAMP).
  uint32_t filter = 1;
  uint32_t addr_u = 3;
  uint32_t addr_v = 3;
  uint32_t addr_w = 3;
  if constexpr (has_member_Filter<DescT>::value) {
    filter = static_cast<uint32_t>(desc.Filter);
  }
  if constexpr (has_member_AddressU<DescT>::value) {
    addr_u = static_cast<uint32_t>(desc.AddressU);
  }
  if constexpr (has_member_AddressV<DescT>::value) {
    addr_v = static_cast<uint32_t>(desc.AddressV);
  }
  if constexpr (has_member_AddressW<DescT>::value) {
    addr_w = static_cast<uint32_t>(desc.AddressW);
  }

  sampler->filter = aerogpu_sampler_filter_from_d3d_filter(filter);
  sampler->address_u = aerogpu_sampler_address_from_d3d_mode(addr_u);
  sampler->address_v = aerogpu_sampler_address_from_d3d_mode(addr_v);
  sampler->address_w = aerogpu_sampler_address_from_d3d_mode(addr_w);
}

// Normalizes the different WDK CreateSampler descriptor layouts into the
// protocol-facing fields stored in our sampler objects.
template <typename SamplerT, typename CreateSamplerArgT>
inline void InitSamplerFromCreateSamplerArg(SamplerT* sampler, const CreateSamplerArgT* pDesc) {
  if (!sampler || !pDesc) {
    return;
  }

  if constexpr (has_member_Desc<CreateSamplerArgT>::value) {
    InitSamplerFromDesc(sampler, pDesc->Desc);
  } else if constexpr (has_member_SamplerDesc<CreateSamplerArgT>::value) {
    InitSamplerFromDesc(sampler, pDesc->SamplerDesc);
  } else {
    // Portable ABI: the create arg itself is the descriptor.
    InitSamplerFromDesc(sampler, *pDesc);
  }
}

template <typename LockT>
inline void InitLockForWrite(LockT* lock) {
  if (!lock) {
    return;
  }
  // `D3DDDICB_LOCKFLAGS` bit names vary slightly across WDK releases. Keep this
  // logic templated so the shared internal header stays WDK-free.
  if constexpr (has_member_Flags<LockT>::value) {
    std::memset(&lock->Flags, 0, sizeof(lock->Flags));
    using FlagsT = std::remove_reference_t<decltype(lock->Flags)>;
    if constexpr (has_member_WriteOnly<FlagsT>::value) {
      lock->Flags.WriteOnly = 1;
    }
    if constexpr (has_member_Write<FlagsT>::value) {
      lock->Flags.Write = 1;
    }
  }
}

template <typename LockT>
inline void InitLockArgsForMap(LockT* lock, uint32_t subresource, uint32_t map_type, uint32_t map_flags) {
  if (!lock) {
    return;
  }

  if constexpr (has_member_SubresourceIndex<LockT>::value) {
    lock->SubresourceIndex = subresource;
  }
  if constexpr (has_member_SubResourceIndex<LockT>::value) {
    lock->SubResourceIndex = subresource;
  }
  if constexpr (has_member_Offset<LockT>::value) {
    lock->Offset = 0;
  }
  if constexpr (has_member_Size<LockT>::value) {
    lock->Size = 0;
  }

  // D3D10/D3D11 share the same WDDM lock callback structure and flag bit
  // semantics. Keep this logic templated so the shared internal header stays
  // WDK-free.
  if constexpr (has_member_Flags<LockT>::value) {
    std::memset(&lock->Flags, 0, sizeof(lock->Flags));
    using FlagsT = std::remove_reference_t<decltype(lock->Flags)>;

    const bool do_not_wait = (map_flags & kD3DMapFlagDoNotWait) != 0;
    const bool is_read_only = (map_type == kD3DMapRead);
    const bool is_write_only =
        (map_type == kD3DMapWrite || map_type == kD3DMapWriteDiscard || map_type == kD3DMapWriteNoOverwrite);
    const bool discard = (map_type == kD3DMapWriteDiscard);
    const bool no_overwrite = (map_type == kD3DMapWriteNoOverwrite);

    if constexpr (has_member_DoNotWait<FlagsT>::value) {
      lock->Flags.DoNotWait = do_not_wait ? 1u : 0u;
    }
    if constexpr (has_member_DonotWait<FlagsT>::value) {
      lock->Flags.DonotWait = do_not_wait ? 1u : 0u;
    }

    if constexpr (has_member_ReadOnly<FlagsT>::value) {
      lock->Flags.ReadOnly = is_read_only ? 1u : 0u;
    }
    if constexpr (has_member_WriteOnly<FlagsT>::value) {
      lock->Flags.WriteOnly = is_write_only ? 1u : 0u;
    }
    if constexpr (has_member_Write<FlagsT>::value) {
      // For READ_WRITE the Win7 contract treats the lock as read+write (no
      // explicit "write" bit).
      lock->Flags.Write = is_write_only ? 1u : 0u;
    }
    if constexpr (has_member_Discard<FlagsT>::value) {
      lock->Flags.Discard = discard ? 1u : 0u;
    }
    if constexpr (has_member_NoOverwrite<FlagsT>::value) {
      lock->Flags.NoOverwrite = no_overwrite ? 1u : 0u;
    }
    if constexpr (has_member_NoOverWrite<FlagsT>::value) {
      lock->Flags.NoOverWrite = no_overwrite ? 1u : 0u;
    }
  }
}

template <typename UnlockT>
inline void InitUnlockArgsForMap(UnlockT* unlock, uint32_t subresource) {
  if (!unlock) {
    return;
  }
  if constexpr (has_member_SubresourceIndex<UnlockT>::value) {
    unlock->SubresourceIndex = subresource;
  }
  if constexpr (has_member_SubResourceIndex<UnlockT>::value) {
    unlock->SubResourceIndex = subresource;
  }
}

template <typename UnlockT>
inline void InitUnlockForWrite(UnlockT* unlock) {
  InitUnlockArgsForMap(unlock, /*subresource=*/0);
}

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

// Some WDK/runtime combinations omit `D3DDDICB_LOCK::Pitch` or report it as 0 for
// non-surface allocations. When a non-zero pitch is reported, validate only that
// it is large enough to contain a texel row for the resource's mip0.
template <typename DeviceT, typename ResourceT>
inline bool ValidateWddmTexturePitch(const DeviceT* dev, const ResourceT* res, uint32_t wddm_pitch) {
  if (!res) {
    return true;
  }
  if (static_cast<uint32_t>(res->kind) != static_cast<uint32_t>(ResourceKind::Texture2D)) {
    return true;
  }
  // Only validate when the runtime provides a non-zero pitch.
  if (wddm_pitch == 0) {
    return true;
  }
  if (!dev || res->width == 0) {
    return false;
  }

  const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
  if (aer_fmt == AEROGPU_FORMAT_INVALID) {
    return false;
  }
  const uint32_t min_row_bytes = aerogpu_texture_min_row_pitch_bytes(aer_fmt, res->width);
  if (min_row_bytes == 0) {
    return false;
  }
  return wddm_pitch >= min_row_bytes;
}

struct Texture2DSubresourceLayout {
  uint32_t mip_level = 0;
  uint32_t array_layer = 0;
  uint32_t width = 0;
  uint32_t height = 0;
  uint64_t offset_bytes = 0;
  // Row pitch in bytes (texel rows for linear formats, block rows for BC).
  uint32_t row_pitch_bytes = 0;
  // Number of "layout rows" in this subresource (texel rows for linear formats, block rows for BC).
  uint32_t rows_in_layout = 0;
  uint64_t size_bytes = 0;
};

inline uint32_t aerogpu_mip_dim(uint32_t base, uint32_t mip_level) {
  if (base == 0) {
    return 0;
  }
  const uint32_t shifted = (mip_level >= 32) ? 0u : (base >> mip_level);
  return std::max(1u, shifted);
}

// D3D10/10.1/11 semantics: when the API/DDI passes `MipLevels == 0` for a 2D
// texture, it means "allocate the full mip chain" down to 1x1.
//
// (This is not the same as "1 mip"; treating it as such causes applications
// that rely on full-chain sampling or GenerateMips to silently see only mip0.)
inline uint32_t CalcFullMipLevels(uint32_t width, uint32_t height) {
  uint32_t w = width ? width : 1u;
  uint32_t h = height ? height : 1u;
  uint32_t levels = 1;
  while (w > 1 || h > 1) {
    w = (w > 1) ? (w / 2) : 1u;
    h = (h > 1) ? (h / 2) : 1u;
    levels++;
  }
  return levels;
}

inline bool build_texture2d_subresource_layouts(uint32_t aerogpu_format,
                                                uint32_t width,
                                                uint32_t height,
                                                uint32_t mip_levels,
                                                uint32_t array_layers,
                                                uint32_t mip0_row_pitch_bytes,
                                                std::vector<Texture2DSubresourceLayout>* out_layouts,
                                                uint64_t* out_total_bytes) {
  if (!out_layouts || !out_total_bytes) {
    return false;
  }
  out_layouts->clear();
  *out_total_bytes = 0;

  if (width == 0 || height == 0 || mip_levels == 0 || array_layers == 0) {
    return false;
  }
  if (mip0_row_pitch_bytes == 0) {
    return false;
  }

  const uint64_t subresource_count = static_cast<uint64_t>(mip_levels) * static_cast<uint64_t>(array_layers);
  if (subresource_count == 0 || subresource_count > static_cast<uint64_t>(SIZE_MAX)) {
    return false;
  }
  try {
    out_layouts->reserve(static_cast<size_t>(subresource_count));
  } catch (...) {
    return false;
  }

  uint64_t offset = 0;
  for (uint32_t layer = 0; layer < array_layers; ++layer) {
    for (uint32_t mip = 0; mip < mip_levels; ++mip) {
      const uint32_t mip_w = aerogpu_mip_dim(width, mip);
      const uint32_t mip_h = aerogpu_mip_dim(height, mip);
      const uint32_t tight_row_pitch = aerogpu_texture_min_row_pitch_bytes(aerogpu_format, mip_w);
      const uint32_t rows = aerogpu_texture_num_rows(aerogpu_format, mip_h);
      if (tight_row_pitch == 0 || rows == 0) {
        return false;
      }

      const uint32_t row_pitch = (mip == 0) ? mip0_row_pitch_bytes : tight_row_pitch;
      if (row_pitch < tight_row_pitch) {
        return false;
      }

      const uint64_t size_bytes = static_cast<uint64_t>(row_pitch) * static_cast<uint64_t>(rows);
      if (size_bytes == 0) {
        return false;
      }

      Texture2DSubresourceLayout layout{};
      layout.mip_level = mip;
      layout.array_layer = layer;
      layout.width = mip_w;
      layout.height = mip_h;
      layout.offset_bytes = offset;
      layout.row_pitch_bytes = row_pitch;
      layout.rows_in_layout = rows;
      layout.size_bytes = size_bytes;
      try {
        out_layouts->push_back(layout);
      } catch (...) {
        return false;
      }

      const uint64_t next = offset + size_bytes;
      if (next < offset) {
        return false;
      }
      offset = next;
    }
  }

  *out_total_bytes = offset;
  return true;
}

template <typename DeviceT, typename ResourceT>
inline uint64_t resource_total_bytes(const DeviceT* dev, const ResourceT* res) {
  if (!res) {
    return 0;
  }
  const uint32_t kind = static_cast<uint32_t>(res->kind);
  if (kind == static_cast<uint32_t>(ResourceKind::Buffer)) {
    return res->size_bytes;
  }
  if (kind == static_cast<uint32_t>(ResourceKind::Texture2D)) {
    if (!res->tex2d_subresources.empty()) {
      const Texture2DSubresourceLayout& last = res->tex2d_subresources.back();
      const uint64_t end = last.offset_bytes + last.size_bytes;
      if (end < last.offset_bytes) {
        return 0;
      }
      return end;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu_compat(dev, res->dxgi_format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return 0;
    }
    return aerogpu_texture_required_size_bytes(aer_fmt, res->row_pitch_bytes, res->height);
  }
  return 0;
}

struct Adapter {
  std::atomic<uint32_t> next_handle{1};

  // Opaque pointer to the runtime's adapter callback table (WDK type depends on
  // D3D10 vs D3D11 and the negotiated interface version).
  const void* runtime_callbacks = nullptr;
  // Negotiated `D3D10DDIARG_OPENADAPTER::Version` value for the D3D11 DDI.
  // Stored so device creation can validate that it is filling function tables
  // matching the negotiated struct layout.
  uint32_t d3d11_ddi_version = 0;

  aerogpu_umd_private_v1 umd_private = {};
  bool umd_private_valid = false;
  // Optional kernel adapter handle (D3DKMT_HANDLE in the WDK headers), opened via
  // D3DKMTOpenAdapterFromHdc for direct D3DKMT calls. Stored as u32 so this
  // shared header stays WDK-independent.
  uint32_t kmt_adapter = 0;

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

#if defined(_WIN32)
namespace detail {

// SplitMix64 mixing function (public domain). Used to scramble fallback entropy.
inline uint64_t splitmix64(uint64_t x) {
  x += 0x9E3779B97F4A7C15ULL;
  x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
  x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
  return x ^ (x >> 31);
}

inline bool fill_random_bytes(void* out, size_t len) {
  if (!out || len == 0) {
    return false;
  }

  using RtlGenRandomFn = BOOLEAN(WINAPI*)(PVOID, ULONG);
  using BCryptGenRandomFn = LONG(WINAPI*)(void* hAlgorithm, unsigned char* pbBuffer, ULONG cbBuffer, ULONG dwFlags);

  static RtlGenRandomFn rtl_gen_random = []() -> RtlGenRandomFn {
    HMODULE advapi = GetModuleHandleW(L"advapi32.dll");
    if (!advapi) {
      advapi = LoadLibraryW(L"advapi32.dll");
    }
    if (!advapi) {
      return nullptr;
    }
    return reinterpret_cast<RtlGenRandomFn>(GetProcAddress(advapi, "SystemFunction036"));
  }();

  if (rtl_gen_random) {
    if (rtl_gen_random(out, static_cast<ULONG>(len)) != FALSE) {
      return true;
    }
  }

  static BCryptGenRandomFn bcrypt_gen_random = []() -> BCryptGenRandomFn {
    HMODULE bcrypt = GetModuleHandleW(L"bcrypt.dll");
    if (!bcrypt) {
      bcrypt = LoadLibraryW(L"bcrypt.dll");
    }
    if (!bcrypt) {
      return nullptr;
    }
    return reinterpret_cast<BCryptGenRandomFn>(GetProcAddress(bcrypt, "BCryptGenRandom"));
  }();

  if (bcrypt_gen_random) {
    constexpr ULONG kBcryptUseSystemPreferredRng = 0x00000002UL; // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    const LONG st = bcrypt_gen_random(nullptr,
                                     reinterpret_cast<unsigned char*>(out),
                                     static_cast<ULONG>(len),
                                     kBcryptUseSystemPreferredRng);
    if (st >= 0) {
      return true;
    }
  }

  return false;
}

inline uint64_t fallback_entropy(uint64_t counter) {
  uint64_t entropy = counter;
  entropy ^= (static_cast<uint64_t>(GetCurrentProcessId()) << 32);
  entropy ^= static_cast<uint64_t>(GetCurrentThreadId());

  LARGE_INTEGER qpc{};
  if (QueryPerformanceCounter(&qpc)) {
    entropy ^= static_cast<uint64_t>(qpc.QuadPart);
  }

  entropy ^= static_cast<uint64_t>(GetTickCount64());
  return entropy;
}

inline aerogpu_handle_t allocate_rng_fallback_handle() {
  static std::atomic<uint64_t> g_counter{1};
  static const uint64_t g_salt = []() -> uint64_t {
    uint64_t salt = 0;
    if (fill_random_bytes(&salt, sizeof(salt)) && salt != 0) {
      return salt;
    }
    return splitmix64(fallback_entropy(0));
  }();

  for (;;) {
    const uint64_t ctr = g_counter.fetch_add(1, std::memory_order_relaxed);
    const uint64_t mixed = splitmix64(g_salt ^ fallback_entropy(ctr));
    const uint32_t low31 = static_cast<uint32_t>(mixed & 0x7FFFFFFFu);
    if (low31 != 0) {
      return static_cast<aerogpu_handle_t>(0x80000000u | low31);
    }
  }
}

inline void log_global_handle_fallback_once() {
  static std::once_flag once;
  std::call_once(once, [] {
    OutputDebugStringA(
        "aerogpu-d3d10_11: GlobalHandleCounter mapping unavailable; using RNG fallback\n");
  });
}

} // namespace detail
#endif // defined(_WIN32)

template <typename TAdapter>
inline aerogpu_handle_t AllocateGlobalHandle(TAdapter* adapter) {
  if (!adapter) {
    return kInvalidHandle;
  }
#if defined(_WIN32)
  static std::mutex g_mutex;
  static HANDLE g_mapping = nullptr;
  static void* g_view = nullptr;

  std::lock_guard<std::mutex> lock(g_mutex);

  if (!g_view) {
    const wchar_t* name = L"Local\\AeroGPU.GlobalHandleCounter";

    // Use a permissive DACL so other processes in the session can open and
    // update the counter (e.g. DWM, sandboxed apps, different integrity levels).
    HANDLE mapping =
        ::aerogpu::win32::CreateFileMappingWBestEffortLowIntegrity(
            INVALID_HANDLE_VALUE, PAGE_READWRITE, 0, sizeof(uint64_t), name);
    if (mapping) {
      void* view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(uint64_t));
      if (view) {
        g_mapping = mapping;
        g_view = view;
      } else {
        CloseHandle(mapping);
      }
    }
  }

  if (g_view) {
    auto* counter = reinterpret_cast<volatile LONG64*>(g_view);
    LONG64 token = InterlockedIncrement64(counter);
    if ((static_cast<uint64_t>(token) & 0x7FFFFFFFULL) == 0) {
      token = InterlockedIncrement64(counter);
    }
    return static_cast<aerogpu_handle_t>(static_cast<uint64_t>(token) & 0xFFFFFFFFu);
  }

  detail::log_global_handle_fallback_once();
  return detail::allocate_rng_fallback_handle();
#else

  aerogpu_handle_t handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  if (handle == kInvalidHandle) {
    handle = adapter->next_handle.fetch_add(1, std::memory_order_relaxed);
  }
  return handle;
#endif
}

#if defined(_WIN32)
inline bool GetPrimaryDisplayName(wchar_t out[CCHDEVICENAME]) {
  if (!out) {
    return false;
  }

  DISPLAY_DEVICEW dd;
  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);

  for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  ZeroMemory(&dd, sizeof(dd));
  dd.cb = sizeof(dd);
  for (DWORD i = 0; EnumDisplayDevicesW(nullptr, i, &dd, 0); ++i) {
    if ((dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0) {
      wcsncpy(out, dd.DeviceName, CCHDEVICENAME - 1);
      out[CCHDEVICENAME - 1] = 0;
      return true;
    }
    ZeroMemory(&dd, sizeof(dd));
    dd.cb = sizeof(dd);
  }

  wcsncpy(out, L"\\\\.\\DISPLAY1", CCHDEVICENAME - 1);
  out[CCHDEVICENAME - 1] = 0;
  return true;
}
#endif

struct Resource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  // Host-visible guest backing allocation ID. 0 means the resource is host-owned
  // and must be updated via `AEROGPU_CMD_UPLOAD_RESOURCE` payloads.
  uint32_t backing_alloc_id = 0;
  // Byte offset into the guest allocation described by `backing_alloc_id`.
  uint32_t backing_offset_bytes = 0;
  // WDDM allocation handle (D3DKMT_HANDLE in the WDK headers) used for runtime
  // callbacks such as LockCb/UnlockCb. This is stored as a u32 so the shared
  // header stays WDK-independent.
  uint32_t wddm_allocation_handle = 0;

  // Stable cross-process token used by EXPORT/IMPORT_SHARED_SURFACE.
  //
  // Do not confuse this with the numeric value of the user-mode shared `HANDLE` returned by
  // IDXGIResource::GetSharedHandle(): NT `HANDLE` values are process-local (often different after
  // DuplicateHandle), and some stacks use token-style shared handles. See:
  // docs/graphics/win7-shared-surfaces-share-token.md
  //
  // 0 if the resource is not shareable.
  uint64_t share_token = 0;

  // True if this resource was created as shareable (D3D10/D3D11 `*_RESOURCE_MISC_SHARED`).
  bool is_shared = false;
  // True if this resource is an imported alias created via OpenResource/OpenSharedResource.
  bool is_shared_alias = false;

  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;
  uint32_t usage = kD3D11UsageDefault;
  uint32_t cpu_access_flags = 0;

  // WDDM identity (kernel-mode handles / allocation identities). DXGI swapchains
  // on Win7 rotate backbuffers by calling pfnRotateResourceIdentities; when
  // resources are backed by real WDDM allocations, these must rotate alongside
  // the AeroGPU handle.
  struct WddmIdentity {
    uint64_t km_resource_handle = 0;
    std::vector<uint64_t> km_allocation_handles;
  } wddm;

  // Buffer fields.
  uint64_t size_bytes = 0;
  // Structure byte stride for structured buffers (D3D11_BUFFER_DESC::StructureByteStride).
  // 0 means "not a structured buffer / unknown".
  uint32_t structure_stride_bytes = 0;

  // Texture2D fields.
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t mip_levels = 1;
  uint32_t array_size = 1;
  uint32_t dxgi_format = 0;
  uint32_t row_pitch_bytes = 0;
  std::vector<Texture2DSubresourceLayout> tex2d_subresources;

  // CPU-visible backing storage for resource uploads / staging reads.
  std::vector<uint8_t> storage;

  // Fence value of the most recent GPU submission that writes into this resource
  // (conservative). Used by the WDK D3D11 UMD for staging readback Map(READ)
  // synchronization.
  uint64_t last_gpu_write_fence = 0;

  // Map/unmap tracking (system-memory-backed implementation).
  bool mapped = false;
  uint32_t mapped_map_type = 0;
  uint32_t mapped_map_flags = 0;
  uint32_t mapped_subresource = 0;
  uint64_t mapped_offset = 0;
  uint64_t mapped_size = 0;

  // Win7/WDDM 1.1 runtime mapping state.
  //
  // The WDK UMDs map runtime-managed allocations via `pfnLockCb`/`pfnUnlockCb`.
  // We keep these fields WDK-free (plain integers/pointers) so the core
  // `Resource` struct can be shared with the non-WDK build.
  void* mapped_wddm_ptr = nullptr;
  uint64_t mapped_wddm_allocation = 0;
  uint32_t mapped_wddm_pitch = 0;
  uint32_t mapped_wddm_slice_pitch = 0;
};

struct Shader {
  aerogpu_handle_t handle = 0;
  uint32_t stage = AEROGPU_SHADER_STAGE_VERTEX;
  std::vector<uint8_t> dxbc;
  bool forced_ndc_z_valid = false;
  float forced_ndc_z = 0.0f;
};

struct InputLayout {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct RenderTargetView {
  aerogpu_handle_t texture = 0;
  Resource* resource = nullptr;
};

struct DepthStencilView {
  aerogpu_handle_t texture = 0;
  Resource* resource = nullptr;
};

// Pipeline state objects are accepted and can be bound, but the host translator
// may use conservative defaults until more encoding is implemented.
struct BlendState {
  uint32_t blend_enable = 0;
  uint32_t src_blend = 0;
  uint32_t dest_blend = 0;
  uint32_t blend_op = 0;
  uint32_t src_blend_alpha = 0;
  uint32_t dest_blend_alpha = 0;
  uint32_t blend_op_alpha = 0;
  uint32_t render_target_write_mask = kD3DColorWriteMaskAll;
};
struct RasterizerState {
  // Stored as raw numeric values so this header remains WDK-free.
  uint32_t fill_mode = 0;
  uint32_t cull_mode = 0;
  uint32_t front_ccw = 0;
  uint32_t scissor_enable = 0;
  int32_t depth_bias = 0;
  uint32_t depth_clip_enable = 1u;
};
struct DepthStencilState {
  // Stored as raw numeric values so this header remains WDK-free.
  uint32_t depth_enable = 0;
  uint32_t depth_write_mask = 0;
  uint32_t depth_func = 0;
  uint32_t stencil_enable = 0;
  uint8_t stencil_read_mask = kD3DStencilMaskAll;
  uint8_t stencil_write_mask = kD3DStencilMaskAll;
};

struct Device {
  uint32_t destroy_cookie = kDeviceDestroyLiveCookie;
  Adapter* adapter = nullptr;
  // Opaque pointer to the runtime's device callback table (contains e.g.
  // pfnSetErrorCb).
  const void* runtime_callbacks = nullptr;
  // Opaque pointer to the runtime's shared WDDM device callback table
  // (`D3DDDI_DEVICECALLBACKS`). Populated by the WDK D3D11 build for real Win7
  // WDDM submissions + fence waits, including LockCb/UnlockCb.
  const void* runtime_ddi_callbacks = nullptr;
  // Opaque pointer to the runtime device handle's private storage. This is used
  // for callbacks that require a `*HRTDEVICE` (e.g. `pfnSetErrorCb`) without
  // including WDK-specific handle types in this shared header.
  void* runtime_device = nullptr;
  // Driver-private pointer backing the immediate context handle. Stored so we
  // can adapt DDIs that sometimes move between device vs context tables across
  // D3D11 DDI interface versions (e.g. Present/RotateResourceIdentities).
  void* immediate_context = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // WDDM submission state (Win7/WDDM 1.1). Handles are stored as plain integers
  // to keep this header WDK-free; the WDK build casts them to `D3DKMT_HANDLE`.
  uint32_t kmt_device = 0;
  uint32_t kmt_context = 0;
  uint32_t kmt_fence_syncobj = 0;
  // Runtime-provided per-DMA-buffer private data (if exposed by CreateContext).
  // Some WDK vintages do not expose this in Allocate/GetCommandBuffer, so keep
  // the CreateContext-provided pointer as a fallback.
  void* wddm_dma_private_data = nullptr;
  uint32_t wddm_dma_private_data_bytes = 0;
  volatile uint64_t* monitored_fence_value = nullptr;
#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  // Shared Win7/WDDM 1.1 submission helper. Only available in WDK builds.
  WddmSubmit wddm_submit;
#endif

  // WDDM allocation handles (D3DKMT_HANDLE values) to include in each submission's
  // allocation list, along with per-allocation read/write tracking used to set
  // DXGK_ALLOCATIONLIST::WriteOperation precisely.
  //
  // This is rebuilt for each command buffer submission so the KMD can attach an
  // allocation table that resolves `backing_alloc_id` values in the AeroGPU
  // command stream.
  std::vector<WddmSubmitAllocation> wddm_submit_allocation_handles;
  // True if we failed to grow `wddm_submit_allocation_handles` due to OOM while
  // recording commands. Submitting with an incomplete allocation list is unsafe
  // for guest-backed resources because the KMD may not be able to resolve
  // `backing_alloc_id` references.
  bool wddm_submit_allocation_list_oom = false;

  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> last_completed_fence{0};

  // Staging resources written by commands recorded since the last submission.
  // After submission, their `last_gpu_write_fence` is updated to the returned
  // fence value.
  std::vector<Resource*> pending_staging_writes;

  // Cached state (shared for the initial immediate-context-only implementation).
  // Render targets (D3D11 OM). D3D11 supports up to 8 render-target slots.
  //
  // `current_rtv_count` tracks the number of slots bound (0..AEROGPU_MAX_RENDER_TARGETS).
  // Individual slots within the range may be null (handle==0), matching D3D11's
  // OMSetRenderTargets semantics.
  uint32_t current_rtv_count = 0;
  std::array<aerogpu_handle_t, AEROGPU_MAX_RENDER_TARGETS> current_rtvs{};
  std::array<Resource*, AEROGPU_MAX_RENDER_TARGETS> current_rtv_resources{};
  aerogpu_handle_t current_dsv = 0;
  Resource* current_dsv_resource = nullptr;
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_vs_srvs{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_ps_srvs{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_gs_srvs{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_cs_srvs{};
  std::array<Resource*, kMaxConstantBufferSlots> current_vs_cbs{};
  std::array<Resource*, kMaxConstantBufferSlots> current_ps_cbs{};
  std::array<Resource*, kMaxConstantBufferSlots> current_gs_cbs{};
  std::array<Resource*, kMaxConstantBufferSlots> current_cs_cbs{};
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_cs = 0;
  aerogpu_handle_t current_gs = 0;
  aerogpu_handle_t current_input_layout = 0;
  InputLayout* current_input_layout_obj = nullptr;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  aerogpu_constant_buffer_binding vs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding ps_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding gs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_constant_buffer_binding cs_constant_buffers[kMaxConstantBufferSlots] = {};
  aerogpu_handle_t vs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t ps_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t gs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t cs_srvs[kMaxShaderResourceSlots] = {};
  aerogpu_handle_t vs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t ps_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t current_gs_samplers[kMaxSamplerSlots] = {};
  aerogpu_handle_t cs_samplers[kMaxSamplerSlots] = {};

  // Buffer SRV bindings (structured/raw buffers).
  aerogpu_shader_resource_buffer_binding vs_srv_buffers[kMaxShaderResourceSlots] = {};
  aerogpu_shader_resource_buffer_binding ps_srv_buffers[kMaxShaderResourceSlots] = {};
  aerogpu_shader_resource_buffer_binding gs_srv_buffers[kMaxShaderResourceSlots] = {};
  aerogpu_shader_resource_buffer_binding cs_srv_buffers[kMaxShaderResourceSlots] = {};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_vs_srv_buffers{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_ps_srv_buffers{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_gs_srv_buffers{};
  std::array<Resource*, kAeroGpuD3D11MaxSrvSlots> current_cs_srv_buffers{};

  // Compute UAV buffer bindings.
  aerogpu_unordered_access_buffer_binding cs_uavs[kMaxUavSlots] = {};
  std::array<Resource*, kMaxUavSlots> current_cs_uavs{};

  // Minimal software-state tracking for the Win7 guest tests. This allows the
  // UMD to produce correct staging readback results even when the submission
  // backend is still a stub.
  //
  // Track all IA vertex buffer slots so WDDM submission + resource-destruction
  // cleanup can conservatively include/unbind any buffers referenced by draw
  // calls. Slot 0 is additionally mirrored into the `current_vb*` fields below
  // for the bring-up software rasterizer.
  std::array<Resource*, kD3D11IaVertexInputResourceSlotCount> current_vb_resources{};
  std::array<uint32_t, kD3D11IaVertexInputResourceSlotCount> current_vb_strides_bytes{};
  std::array<uint32_t, kD3D11IaVertexInputResourceSlotCount> current_vb_offsets_bytes{};
  Resource* current_vb = nullptr;
  uint32_t current_vb_stride_bytes = 0;
  uint32_t current_vb_offset_bytes = 0;
  Resource* current_ib = nullptr;
  uint32_t current_ib_format = kDxgiFormatUnknown;
  uint32_t current_ib_offset_bytes = 0;
  Resource* current_vs_cb0 = nullptr;
  uint32_t current_vs_cb0_first_constant = 0;
  uint32_t current_vs_cb0_num_constants = 0;
  Resource* current_ps_cb0 = nullptr;
  uint32_t current_ps_cb0_first_constant = 0;
  uint32_t current_ps_cb0_num_constants = 0;
  Resource* current_vs_srv0 = nullptr;
  Resource* current_ps_srv0 = nullptr;
  uint32_t current_vs_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_vs_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_ps_sampler0_address_u = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  uint32_t current_ps_sampler0_address_v = AEROGPU_SAMPLER_ADDRESS_CLAMP_TO_EDGE;
  DepthStencilState* current_dss = nullptr;
  uint32_t current_stencil_ref = 0;
  RasterizerState* current_rs = nullptr;
  BlendState* current_bs = nullptr;
  float current_blend_factor[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  uint32_t current_sample_mask = kD3DSampleMaskAll;

  bool scissor_valid = false;
  int32_t scissor_left = 0;
  int32_t scissor_top = 0;
  int32_t scissor_right = 0;
  int32_t scissor_bottom = 0;

  bool current_vs_forced_z_valid = false;
  float current_vs_forced_z = 0.0f;

  float viewport_x = 0.0f;
  float viewport_y = 0.0f;
  float viewport_width = 0.0f;
  float viewport_height = 0.0f;
  float viewport_min_depth = 0.0f;
  float viewport_max_depth = 1.0f;

  Device() {
    cmd.reset();
  }

  ~Device() {
    destroy_cookie = 0;
  }
};

// Updates the device's cached OM render target bindings (RTVs/DSV) from view
// objects. This is WDK-independent so it can be shared by both the WDK and
// repo-local ("portable") builds.
//
// Notes:
// - `num_rtvs` is clamped to AEROGPU_MAX_RENDER_TARGETS.
// - Slots within `[0, current_rtv_count)` may be null (handle==0), matching D3D11's
//   OMSetRenderTargets semantics (including "gaps").
// - Slots >= current_rtv_count are cleared to 0/nullptr.

inline void SetRenderTargetsStateLocked(Device* dev,
                                        uint32_t num_rtvs,
                                        const RenderTargetView* const* rtvs,
                                        const DepthStencilView* dsv) {
  if (!dev) {
    return;
  }

  const uint32_t count = std::min<uint32_t>(num_rtvs, AEROGPU_MAX_RENDER_TARGETS);
  // Accept the runtime-provided RTV slot count. Individual slots inside
  // `[0, count)` may be null, matching D3D11's OMSetRenderTargets semantics.
  dev->current_rtv_count = count;
  dev->current_rtvs.fill(0);
  dev->current_rtv_resources.fill(nullptr);

  for (uint32_t i = 0; i < count; ++i) {
    const RenderTargetView* view = (rtvs != nullptr) ? rtvs[i] : nullptr;
    Resource* res = view ? view->resource : nullptr;
    dev->current_rtv_resources[i] = res;
    // `view->texture` is a protocol view handle when non-zero. When it is 0,
    // this view is "trivial" (full-resource) and should bind the underlying
    // resource handle, which can change via RotateResourceIdentities.
    dev->current_rtvs[i] = view ? (view->texture ? view->texture : (res ? res->handle : 0)) : 0;
  }

  if (dsv) {
    dev->current_dsv_resource = dsv->resource;
    dev->current_dsv = dsv->texture ? dsv->texture : (dsv->resource ? dsv->resource->handle : 0);
  } else {
    dev->current_dsv = 0;
    dev->current_dsv_resource = nullptr;
  }
}

// Optional helper: normalize RTV bindings to a contiguous prefix.
//
// D3D11 allows "gaps" in the RTV array (a null RTV in slot 0 with a non-null RTV
// in slot 1, etc). Some bring-up backends may prefer to avoid gaps; callers can
// use this helper to truncate the RTV list at the first null slot and clear any
// subsequent slots.
//
// Note: `EmitSetRenderTargetsCmdFromStateLocked` does *not* call this helper;
// it encodes gaps as-is to preserve D3D11 semantics.
inline void NormalizeRenderTargetsNoGapsLocked(Device* dev) {
  if (!dev) {
    return;
  }

  const uint32_t count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  uint32_t new_count = 0;
  bool seen_gap = false;
  for (uint32_t i = 0; i < count; ++i) {
    const aerogpu_handle_t h = dev->current_rtvs[i];
    if (h == 0) {
      seen_gap = true;
      continue;
    }
    if (seen_gap) {
      dev->current_rtvs[i] = 0;
      dev->current_rtv_resources[i] = nullptr;
    } else {
      new_count = i + 1;
    }
  }
  for (uint32_t i = new_count; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    dev->current_rtvs[i] = 0;
    dev->current_rtv_resources[i] = nullptr;
  }
  dev->current_rtv_count = new_count;
}

// Emits an AEROGPU_CMD_SET_RENDER_TARGETS packet based on the device's current
// cached RTV/DSV state. Returns false if the command could not be appended.
inline bool EmitSetRenderTargetsCmdFromStateLocked(Device* dev) {
  if (!dev) {
    return false;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    return false;
  }

  const uint32_t count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  cmd->color_count = count;
  cmd->depth_stencil = dev->current_dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    cmd->colors[i] = (i < count) ? dev->current_rtvs[i] : 0;
  }
  return true;
}

// -------------------------------------------------------------------------------------------------
// Render target helpers (D3D10/D3D10.1 WDK UMD state tracking).
// -------------------------------------------------------------------------------------------------
//
// The WDK D3D10 and D3D10.1 translation units each define their own `AeroGpuDevice`
// struct with fields mirroring the D3D10 OM render target state:
//   - `current_rtv_count`
//   - `current_rtvs[]`
//   - `current_rtv_resources[]`
//   - `current_dsv`
//   - `current_dsv_res`
//
// Keep these helpers templated to avoid pulling WDK-specific types into this
// shared header (repo builds use a small ABI subset).
template <typename DeviceT>
inline void NormalizeRenderTargetsLocked(DeviceT* dev) {
  if (!dev) {
    return;
  }

  // Clamp RTV count to the protocol maximum and keep unused entries cleared.
  dev->current_rtv_count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  for (uint32_t i = dev->current_rtv_count; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    dev->current_rtvs[i] = 0;
    dev->current_rtv_resources[i] = nullptr;
  }

  // Keep the cached DSV handle consistent with the cached resource pointer. The
  // protocol binds a handle for the depth/stencil attachment; if the resource
  // pointer is null, ensure we do not accidentally re-emit a stale handle.
  if (!dev->current_dsv_res) {
    dev->current_dsv = 0;
  }
}

template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetRenderTargetsCmdLocked(DeviceT* dev,
                                         uint32_t rtv_count,
                                         const aerogpu_handle_t* rtvs,
                                         aerogpu_handle_t dsv,
                                         SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }

  const uint32_t count = std::min<uint32_t>(rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }

  cmd->color_count = count;
  cmd->depth_stencil = dsv;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    cmd->colors[i] = 0;
  }
  if (rtvs) {
    for (uint32_t i = 0; i < count; ++i) {
      cmd->colors[i] = rtvs[i];
    }
  }

  // Bring-up logging: helps confirm MRT bindings (color_count + colors[]) reach
  // the host intact.
  AEROGPU_D3D10_11_LOG("SET_RENDER_TARGETS: color_count=%u depth=%u colors=[%u,%u,%u,%u,%u,%u,%u,%u]",
                       static_cast<unsigned>(count),
                       static_cast<unsigned>(dsv),
                       static_cast<unsigned>(cmd->colors[0]),
                       static_cast<unsigned>(cmd->colors[1]),
                       static_cast<unsigned>(cmd->colors[2]),
                       static_cast<unsigned>(cmd->colors[3]),
                       static_cast<unsigned>(cmd->colors[4]),
                       static_cast<unsigned>(cmd->colors[5]),
                       static_cast<unsigned>(cmd->colors[6]),
                       static_cast<unsigned>(cmd->colors[7]));
  return true;
}

template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetRenderTargetsLocked(DeviceT* dev, SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  NormalizeRenderTargetsLocked(dev);
  return EmitSetRenderTargetsCmdLocked(dev,
                                       dev->current_rtv_count,
                                       dev->current_rtvs,
                                       dev->current_dsv,
                                       std::forward<SetErrorFn>(set_error));
}

template <typename DeviceT, typename ResourceT, typename SetErrorFn>
inline bool UnbindResourceFromOutputsLocked(DeviceT* dev,
                                            aerogpu_handle_t handle,
                                            const ResourceT* res,
                                            SetErrorFn&& set_error) {
  if (!dev || (handle == 0 && !res)) {
    return true;
  }

  const uint32_t count = std::min<uint32_t>(dev->current_rtv_count, AEROGPU_MAX_RENDER_TARGETS);
  aerogpu_handle_t rtvs[AEROGPU_MAX_RENDER_TARGETS] = {};
  using ResourcePtr = std::remove_reference_t<decltype(dev->current_rtv_resources[0])>;
  ResourcePtr rtv_resources[AEROGPU_MAX_RENDER_TARGETS] = {};
  for (uint32_t i = 0; i < count; ++i) {
    rtvs[i] = dev->current_rtvs[i];
    rtv_resources[i] = dev->current_rtv_resources[i];
  }
  aerogpu_handle_t dsv = dev->current_dsv;
  ResourcePtr dsv_res = dev->current_dsv_res;
  if (!dsv_res) {
    dsv = 0;
  }

  bool changed = false;
  for (uint32_t i = 0; i < count; ++i) {
    if ((handle != 0 && rtvs[i] == handle) ||
        (res && ResourcesAlias(rtv_resources[i], res))) {
      rtvs[i] = 0;
      rtv_resources[i] = nullptr;
      changed = true;
    }
  }
  if ((handle != 0 && dsv == handle) ||
      (res && ResourcesAlias(dsv_res, res))) {
    dsv = 0;
    dsv_res = nullptr;
    changed = true;
  }

  if (!changed) {
    return true;
  }

  if (!EmitSetRenderTargetsCmdLocked(dev, count, rtvs, dsv, std::forward<SetErrorFn>(set_error))) {
    return false;
  }

  // Commit state only after successfully appending the command.
  dev->current_rtv_count = count;
  for (uint32_t i = 0; i < count; ++i) {
    dev->current_rtvs[i] = rtvs[i];
    dev->current_rtv_resources[i] = rtv_resources[i];
  }
  for (uint32_t i = count; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    dev->current_rtvs[i] = 0;
    dev->current_rtv_resources[i] = nullptr;
  }
  dev->current_dsv = dsv;
  dev->current_dsv_res = dsv_res;
  return true;
}

// -------------------------------------------------------------------------------------------------
// Dynamic state helpers (viewport + scissor)
// -------------------------------------------------------------------------------------------------
//
// The AeroGPU command stream currently supports only a single viewport and a single scissor rect.
// D3D11 supports arrays of viewports/scissors; the Win7 runtime will pass those arrays down to the
// UMD. To avoid silent misrendering when applications use multiple viewports or scissors, we
// validate that any additional entries are either identical to the first entry or effectively
// disabled/unused, and report E_NOTIMPL otherwise.
//
// These helpers are WDK-free so they can be exercised by host-side unit tests without requiring
// d3d11umddi.h. The caller is expected to hold `dev->mutex`.

template <typename ViewportT>
inline bool viewport_is_default_or_disabled(const ViewportT& vp) {
  // Treat viewports with non-positive dimensions (or NaNs) as disabled. This matches the host-side
  // command executor's behavior, where width/height <= 0 results in leaving the render pass's
  // default viewport in place.
  return !(vp.Width > 0.0f && vp.Height > 0.0f);
}

template <typename ViewportT>
inline bool viewport_equal(const ViewportT& a, const ViewportT& b) {
  return a.TopLeftX == b.TopLeftX &&
         a.TopLeftY == b.TopLeftY &&
         a.Width == b.Width &&
         a.Height == b.Height &&
         a.MinDepth == b.MinDepth &&
         a.MaxDepth == b.MaxDepth;
}

template <typename RectT>
inline bool scissor_is_default_or_disabled(const RectT& r) {
  const int64_t w = static_cast<int64_t>(r.right) - static_cast<int64_t>(r.left);
  const int64_t h = static_cast<int64_t>(r.bottom) - static_cast<int64_t>(r.top);
  return w <= 0 || h <= 0;
}

template <typename RectT>
inline bool scissor_equal(const RectT& a, const RectT& b) {
  return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

inline int32_t clamp_i64_to_i32(int64_t v) {
  if (v > static_cast<int64_t>(std::numeric_limits<int32_t>::max())) {
    return std::numeric_limits<int32_t>::max();
  }
  if (v < static_cast<int64_t>(std::numeric_limits<int32_t>::min())) {
    return std::numeric_limits<int32_t>::min();
  }
  return static_cast<int32_t>(v);
}

template <typename DeviceT, typename ViewportT, typename SetErrorFn>
inline void validate_and_emit_viewports_locked(DeviceT* dev,
                                               uint32_t num_viewports,
                                               const ViewportT* viewports,
                                               SetErrorFn&& set_error) {
  if (!dev) {
    return;
  }

  // D3D11: NumViewports==0 disables viewports (runtime clear-state path). Encode this as a
  // zero-area viewport so the host runtime falls back to its default full-target viewport.
  if (num_viewports == 0) {
    auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
    if (!cmd) {
      set_error(E_OUTOFMEMORY);
      return;
    }
    cmd->x_f32 = f32_bits(0.0f);
    cmd->y_f32 = f32_bits(0.0f);
    cmd->width_f32 = f32_bits(0.0f);
    cmd->height_f32 = f32_bits(0.0f);
    cmd->min_depth_f32 = f32_bits(0.0f);
    cmd->max_depth_f32 = f32_bits(1.0f);

    if constexpr (has_member_viewport_x<DeviceT>::value) {
      dev->viewport_x = 0.0f;
    }
    if constexpr (has_member_viewport_y<DeviceT>::value) {
      dev->viewport_y = 0.0f;
    }
    if constexpr (has_member_viewport_width<DeviceT>::value) {
      dev->viewport_width = 0;
    }
    if constexpr (has_member_viewport_height<DeviceT>::value) {
      dev->viewport_height = 0;
    }
    if constexpr (has_member_viewport_min_depth<DeviceT>::value) {
      dev->viewport_min_depth = 0.0f;
    }
    if constexpr (has_member_viewport_max_depth<DeviceT>::value) {
      dev->viewport_max_depth = 1.0f;
    }
    return;
  }

  if (!viewports) {
    set_error(E_INVALIDARG);
    return;
  }

  const ViewportT& vp0 = viewports[0];
  bool unsupported = false;
  if (num_viewports > 1) {
    for (uint32_t i = 1; i < num_viewports; i++) {
      const ViewportT& vpi = viewports[i];
      if (viewport_equal(vpi, vp0) || viewport_is_default_or_disabled(vpi)) {
        continue;
      }
      unsupported = true;
      break;
    }
  }

  // Protocol supports only one viewport. We'll still apply slot 0 as a
  // best-effort fallback and report E_NOTIMPL after successfully encoding it.

  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return;
  }
  cmd->x_f32 = f32_bits(vp0.TopLeftX);
  cmd->y_f32 = f32_bits(vp0.TopLeftY);
  cmd->width_f32 = f32_bits(vp0.Width);
  cmd->height_f32 = f32_bits(vp0.Height);
  cmd->min_depth_f32 = f32_bits(vp0.MinDepth);
  cmd->max_depth_f32 = f32_bits(vp0.MaxDepth);

  if constexpr (has_member_viewport_x<DeviceT>::value) {
    dev->viewport_x = vp0.TopLeftX;
    if constexpr (has_member_viewport_y<DeviceT>::value) {
      dev->viewport_y = vp0.TopLeftY;
    }
    if constexpr (has_member_viewport_width<DeviceT>::value) {
      dev->viewport_width = vp0.Width;
    }
    if constexpr (has_member_viewport_height<DeviceT>::value) {
      dev->viewport_height = vp0.Height;
    }
    if constexpr (has_member_viewport_min_depth<DeviceT>::value) {
      dev->viewport_min_depth = vp0.MinDepth;
    }
    if constexpr (has_member_viewport_max_depth<DeviceT>::value) {
      dev->viewport_max_depth = vp0.MaxDepth;
    }
  } else {
    // D3D10/D3D10.1 WDK UMDs track only integer viewport width/height for the
    // bring-up software rasterizer. Preserve their behavior of only updating the
    // cached dimensions when the viewport is actually enabled.
    if (vp0.Width > 0.0f && vp0.Height > 0.0f) {
      if constexpr (has_member_viewport_width<DeviceT>::value) {
        using WidthT = std::remove_reference_t<decltype(dev->viewport_width)>;
        dev->viewport_width = static_cast<WidthT>(vp0.Width);
      }
      if constexpr (has_member_viewport_height<DeviceT>::value) {
        using HeightT = std::remove_reference_t<decltype(dev->viewport_height)>;
        dev->viewport_height = static_cast<HeightT>(vp0.Height);
      }
    }
  }

  if (unsupported) {
    set_error(E_NOTIMPL);
  }
}

template <typename DeviceT, typename RectT, typename SetErrorFn>
inline void validate_and_emit_scissor_rects_locked(DeviceT* dev,
                                                   uint32_t num_rects,
                                                   const RectT* rects,
                                                   SetErrorFn&& set_error) {
  if (!dev) {
    return;
  }

  // D3D11: NumRects==0 disables scissor rects. Encode this as a 0x0 rect; the host command executor
  // treats width/height <= 0 as "scissor disabled".
  if (num_rects == 0) {
    auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
    if (!cmd) {
      set_error(E_OUTOFMEMORY);
      return;
    }
    cmd->x = 0;
    cmd->y = 0;
    cmd->width = 0;
    cmd->height = 0;

    if constexpr (has_member_scissor_valid<DeviceT>::value) {
      dev->scissor_valid = false;
    }
    if constexpr (has_member_scissor_left<DeviceT>::value) {
      dev->scissor_left = 0;
    }
    if constexpr (has_member_scissor_top<DeviceT>::value) {
      dev->scissor_top = 0;
    }
    if constexpr (has_member_scissor_right<DeviceT>::value) {
      dev->scissor_right = 0;
    }
    if constexpr (has_member_scissor_bottom<DeviceT>::value) {
      dev->scissor_bottom = 0;
    }
    return;
  }

  if (!rects) {
    set_error(E_INVALIDARG);
    return;
  }

  const RectT& r0 = rects[0];
  bool unsupported = false;
  if (num_rects > 1) {
    for (uint32_t i = 1; i < num_rects; i++) {
      const RectT& ri = rects[i];
      if (scissor_equal(ri, r0) || scissor_is_default_or_disabled(ri)) {
        continue;
      }
      unsupported = true;
      break;
    }
  }

  // Protocol supports only one scissor rect. We'll still apply slot 0 as a
  // best-effort fallback and report E_NOTIMPL after successfully encoding it.

  const int32_t w = clamp_i64_to_i32(static_cast<int64_t>(r0.right) - static_cast<int64_t>(r0.left));
  const int32_t h = clamp_i64_to_i32(static_cast<int64_t>(r0.bottom) - static_cast<int64_t>(r0.top));
  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_scissor>(AEROGPU_CMD_SET_SCISSOR);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return;
  }
  cmd->x = r0.left;
  cmd->y = r0.top;
  cmd->width = w;
  cmd->height = h;

  if constexpr (has_member_scissor_valid<DeviceT>::value) {
    dev->scissor_valid = (w > 0 && h > 0);
  }
  if constexpr (has_member_scissor_left<DeviceT>::value) {
    dev->scissor_left = r0.left;
  }
  if constexpr (has_member_scissor_top<DeviceT>::value) {
    dev->scissor_top = r0.top;
  }
  if constexpr (has_member_scissor_right<DeviceT>::value) {
    dev->scissor_right = r0.right;
  }
  if constexpr (has_member_scissor_bottom<DeviceT>::value) {
    dev->scissor_bottom = r0.bottom;
  }

  if (unsupported) {
    set_error(E_NOTIMPL);
  }
}

// -------------------------------------------------------------------------------------------------
// Input assembler helpers (primitive topology)
// -------------------------------------------------------------------------------------------------
//
// The protocol's `aerogpu_primitive_topology` values intentionally match the
// D3D10/D3D11 runtime numeric values, so UMDs can forward them directly.
//
// The caller is expected to hold `dev->mutex`.
template <typename DeviceT, typename SetErrorFn>
inline bool SetPrimitiveTopologyLocked(DeviceT* dev, uint32_t topology, SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  if (dev->current_topology == topology) {
    return true;
  }

  auto* cmd =
      dev->cmd.template append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }

  cmd->topology = topology;
  cmd->reserved0 = 0;
  dev->current_topology = topology;
  return true;
}

struct SetPrimitiveTopologyNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool SetPrimitiveTopologyLocked(DeviceT* dev, uint32_t topology) {
  return SetPrimitiveTopologyLocked(dev, topology, SetPrimitiveTopologyNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Input assembler helpers (SET_VERTEX_BUFFERS)
// -------------------------------------------------------------------------------------------------
//
// The caller is expected to hold `dev->mutex`.
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetVertexBuffersCmdLocked(DeviceT* dev,
                                          uint32_t start_slot,
                                          uint32_t buffer_count,
                                          const aerogpu_vertex_buffer_binding* bindings,
                                          SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  if (buffer_count != 0 && !bindings) {
    set_error(E_INVALIDARG);
    return false;
  }

  auto* cmd = dev->cmd.template append_with_payload<aerogpu_cmd_set_vertex_buffers>(
      AEROGPU_CMD_SET_VERTEX_BUFFERS, bindings, static_cast<size_t>(buffer_count) * sizeof(bindings[0]));
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->start_slot = start_slot;
  cmd->buffer_count = buffer_count;
  return true;
}

struct EmitSetVertexBuffersNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetVertexBuffersCmdLocked(DeviceT* dev,
                                          uint32_t start_slot,
                                          uint32_t buffer_count,
                                          const aerogpu_vertex_buffer_binding* bindings) {
  return EmitSetVertexBuffersCmdLocked(dev, start_slot, buffer_count, bindings, EmitSetVertexBuffersNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Input assembler helpers (SET_INPUT_LAYOUT)
// -------------------------------------------------------------------------------------------------
//
// The caller is expected to hold `dev->mutex`.
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetInputLayoutCmdLocked(DeviceT* dev, aerogpu_handle_t input_layout_handle, SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->input_layout_handle = input_layout_handle;
  cmd->reserved0 = 0;
  return true;
}

struct EmitSetInputLayoutNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetInputLayoutCmdLocked(DeviceT* dev, aerogpu_handle_t input_layout_handle) {
  return EmitSetInputLayoutCmdLocked(dev, input_layout_handle, EmitSetInputLayoutNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Input assembler helpers (SET_INDEX_BUFFER)
// -------------------------------------------------------------------------------------------------
//
// The caller is expected to hold `dev->mutex`.
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetIndexBufferCmdLocked(DeviceT* dev,
                                        aerogpu_handle_t buffer,
                                        uint32_t format,
                                        uint32_t offset_bytes,
                                        SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->buffer = buffer;
  cmd->format = format;
  cmd->offset_bytes = offset_bytes;
  cmd->reserved0 = 0;
  return true;
}

struct EmitSetIndexBufferNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetIndexBufferCmdLocked(DeviceT* dev, aerogpu_handle_t buffer, uint32_t format, uint32_t offset_bytes) {
  return EmitSetIndexBufferCmdLocked(dev, buffer, format, offset_bytes, EmitSetIndexBufferNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Resource binding helpers (SET_TEXTURE)
// -------------------------------------------------------------------------------------------------
//
// Emits an AEROGPU_CMD_SET_TEXTURE packet. This is shared across D3D10/D3D10.1/D3D11
// codepaths; higher-level helpers are responsible for managing per-stage binding
// tables and resource hazard mitigation.
//
// The caller is expected to hold `dev->mutex`.
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetTextureCmdLocked(DeviceT* dev,
                                    uint32_t shader_stage,
                                    uint32_t slot,
                                    aerogpu_handle_t texture,
                                    SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  auto* cmd = dev->cmd.template append_fixed<aerogpu_cmd_set_texture>(AEROGPU_CMD_SET_TEXTURE);
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->slot = slot;
  cmd->texture = texture;
  cmd->reserved0 = 0;
  return true;
}

struct EmitSetTextureNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetTextureCmdLocked(DeviceT* dev, uint32_t shader_stage, uint32_t slot, aerogpu_handle_t texture) {
  return EmitSetTextureCmdLocked(dev, shader_stage, slot, texture, EmitSetTextureNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Resource binding helpers (SET_SAMPLERS)
// -------------------------------------------------------------------------------------------------
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetSamplersCmdLocked(DeviceT* dev,
                                     uint32_t shader_stage,
                                     uint32_t start_slot,
                                     uint32_t sampler_count,
                                     const aerogpu_handle_t* samplers,
                                     SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  if (sampler_count != 0 && !samplers) {
    set_error(E_INVALIDARG);
    return false;
  }

  auto* cmd = dev->cmd.template append_with_payload<aerogpu_cmd_set_samplers>(
      AEROGPU_CMD_SET_SAMPLERS, samplers, static_cast<size_t>(sampler_count) * sizeof(samplers[0]));
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->sampler_count = sampler_count;
  cmd->reserved0 = 0;
  return true;
}

struct EmitSetSamplersNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetSamplersCmdLocked(DeviceT* dev,
                                     uint32_t shader_stage,
                                     uint32_t start_slot,
                                     uint32_t sampler_count,
                                     const aerogpu_handle_t* samplers) {
  return EmitSetSamplersCmdLocked(dev, shader_stage, start_slot, sampler_count, samplers, EmitSetSamplersNoopSetError{});
}

// -------------------------------------------------------------------------------------------------
// Resource binding helpers (SET_CONSTANT_BUFFERS)
// -------------------------------------------------------------------------------------------------
template <typename DeviceT, typename SetErrorFn>
inline bool EmitSetConstantBuffersCmdLocked(DeviceT* dev,
                                            uint32_t shader_stage,
                                            uint32_t start_slot,
                                            uint32_t buffer_count,
                                            const aerogpu_constant_buffer_binding* buffers,
                                            SetErrorFn&& set_error) {
  if (!dev) {
    return false;
  }
  if (buffer_count != 0 && !buffers) {
    set_error(E_INVALIDARG);
    return false;
  }

  auto* cmd = dev->cmd.template append_with_payload<aerogpu_cmd_set_constant_buffers>(
      AEROGPU_CMD_SET_CONSTANT_BUFFERS, buffers, static_cast<size_t>(buffer_count) * sizeof(buffers[0]));
  if (!cmd) {
    set_error(E_OUTOFMEMORY);
    return false;
  }
  cmd->shader_stage = shader_stage;
  cmd->start_slot = start_slot;
  cmd->buffer_count = buffer_count;
  cmd->reserved0 = 0;
  return true;
}

struct EmitSetConstantBuffersNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT>
inline bool EmitSetConstantBuffersCmdLocked(DeviceT* dev,
                                            uint32_t shader_stage,
                                            uint32_t start_slot,
                                            uint32_t buffer_count,
                                            const aerogpu_constant_buffer_binding* buffers) {
  return EmitSetConstantBuffersCmdLocked(dev,
                                         shader_stage,
                                         start_slot,
                                         buffer_count,
                                         buffers,
                                         EmitSetConstantBuffersNoopSetError{});
}

template <typename THandle, typename TObject>
inline TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

template <typename T>
inline std::uintptr_t D3dHandleToUintPtr(T value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<std::uintptr_t>(value);
  } else {
    return static_cast<std::uintptr_t>(value);
  }
}

template <typename T>
inline T UintPtrToD3dHandle(std::uintptr_t value) {
  if constexpr (std::is_pointer_v<T>) {
    return reinterpret_cast<T>(value);
  } else {
    return static_cast<T>(value);
  }
}

// Converts D3D10/11 fill-mode numeric values to `aerogpu_fill_mode` values used
// by the AeroGPU protocol.
//
// D3D10/D3D11 values are 2=WIREFRAME, 3=SOLID.
inline uint32_t D3DFillModeToAerogpu(uint32_t fill_mode) {
  switch (fill_mode) {
    case 2: // D3D10_FILL_WIREFRAME / D3D11_FILL_WIREFRAME
      return AEROGPU_FILL_WIREFRAME;
    case 3: // D3D10_FILL_SOLID / D3D11_FILL_SOLID
    default:
      return AEROGPU_FILL_SOLID;
  }
}

// Converts D3D10/11 cull-mode numeric values to `aerogpu_cull_mode` values used
// by the AeroGPU protocol.
//
// D3D10/D3D11 values are 1=NONE, 2=FRONT, 3=BACK.
inline uint32_t D3DCullModeToAerogpu(uint32_t cull_mode) {
  switch (cull_mode) {
    case 1: // D3D10_CULL_NONE / D3D11_CULL_NONE
      return AEROGPU_CULL_NONE;
    case 2: // D3D10_CULL_FRONT / D3D11_CULL_FRONT
      return AEROGPU_CULL_FRONT;
    case 3: // D3D10_CULL_BACK / D3D11_CULL_BACK
    default:
      return AEROGPU_CULL_BACK;
  }
}

// Converts D3D11_COMPARISON_FUNC numeric values (as stored in the D3D11 DDI) to
// `aerogpu_compare_func` values used by the AeroGPU protocol.
//
// D3D11 values are 1..8 (NEVER..ALWAYS). The AeroGPU protocol uses 0..7.
inline uint32_t D3D11CompareFuncToAerogpu(uint32_t func) {
  switch (func) {
    case 1: // D3D11_COMPARISON_NEVER
      return AEROGPU_COMPARE_NEVER;
    case 2: // D3D11_COMPARISON_LESS
      return AEROGPU_COMPARE_LESS;
    case 3: // D3D11_COMPARISON_EQUAL
      return AEROGPU_COMPARE_EQUAL;
    case 4: // D3D11_COMPARISON_LESS_EQUAL
      return AEROGPU_COMPARE_LESS_EQUAL;
    case 5: // D3D11_COMPARISON_GREATER
      return AEROGPU_COMPARE_GREATER;
    case 6: // D3D11_COMPARISON_NOT_EQUAL
      return AEROGPU_COMPARE_NOT_EQUAL;
    case 7: // D3D11_COMPARISON_GREATER_EQUAL
      return AEROGPU_COMPARE_GREATER_EQUAL;
    case 8: // D3D11_COMPARISON_ALWAYS
      return AEROGPU_COMPARE_ALWAYS;
    default:
      break;
  }
  return AEROGPU_COMPARE_ALWAYS;
}

// D3D10 and D3D11 share the same numeric encoding for comparison functions, so
// D3D10 paths can reuse the D3D11 mapping.
inline uint32_t D3DCompareFuncToAerogpu(uint32_t func) {
  return D3D11CompareFuncToAerogpu(func);
}

// Emits `AEROGPU_CMD_SET_DEPTH_STENCIL_STATE` using state tracked in `dss`.
//
// Returns false if command stream emission failed (e.g. OOM).
inline bool EmitDepthStencilStateCmdLocked(Device* dev, const DepthStencilState* dss) {
  if (!dev) {
    return false;
  }

  // Defaults matching the D3D11 default depth-stencil state.
  uint32_t depth_enable = 1u;
  uint32_t depth_write_mask = 1u; // D3D11_DEPTH_WRITE_MASK_ALL
  uint32_t depth_func = 2u; // D3D11_COMPARISON_LESS
  uint32_t stencil_enable = 0u;
  uint8_t stencil_read_mask = kD3DStencilMaskAll;
  uint8_t stencil_write_mask = kD3DStencilMaskAll;
  if (dss) {
    depth_enable = dss->depth_enable;
    depth_write_mask = dss->depth_write_mask;
    depth_func = dss->depth_func;
    stencil_enable = dss->stencil_enable;
    stencil_read_mask = dss->stencil_read_mask;
    stencil_write_mask = dss->stencil_write_mask;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_depth_stencil_state>(AEROGPU_CMD_SET_DEPTH_STENCIL_STATE);
  if (!cmd) {
    return false;
  }

  cmd->state.depth_enable = depth_enable ? 1u : 0u;
  // D3D11 semantics: DepthWriteMask is ignored when depth testing is disabled.
  cmd->state.depth_write_enable = (depth_enable && depth_write_mask) ? 1u : 0u;
  cmd->state.depth_func = D3D11CompareFuncToAerogpu(depth_func);
  cmd->state.stencil_enable = stencil_enable ? 1u : 0u;
  cmd->state.stencil_read_mask = stencil_read_mask;
  cmd->state.stencil_write_mask = stencil_write_mask;
  cmd->state.reserved0[0] = 0;
  cmd->state.reserved0[1] = 0;
  return true;
}

struct TrackStagingWriteNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

// Forward declaration so `TrackStagingWriteLocked` can force a submission when it
// cannot grow the `pending_staging_writes` vector (OOM).
uint64_t submit_locked(Device* dev, bool want_present, HRESULT* out_hr);

template <typename DeviceT, typename ResourceT, typename SetErrorFn>
inline void TrackStagingWriteLocked(DeviceT* dev, ResourceT* dst, SetErrorFn&& set_error) {
  if (!dev || !dst) {
    return;
  }

  // Track writes into staging readback resources so Map(READ)/Map(DO_NOT_WAIT)
  // can wait on the fence that actually produces the bytes, instead of waiting
  // on the device's latest fence (which can include unrelated work).
  //
  // Prefer the captured Usage field when available, but keep the legacy
  // bind-flags heuristic as a fallback in case an older ABI doesn't expose it.
  if (dst->usage != 0) {
    if (dst->usage != kD3D11UsageStaging) {
      return;
    }
  } else {
    if (dst->bind_flags != 0) {
      return;
    }
  }

  // Prefer to only track CPU-readable staging resources, but fall back to
  // tracking all bindless resources if CPU access flags were not captured.
  if (dst->cpu_access_flags != 0 && (dst->cpu_access_flags & kD3D11CpuAccessRead) == 0) {
    return;
  }

  auto& tracked = dev->pending_staging_writes;
  if (std::find(tracked.begin(), tracked.end(), dst) != tracked.end()) {
    return;
  }

  try {
    tracked.push_back(dst);
  } catch (...) {
    // If we cannot record the staging write due to OOM, fall back to an
    // immediate submission so we can still stamp the staging fence without
    // needing to grow `pending_staging_writes`.
    //
    // This avoids Map(READ) observing stale `last_gpu_write_fence==0` and
    // returning data before the GPU/host has written back into the staging
    // allocation.
    if constexpr (std::is_same_v<std::remove_cv_t<DeviceT>, Device> &&
                  std::is_same_v<std::remove_cv_t<ResourceT>, Resource>) {
      HRESULT submit_hr = S_OK;
      const uint64_t fence = submit_locked(static_cast<Device*>(dev), /*want_present=*/false, &submit_hr);
      if (FAILED(submit_hr)) {
        set_error(submit_hr);
        return;
      }
      if (fence != 0) {
        dst->last_gpu_write_fence = fence;
      }
      return;
    }

    set_error(E_OUTOFMEMORY);
  }
}

template <typename DeviceT, typename ResourceT>
inline void TrackStagingWriteLocked(DeviceT* dev, ResourceT* dst) {
  TrackStagingWriteLocked(dev, dst, TrackStagingWriteNoopSetError{});
}

struct TrackWddmAllocNoopSetError {
  void operator()(HRESULT) const noexcept {}
};

template <typename DeviceT, typename ResourceT, typename SetErrorFn>
inline void TrackWddmAllocForSubmitLocked(DeviceT* dev, const ResourceT* res, bool write, SetErrorFn&& set_error) {
  if (!dev || !res) {
    return;
  }
  if (dev->wddm_submit_allocation_list_oom) {
    return;
  }
  if (res->backing_alloc_id == 0 || res->wddm_allocation_handle == 0) {
    return;
  }

  const uint32_t handle = res->wddm_allocation_handle;
  for (auto& entry : dev->wddm_submit_allocation_handles) {
    if (entry.allocation_handle == handle) {
      if (write) {
        entry.write = 1;
      }
      return;
    }
  }

  WddmSubmitAllocation entry{};
  entry.allocation_handle = handle;
  entry.write = write ? 1 : 0;
  try {
    dev->wddm_submit_allocation_handles.push_back(entry);
  } catch (...) {
    dev->wddm_submit_allocation_list_oom = true;
    set_error(E_OUTOFMEMORY);
  }
}

template <typename DeviceT, typename ResourceT>
inline void TrackWddmAllocForSubmitLocked(DeviceT* dev, const ResourceT* res, bool write) {
  TrackWddmAllocForSubmitLocked(dev, res, write, TrackWddmAllocNoopSetError{});
}

inline void atomic_max_u64(std::atomic<uint64_t>* target, uint64_t value) {
  if (!target) {
    return;
  }

  uint64_t cur = target->load(std::memory_order_relaxed);
  while (cur < value && !target->compare_exchange_weak(cur, value, std::memory_order_relaxed)) {
  }
}

inline uint64_t submit_locked(Device* dev, bool want_present = false, HRESULT* out_hr = nullptr) {
  if (out_hr) {
    *out_hr = S_OK;
  }
  if (!dev) {
    return 0;
  }
  if (dev->wddm_submit_allocation_list_oom) {
    if (out_hr) {
      *out_hr = E_OUTOFMEMORY;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }
  if (dev->cmd.empty()) {
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }

  dev->cmd.finalize();

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  const size_t submit_bytes = dev->cmd.size();
  uint64_t fence = 0;
  const WddmSubmitAllocation* allocs =
      dev->wddm_submit_allocation_handles.empty() ? nullptr : dev->wddm_submit_allocation_handles.data();
  const uint32_t alloc_count = static_cast<uint32_t>(dev->wddm_submit_allocation_handles.size());
  const HRESULT hr =
      dev->wddm_submit.SubmitAeroCmdStream(dev->cmd.data(),
                                           dev->cmd.size(),
                                           want_present,
                                           allocs,
                                           alloc_count,
                                           &fence);
  if (out_hr) {
    *out_hr = hr;
  }
  dev->cmd.reset();
  dev->wddm_submit_allocation_handles.clear();
  dev->wddm_submit_allocation_list_oom = false;
  if (FAILED(hr)) {
    dev->pending_staging_writes.clear();
    return 0;
  }

  if (fence != 0) {
    atomic_max_u64(&dev->last_submitted_fence, fence);
    for (Resource* res : dev->pending_staging_writes) {
      if (res) {
        res->last_gpu_write_fence = fence;
      }
    }
  }
  dev->pending_staging_writes.clear();

  const uint64_t completed = dev->wddm_submit.QueryCompletedFence();
  atomic_max_u64(&dev->last_completed_fence, completed);
  AEROGPU_D3D10_11_LOG("submit_locked: present=%u bytes=%llu fence=%llu completed=%llu",
                       want_present ? 1u : 0u,
                       static_cast<unsigned long long>(submit_bytes),
                       static_cast<unsigned long long>(fence),
                       static_cast<unsigned long long>(completed));
  return fence;
#else
  (void)want_present;
  Adapter* adapter = dev->adapter;
  if (!adapter) {
    if (out_hr) {
      *out_hr = E_FAIL;
    }
    dev->pending_staging_writes.clear();
    dev->cmd.reset();
    dev->wddm_submit_allocation_handles.clear();
    dev->wddm_submit_allocation_list_oom = false;
    return 0;
  }

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  dev->last_submitted_fence.store(fence, std::memory_order_relaxed);
  dev->last_completed_fence.store(fence, std::memory_order_relaxed);
  for (Resource* res : dev->pending_staging_writes) {
    if (res) {
      res->last_gpu_write_fence = fence;
    }
  }
  dev->pending_staging_writes.clear();
  dev->cmd.reset();
  dev->wddm_submit_allocation_handles.clear();
  dev->wddm_submit_allocation_list_oom = false;
  return fence;
#endif
}

inline HRESULT flush_locked(Device* dev) {
  HRESULT hr = S_OK;
  (void)submit_locked(dev, /*want_present=*/false, &hr);
  return hr;
}

} // namespace aerogpu::d3d10_11
