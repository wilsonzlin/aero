#pragma once

#include <cstdint>

namespace aerogpu {

// Fixed-function constant register layout used by the D3D9 UMD.
//
// Tests keep these values as local numeric constants (rather than including
// driver internals or Windows SDK/WDK headers) so host tests remain portable.

// WVP constant block uploaded by the fixed-function VS variants.
constexpr uint32_t kFixedfuncMatrixStartRegister = 240u;
constexpr uint32_t kFixedfuncMatrixVec4Count = 4u;

// Lighting/material constant block uploaded by the fixed-function *lit* VS variants.
constexpr uint32_t kFixedfuncLightingStartRegister = 208u;
constexpr uint32_t kFixedfuncLightingVec4Count = 29u;

// Global ambient constant lives at c236 (last register in the lighting block).
constexpr uint32_t kFixedfuncLightingGlobalAmbientRegister = 236u;
constexpr uint32_t kFixedfuncLightingGlobalAmbientRel =
    kFixedfuncLightingGlobalAmbientRegister - kFixedfuncLightingStartRegister;

} // namespace aerogpu

