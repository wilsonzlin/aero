// Shared D3D10/D3D10.1 blend-state validation + translation helpers.
//
// The AeroGPU protocol only supports a small subset of blend factors (primarily
// alpha blending + constant blend factors) and encodes a single global blend
// state (no per-render-target blend configuration).
//
// The Windows D3D10/D3D10.1 runtimes allow blend descriptors that cannot be
// represented. This helper returns `E_NOTIMPL` for those configurations.
//
// Policy note:
// - Callers that want strict correctness should propagate `E_NOTIMPL` out of
//   CreateBlendState so apps can detect missing support.
// - Some bring-up / MVP paths may choose to treat `E_NOTIMPL` as "use a
//   conservative default" (blend disabled) so apps can continue running, at the
//   cost of rendering differences.
//
// This header is shared by the WDK (real Win7) and portable (non-WDK) UMD builds
// so that unit tests can validate the mapping in a host environment.
//
// Numeric values for the D3D10/D3D10.1 blend enums are taken from the official
// Windows SDK headers (d3d10.h/d3d11.h). Keep these in sync if the protocol is
// extended.
#pragma once

#include <cstdint>

#include "../../../protocol/aerogpu_cmd.h"

namespace aerogpu::d3d10_11 {

// D3D10_BLEND / D3D11_BLEND subset (numeric values from d3d10.h/d3d11.h).
constexpr uint32_t kD3dBlendZero = 1;
constexpr uint32_t kD3dBlendOne = 2;
constexpr uint32_t kD3dBlendSrcColor = 3;
constexpr uint32_t kD3dBlendInvSrcColor = 4;
constexpr uint32_t kD3dBlendSrcAlpha = 5;
constexpr uint32_t kD3dBlendInvSrcAlpha = 6;
constexpr uint32_t kD3dBlendDestAlpha = 7;
constexpr uint32_t kD3dBlendInvDestAlpha = 8;
constexpr uint32_t kD3dBlendDestColor = 9;
constexpr uint32_t kD3dBlendInvDestColor = 10;
constexpr uint32_t kD3dBlendSrcAlphaSat = 11;
// 12/13 are reserved/unused in the SDK headers.
constexpr uint32_t kD3dBlendBlendFactor = 14;
constexpr uint32_t kD3dBlendInvBlendFactor = 15;
// D3D10.1 additions.
constexpr uint32_t kD3dBlendSrc1Color = 16;
constexpr uint32_t kD3dBlendInvSrc1Color = 17;
constexpr uint32_t kD3dBlendSrc1Alpha = 18;
constexpr uint32_t kD3dBlendInvSrc1Alpha = 19;

// D3D10_BLEND_OP / D3D11_BLEND_OP subset (numeric values from d3d10.h/d3d11.h).
constexpr uint32_t kD3dBlendOpAdd = 1;
constexpr uint32_t kD3dBlendOpSubtract = 2;
constexpr uint32_t kD3dBlendOpRevSubtract = 3;
constexpr uint32_t kD3dBlendOpMin = 4;
constexpr uint32_t kD3dBlendOpMax = 5;

struct D3dRtBlendDesc {
  bool blend_enable = false;
  uint8_t write_mask = 0xFu;
  uint32_t src_blend = kD3dBlendOne;
  uint32_t dest_blend = kD3dBlendZero;
  uint32_t blend_op = kD3dBlendOpAdd;
  uint32_t src_blend_alpha = kD3dBlendOne;
  uint32_t dest_blend_alpha = kD3dBlendZero;
  uint32_t blend_op_alpha = kD3dBlendOpAdd;
};

struct AerogpuBlendStateBase {
  uint32_t enable = 0;
  uint32_t src_factor = AEROGPU_BLEND_ONE;
  uint32_t dst_factor = AEROGPU_BLEND_ZERO;
  uint32_t blend_op = AEROGPU_BLEND_OP_ADD;
  uint32_t src_factor_alpha = AEROGPU_BLEND_ONE;
  uint32_t dst_factor_alpha = AEROGPU_BLEND_ZERO;
  uint32_t blend_op_alpha = AEROGPU_BLEND_OP_ADD;
  uint8_t color_write_mask = 0xFu;
};

inline bool D3dBlendFactorToAerogpu(uint32_t factor, uint32_t* out_factor) {
  if (!out_factor) {
    return false;
  }
  switch (factor) {
    case kD3dBlendZero:
      *out_factor = AEROGPU_BLEND_ZERO;
      return true;
    case kD3dBlendOne:
      *out_factor = AEROGPU_BLEND_ONE;
      return true;
    case kD3dBlendSrcAlpha:
      *out_factor = AEROGPU_BLEND_SRC_ALPHA;
      return true;
    case kD3dBlendInvSrcAlpha:
      *out_factor = AEROGPU_BLEND_INV_SRC_ALPHA;
      return true;
    case kD3dBlendDestAlpha:
      *out_factor = AEROGPU_BLEND_DEST_ALPHA;
      return true;
    case kD3dBlendInvDestAlpha:
      *out_factor = AEROGPU_BLEND_INV_DEST_ALPHA;
      return true;
    case kD3dBlendBlendFactor:
      *out_factor = AEROGPU_BLEND_CONSTANT;
      return true;
    case kD3dBlendInvBlendFactor:
      *out_factor = AEROGPU_BLEND_INV_CONSTANT;
      return true;
    default:
      return false;
  }
}

inline uint32_t D3dBlendFactorToAerogpuOr(uint32_t factor, uint32_t fallback) {
  uint32_t out = fallback;
  (void)D3dBlendFactorToAerogpu(factor, &out);
  return out;
}

inline bool D3dBlendOpToAerogpu(uint32_t blend_op, uint32_t* out_op) {
  if (!out_op) {
    return false;
  }
  switch (blend_op) {
    case kD3dBlendOpAdd:
      *out_op = AEROGPU_BLEND_OP_ADD;
      return true;
    case kD3dBlendOpSubtract:
      *out_op = AEROGPU_BLEND_OP_SUBTRACT;
      return true;
    case kD3dBlendOpRevSubtract:
      *out_op = AEROGPU_BLEND_OP_REV_SUBTRACT;
      return true;
    case kD3dBlendOpMin:
      *out_op = AEROGPU_BLEND_OP_MIN;
      return true;
    case kD3dBlendOpMax:
      *out_op = AEROGPU_BLEND_OP_MAX;
      return true;
    default:
      return false;
  }
}

inline uint32_t D3dBlendOpToAerogpuOr(uint32_t blend_op, uint32_t fallback) {
  uint32_t out = fallback;
  (void)D3dBlendOpToAerogpu(blend_op, &out);
  return out;
}

inline bool D3dRtBlendDescMatchesRt0(const D3dRtBlendDesc& rt, const D3dRtBlendDesc& rt0) {
  if (rt.blend_enable != rt0.blend_enable) {
    return false;
  }
  if (rt.write_mask != rt0.write_mask) {
    return false;
  }
  // Blend factors/ops only matter when blending is enabled.
  if (!rt0.blend_enable) {
    return true;
  }
  return rt.src_blend == rt0.src_blend && rt.dest_blend == rt0.dest_blend && rt.blend_op == rt0.blend_op &&
         rt.src_blend_alpha == rt0.src_blend_alpha && rt.dest_blend_alpha == rt0.dest_blend_alpha &&
         rt.blend_op_alpha == rt0.blend_op_alpha;
}

inline HRESULT ValidateAndConvertBlendDesc(const D3dRtBlendDesc* rts,
                                          uint32_t rt_count,
                                          bool alpha_to_coverage_enable,
                                          AerogpuBlendStateBase* out_state) {
  if (!rts || !out_state || rt_count == 0) {
    return E_INVALIDARG;
  }

  // Alpha-to-coverage is not representable in the protocol.
  if (alpha_to_coverage_enable) {
    return E_NOTIMPL;
  }

  const D3dRtBlendDesc& rt0 = rts[0];

  // The protocol only supports a single global blend state. If D3D supplies
  // per-render-target states, reject unless all targets match RT0.
  for (uint32_t i = 1; i < rt_count; ++i) {
    if (!D3dRtBlendDescMatchesRt0(rts[i], rt0)) {
      return E_NOTIMPL;
    }
  }

  // Write mask is only 4 bits in the protocol.
  if ((rt0.write_mask & ~0xFu) != 0) {
    return E_NOTIMPL;
  }

  AerogpuBlendStateBase s{};
  s.enable = rt0.blend_enable ? 1u : 0u;
  s.color_write_mask = static_cast<uint8_t>(rt0.write_mask & 0xFu);

  if (rt0.blend_enable) {
    if (!D3dBlendFactorToAerogpu(rt0.src_blend, &s.src_factor) ||
        !D3dBlendFactorToAerogpu(rt0.dest_blend, &s.dst_factor) ||
        !D3dBlendOpToAerogpu(rt0.blend_op, &s.blend_op) ||
        !D3dBlendFactorToAerogpu(rt0.src_blend_alpha, &s.src_factor_alpha) ||
        !D3dBlendFactorToAerogpu(rt0.dest_blend_alpha, &s.dst_factor_alpha) ||
        !D3dBlendOpToAerogpu(rt0.blend_op_alpha, &s.blend_op_alpha)) {
      return E_NOTIMPL;
    }
  }

  *out_state = s;
  return S_OK;
}

} // namespace aerogpu::d3d10_11
