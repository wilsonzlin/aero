#pragma once

#include <cstdint>

namespace aerogpu {

// Minimal built-in D3D9 SM2.0 shader token streams used by the AeroGPU D3D9 UMD
// fixed-function fallback path (bring-up FVF subset: `XYZRHW|DIFFUSE` and
// `XYZRHW|DIFFUSE|TEX1`).
//
// These are intentionally tiny and avoid declarations so they can be consumed by
// early bring-up shader translators (mov/add/mul subset).
namespace fixedfunc {

// vs_2_0:
//   mov oPos, v0
//   mov oD0, v1       ; D3DCOLOR is BGRA in memory but is presented to shaders as RGBA
//   end
static constexpr uint32_t kVsPassthroughPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x02000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   mov oC0, v0
//   end
static constexpr uint32_t kPsPassthroughColor[] = {
    0xFFFF0200u, // ps_2_0
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// vs_2_0:
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v2
//   end
static constexpr uint32_t kVsPassthroughPosColorTex1[] = {
    0xFFFE0200u, // vs_2_0
    0x02000001u, // mov (2 operands)
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x02000001u, // mov (2 operands)
    0x600F0000u, // oT0.xyzw
    0x10E40002u, // v2.xyzw
    0x0000FFFFu, // end
};

// ps_2_0:
//   texld r0, t0, s0
//   mul r0, r0, v0
//   mov oC0, r0
//   end
static constexpr uint32_t kPsTexturedModulateVertexColor[] = {
    0xFFFF0200u, // ps_2_0
    0x03000042u, // texld (3 operands)
    0x000F0000u, // r0.xyzw
    0x30E40000u, // t0.xyzw
    0x20E40800u, // s0
    0x03000005u, // mul (3 operands)
    0x000F0000u, // r0.xyzw
    0x00E40000u, // r0.xyzw
    0x10E40000u, // v0.xyzw
    0x02000001u, // mov (2 operands)
    0x000F0800u, // oC0.xyzw
    0x00E40000u, // r0.xyzw
    0x0000FFFFu, // end
};

} // namespace fixedfunc
} // namespace aerogpu
