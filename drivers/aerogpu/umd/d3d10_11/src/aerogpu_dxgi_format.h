// AeroGPU D3D10/11 UMD - shared DXGI format helpers.
//
// This header is intentionally WDK-free so it can be compiled by the repository
// "portable" build and host-side unit tests.
#pragma once

#include <cstdint>
#include <type_traits>

#include "../../../protocol/aerogpu_cmd.h"
#include "../../../protocol/aerogpu_umd_private.h"

namespace aerogpu::d3d10_11 {

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
//
// We intentionally define the numeric values here instead of relying on WDK/SDK
// headers so older header sets (or the portable build) can still compile while
// keeping a single source of truth.
constexpr uint32_t kDxgiFormatUnknown = 0;
constexpr uint32_t kDxgiFormatR32G32B32A32Float = 2;
constexpr uint32_t kDxgiFormatR32G32B32Float = 6;
constexpr uint32_t kDxgiFormatR32G32Float = 16;
constexpr uint32_t kDxgiFormatR8G8B8A8Typeless = 27;
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatR8G8B8A8UnormSrgb = 29;
constexpr uint32_t kDxgiFormatR32Typeless = 39;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatR32Float = 41;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatR32Sint = 43;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatBc1Typeless = 70;
constexpr uint32_t kDxgiFormatBc1Unorm = 71;
constexpr uint32_t kDxgiFormatBc1UnormSrgb = 72;
constexpr uint32_t kDxgiFormatBc2Typeless = 73;
constexpr uint32_t kDxgiFormatBc2Unorm = 74;
constexpr uint32_t kDxgiFormatBc2UnormSrgb = 75;
constexpr uint32_t kDxgiFormatBc3Typeless = 76;
constexpr uint32_t kDxgiFormatBc3Unorm = 77;
constexpr uint32_t kDxgiFormatBc3UnormSrgb = 78;
constexpr uint32_t kDxgiFormatB5G6R5Unorm = 85;
constexpr uint32_t kDxgiFormatB5G5R5A1Unorm = 86;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;
constexpr uint32_t kDxgiFormatB8G8R8A8Typeless = 90;
constexpr uint32_t kDxgiFormatB8G8R8A8UnormSrgb = 91;
constexpr uint32_t kDxgiFormatB8G8R8X8Typeless = 92;
constexpr uint32_t kDxgiFormatB8G8R8X8UnormSrgb = 93;
constexpr uint32_t kDxgiFormatBc7Typeless = 97;
constexpr uint32_t kDxgiFormatBc7Unorm = 98;
constexpr uint32_t kDxgiFormatBc7UnormSrgb = 99;

inline uint32_t DxgiFormatToAerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB5G6R5Unorm:
      return AEROGPU_FORMAT_B5G6R5_UNORM;
    case kDxgiFormatB5G5R5A1Unorm:
      return AEROGPU_FORMAT_B5G5R5A1_UNORM;
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8Typeless:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case kDxgiFormatB8G8R8A8UnormSrgb:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB;
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8Typeless:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    case kDxgiFormatB8G8R8X8UnormSrgb:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB;
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8Typeless:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    case kDxgiFormatR8G8B8A8UnormSrgb:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB;
    case kDxgiFormatBc1Typeless:
    case kDxgiFormatBc1Unorm:
      return AEROGPU_FORMAT_BC1_RGBA_UNORM;
    case kDxgiFormatBc1UnormSrgb:
      return AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB;
    case kDxgiFormatBc2Typeless:
    case kDxgiFormatBc2Unorm:
      return AEROGPU_FORMAT_BC2_RGBA_UNORM;
    case kDxgiFormatBc2UnormSrgb:
      return AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB;
    case kDxgiFormatBc3Typeless:
    case kDxgiFormatBc3Unorm:
      return AEROGPU_FORMAT_BC3_RGBA_UNORM;
    case kDxgiFormatBc3UnormSrgb:
      return AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB;
    case kDxgiFormatBc7Typeless:
    case kDxgiFormatBc7Unorm:
      return AEROGPU_FORMAT_BC7_RGBA_UNORM;
    case kDxgiFormatBc7UnormSrgb:
      return AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB;
    case kDxgiFormatD24UnormS8Uint:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    case kDxgiFormatD32Float:
      return AEROGPU_FORMAT_D32_FLOAT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

// Backwards-compatible alias (existing call sites).
inline uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
  return DxgiFormatToAerogpu(dxgi_format);
}

namespace detail {

// Some DDIs ask format/cap questions on an adapter, while others ask through a
// device/context that holds an `adapter` pointer. Keep the feature-gating helpers
// generic so D3D10/D3D10.1/D3D11 UMDs can share the same logic without having to
// keep per-UMD copies in sync.
template <typename T, typename = void>
struct HasAdapterMember : std::false_type {};

template <typename T>
struct HasAdapterMember<T, std::void_t<decltype(&T::adapter)>> : std::true_type {};

template <typename T>
inline auto GetCapsAdapter(const T* dev_or_adapter) {
  if constexpr (HasAdapterMember<T>::value) {
    return dev_or_adapter ? dev_or_adapter->adapter : nullptr;
  } else {
    return dev_or_adapter;
  }
}

inline bool AbiMajorMinorAtLeast(const aerogpu_umd_private_v1& blob, uint32_t want_major, uint32_t want_minor) {
  const uint32_t major = blob.device_abi_version_u32 >> 16;
  const uint32_t minor = blob.device_abi_version_u32 & 0xFFFFu;
  return (major == want_major) && (minor >= want_minor);
}

} // namespace detail

template <typename T>
inline bool SupportsTransfer(const T* dev_or_adapter) {
  const auto* adapter = detail::GetCapsAdapter(dev_or_adapter);
  if (!adapter || !adapter->umd_private_valid) {
    return false;
  }
  const aerogpu_umd_private_v1& blob = adapter->umd_private;
  if ((blob.device_features & AEROGPU_UMDPRIV_FEATURE_TRANSFER) == 0) {
    return false;
  }
  return detail::AbiMajorMinorAtLeast(blob, AEROGPU_ABI_MAJOR, /*want_minor=*/1);
}

template <typename T>
inline bool SupportsSrgbFormats(const T* dev_or_adapter) {
  // ABI 1.2 adds explicit sRGB format variants. When running against an older
  // host/device ABI, map sRGB DXGI formats to UNORM equivalents so the command
  // stream stays compatible.
  const auto* adapter = detail::GetCapsAdapter(dev_or_adapter);
  if (!adapter || !adapter->umd_private_valid) {
    return false;
  }
  return detail::AbiMajorMinorAtLeast(adapter->umd_private, AEROGPU_ABI_MAJOR, /*want_minor=*/2);
}

template <typename T>
inline bool SupportsBcFormats(const T* dev_or_adapter) {
  const auto* adapter = detail::GetCapsAdapter(dev_or_adapter);
  if (!adapter || !adapter->umd_private_valid) {
    return false;
  }
  return detail::AbiMajorMinorAtLeast(adapter->umd_private, AEROGPU_ABI_MAJOR, /*want_minor=*/2);
}

template <typename T>
inline uint32_t DxgiFormatToCompatDxgiFormat(const T* dev_or_adapter, uint32_t dxgi_format) {
  if (!SupportsSrgbFormats(dev_or_adapter)) {
    switch (dxgi_format) {
      case kDxgiFormatB8G8R8A8UnormSrgb:
        return kDxgiFormatB8G8R8A8Unorm;
      case kDxgiFormatB8G8R8X8UnormSrgb:
        return kDxgiFormatB8G8R8X8Unorm;
      case kDxgiFormatR8G8B8A8UnormSrgb:
        return kDxgiFormatR8G8B8A8Unorm;
      default:
        break;
    }
  }
  return dxgi_format;
}

template <typename T>
inline uint32_t DxgiFormatToAerogpuCompat(const T* dev_or_adapter, uint32_t dxgi_format) {
  return DxgiFormatToAerogpu(DxgiFormatToCompatDxgiFormat(dev_or_adapter, dxgi_format));
}

// Backwards-compatible alias (existing call sites).
template <typename T>
inline uint32_t dxgi_format_to_aerogpu_compat(const T* dev_or_adapter, uint32_t dxgi_format) {
  return DxgiFormatToAerogpuCompat(dev_or_adapter, dxgi_format);
}

enum class AerogpuFormatUsage : uint32_t {
  Texture2D = 1,
  RenderTarget = 2,
  DepthStencil = 3,
  ShaderSample = 4,
  Display = 5,
  Blendable = 6,
  CpuLockable = 7,
  Buffer = 8,
  IaVertexBuffer = 9,
  IaIndexBuffer = 10,
};

enum AerogpuDxgiFormatCaps : uint32_t {
  kAerogpuDxgiFormatCapNone = 0,
  kAerogpuDxgiFormatCapTexture2D = 1u << 0,
  kAerogpuDxgiFormatCapRenderTarget = 1u << 1,
  kAerogpuDxgiFormatCapDepthStencil = 1u << 2,
  kAerogpuDxgiFormatCapShaderSample = 1u << 3,
  kAerogpuDxgiFormatCapDisplay = 1u << 4,
  kAerogpuDxgiFormatCapBlendable = 1u << 5,
  kAerogpuDxgiFormatCapCpuLockable = 1u << 6,
  kAerogpuDxgiFormatCapBuffer = 1u << 7,
  kAerogpuDxgiFormatCapIaVertexBuffer = 1u << 8,
  kAerogpuDxgiFormatCapIaIndexBuffer = 1u << 9,
};

template <typename T>
inline uint32_t AerogpuDxgiFormatCapsMask(const T* dev_or_adapter, uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB5G6R5Unorm:
    case kDxgiFormatB5G5R5A1Unorm:
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8Typeless:
    case kDxgiFormatB8G8R8X8Unorm:
    case kDxgiFormatB8G8R8X8Typeless:
    case kDxgiFormatR8G8B8A8Unorm:
    case kDxgiFormatR8G8B8A8Typeless:
      return kAerogpuDxgiFormatCapTexture2D |
             kAerogpuDxgiFormatCapRenderTarget |
             kAerogpuDxgiFormatCapShaderSample |
             kAerogpuDxgiFormatCapDisplay |
             kAerogpuDxgiFormatCapBlendable |
             kAerogpuDxgiFormatCapCpuLockable;
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8X8UnormSrgb:
    case kDxgiFormatR8G8B8A8UnormSrgb:
      if (!SupportsSrgbFormats(dev_or_adapter)) {
        return kAerogpuDxgiFormatCapNone;
      }
      return kAerogpuDxgiFormatCapTexture2D |
             kAerogpuDxgiFormatCapRenderTarget |
             kAerogpuDxgiFormatCapShaderSample |
             kAerogpuDxgiFormatCapDisplay |
             kAerogpuDxgiFormatCapBlendable |
             kAerogpuDxgiFormatCapCpuLockable;
    case kDxgiFormatBc1Typeless:
    case kDxgiFormatBc1Unorm:
    case kDxgiFormatBc1UnormSrgb:
    case kDxgiFormatBc2Typeless:
    case kDxgiFormatBc2Unorm:
    case kDxgiFormatBc2UnormSrgb:
    case kDxgiFormatBc3Typeless:
    case kDxgiFormatBc3Unorm:
    case kDxgiFormatBc3UnormSrgb:
    case kDxgiFormatBc7Typeless:
    case kDxgiFormatBc7Unorm:
    case kDxgiFormatBc7UnormSrgb:
      if (!SupportsBcFormats(dev_or_adapter)) {
        return kAerogpuDxgiFormatCapNone;
      }
      return kAerogpuDxgiFormatCapTexture2D |
             kAerogpuDxgiFormatCapShaderSample |
             kAerogpuDxgiFormatCapCpuLockable;
    case kDxgiFormatD24UnormS8Uint:
    case kDxgiFormatD32Float:
      return kAerogpuDxgiFormatCapTexture2D | kAerogpuDxgiFormatCapDepthStencil;
    case kDxgiFormatR16Uint:
    case kDxgiFormatR32Uint:
      return kAerogpuDxgiFormatCapBuffer | kAerogpuDxgiFormatCapIaIndexBuffer;
    case kDxgiFormatR32Typeless:
    case kDxgiFormatR32Float:
    case kDxgiFormatR32Sint:
      return kAerogpuDxgiFormatCapBuffer;
    case kDxgiFormatR32G32Float:
    case kDxgiFormatR32G32B32Float:
    case kDxgiFormatR32G32B32A32Float:
      return kAerogpuDxgiFormatCapBuffer | kAerogpuDxgiFormatCapIaVertexBuffer;
    default:
      return kAerogpuDxgiFormatCapNone;
  }
}

template <typename T>
inline bool AerogpuSupportsDxgiFormat(const T* dev_or_adapter, uint32_t dxgi_format, AerogpuFormatUsage usage) {
  const uint32_t caps = AerogpuDxgiFormatCapsMask(dev_or_adapter, dxgi_format);
  switch (usage) {
    case AerogpuFormatUsage::Texture2D:
      return (caps & kAerogpuDxgiFormatCapTexture2D) != 0;
    case AerogpuFormatUsage::RenderTarget:
      return (caps & kAerogpuDxgiFormatCapRenderTarget) != 0;
    case AerogpuFormatUsage::DepthStencil:
      return (caps & kAerogpuDxgiFormatCapDepthStencil) != 0;
    case AerogpuFormatUsage::ShaderSample:
      return (caps & kAerogpuDxgiFormatCapShaderSample) != 0;
    case AerogpuFormatUsage::Display:
      return (caps & kAerogpuDxgiFormatCapDisplay) != 0;
    case AerogpuFormatUsage::Blendable:
      return (caps & kAerogpuDxgiFormatCapBlendable) != 0;
    case AerogpuFormatUsage::CpuLockable:
      return (caps & kAerogpuDxgiFormatCapCpuLockable) != 0;
    case AerogpuFormatUsage::Buffer:
      return (caps & kAerogpuDxgiFormatCapBuffer) != 0;
    case AerogpuFormatUsage::IaVertexBuffer:
      return (caps & kAerogpuDxgiFormatCapIaVertexBuffer) != 0;
    case AerogpuFormatUsage::IaIndexBuffer:
      return (caps & kAerogpuDxgiFormatCapIaIndexBuffer) != 0;
    default:
      return false;
  }
}

// Convenience wrapper for "compat" checks used by command-stream emission paths:
// apply the sRGB→UNORM compatibility mapping first, then evaluate format support.
template <typename T>
inline bool AerogpuSupportsDxgiFormatCompat(const T* dev_or_adapter, uint32_t dxgi_format, AerogpuFormatUsage usage) {
  const uint32_t compat = DxgiFormatToCompatDxgiFormat(dev_or_adapter, dxgi_format);
  return AerogpuSupportsDxgiFormat(dev_or_adapter, compat, usage);
}

template <typename T>
inline bool AerogpuSupportsMultisampleQualityLevels(const T* dev_or_adapter, uint32_t dxgi_format) {
  const uint32_t caps = AerogpuDxgiFormatCapsMask(dev_or_adapter, dxgi_format);
  return (caps & kAerogpuDxgiFormatCapTexture2D) != 0 &&
         (caps & (kAerogpuDxgiFormatCapRenderTarget | kAerogpuDxgiFormatCapDepthStencil)) != 0;
}

} // namespace aerogpu::d3d10_11
