#include <cstdint>
#include <cstdio>
#include <cstring>

#include "aerogpu_d3d9_objects.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool CheckEq(aerogpu::FixedFuncVariant got, aerogpu::FixedFuncVariant expected, const char* msg) {
  if (got != expected) {
    std::fprintf(stderr,
                 "FAIL: %s (got=%u expected=%u)\n",
                 msg,
                 static_cast<unsigned>(got),
                 static_cast<unsigned>(expected));
    return false;
  }
  return true;
}

}  // namespace

int main() {
  using aerogpu::FixedFuncVariant;
  using aerogpu::fixedfunc_variant_from_decl_blob;
  using aerogpu::fixedfunc_variant_from_fvf;

  // FVF mapping.
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyzRhw | aerogpu::kD3dFvfDiffuse),
               FixedFuncVariant::RHW_COLOR,
               "FVF -> RHW_COLOR")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyzRhw | aerogpu::kD3dFvfDiffuse | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::RHW_COLOR_TEX1,
               "FVF -> RHW_COLOR_TEX1")) {
    return 1;
  }
  // Some runtimes leave garbage TEXCOORDSIZE bits set for *unused* texcoord sets
  // (e.g. TEXCOORD1 when TEXCOUNT=1). Fixed-function bring-up paths should ignore
  // those and key only off TEXCOORD0.
  if (!CheckEq(fixedfunc_variant_from_fvf((aerogpu::kD3dFvfXyzRhw | aerogpu::kD3dFvfDiffuse | aerogpu::kD3dFvfTex1) | 0x40000u),
               FixedFuncVariant::RHW_COLOR_TEX1,
               "FVF (+unused TEXCOORDSIZE bits) -> RHW_COLOR_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyzRhw | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::RHW_TEX1,
               "FVF -> RHW_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfDiffuse),
               FixedFuncVariant::XYZ_COLOR,
               "FVF -> XYZ_COLOR")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfDiffuse | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::XYZ_COLOR_TEX1,
               "FVF -> XYZ_COLOR_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::XYZ_TEX1,
               "FVF -> XYZ_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfNormal),
               FixedFuncVariant::XYZ_NORMAL,
               "FVF -> XYZ_NORMAL")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfNormal | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::XYZ_NORMAL_TEX1,
               "FVF -> XYZ_NORMAL_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfNormal | aerogpu::kD3dFvfDiffuse),
               FixedFuncVariant::XYZ_NORMAL_COLOR,
               "FVF -> XYZ_NORMAL_COLOR")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(aerogpu::kD3dFvfXyz | aerogpu::kD3dFvfNormal | aerogpu::kD3dFvfDiffuse | aerogpu::kD3dFvfTex1),
               FixedFuncVariant::XYZ_NORMAL_COLOR_TEX1,
               "FVF -> XYZ_NORMAL_COLOR_TEX1")) {
    return 1;
  }
  if (!CheckEq(fixedfunc_variant_from_fvf(0xFFFFFFFFu), FixedFuncVariant::NONE, "FVF -> NONE (unknown)")) {
    return 1;
  }

  // Decl-blob mapping (synthesized SetFVF -> SetVertexDecl path).
  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat4, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePositionT, 0},
        {0, 16, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::RHW_COLOR,
                 "decl -> RHW_COLOR")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat4, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePositionT, 0},
        {0, 16, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0, 20, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::RHW_COLOR_TEX1,
                 "decl -> RHW_COLOR_TEX1")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat4, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePositionT, 0},
        {0, 16, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::RHW_TEX1,
                 "decl -> RHW_TEX1")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_COLOR,
                 "decl -> XYZ_COLOR")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0, 16, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_COLOR_TEX1,
                 "decl -> XYZ_COLOR_TEX1")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_TEX1,
                 "decl -> XYZ_TEX1")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageNormal, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_NORMAL,
                 "decl -> XYZ_NORMAL")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageNormal, 0},
        {0, 24, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_NORMAL_TEX1,
                 "decl -> XYZ_NORMAL_TEX1")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageNormal, 0},
        {0, 24, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_NORMAL_COLOR,
                 "decl -> XYZ_NORMAL_COLOR")) {
      return 1;
    }
  }

  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 12, aerogpu::kD3dDeclTypeFloat3, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageNormal, 0},
        {0, 24, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0, 28, aerogpu::kD3dDeclTypeFloat2, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageTexCoord, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::XYZ_NORMAL_COLOR_TEX1,
                 "decl -> XYZ_NORMAL_COLOR_TEX1")) {
      return 1;
    }
  }

  // Allow POSITION usage as a synonym for POSITIONT in the first element (runtime variance).
  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat4, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePosition, 0},
        {0, 16, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        {0xFF, 0, aerogpu::kD3dDeclTypeUnused, 0, 0, 0},
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::RHW_COLOR,
                 "decl POSITION -> RHW_COLOR")) {
      return 1;
    }
  }

  if (!CheckEq(fixedfunc_variant_from_decl_blob(nullptr, 0), FixedFuncVariant::NONE, "decl nullptr -> NONE")) {
    return 1;
  }

  // Truncated declarations should not match.
  {
    const aerogpu::D3DVERTEXELEMENT9_COMPAT decl[] = {
        {0, 0, aerogpu::kD3dDeclTypeFloat4, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsagePositionT, 0},
        {0, 16, aerogpu::kD3dDeclTypeD3dColor, aerogpu::kD3dDeclMethodDefault, aerogpu::kD3dDeclUsageColor, 0},
        // Missing D3DDECL_END.
    };
    if (!CheckEq(fixedfunc_variant_from_decl_blob(decl, sizeof(decl)),
                 FixedFuncVariant::NONE,
                 "decl missing END -> NONE")) {
      return 1;
    }
  }

  return 0;
}
