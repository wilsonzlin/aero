#pragma once

#include <cstdint>

namespace aerogpu {

// Minimal built-in D3D9 SM2.0 shader token streams used as a fixed-function
// fallback for the Win7 `d3d9ex_triangle` test.
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

} // namespace fixedfunc
} // namespace aerogpu
