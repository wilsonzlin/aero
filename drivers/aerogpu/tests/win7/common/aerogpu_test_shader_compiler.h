#pragma once

#include "aerogpu_test_common.h"

#include <d3dcompiler.h>

// Runtime HLSL compilation helper.
//
// Historically the Win7 suite used fxc.exe (DirectX SDK June 2010) at build-time
// to produce *.cso files. For modern toolchains/automation we compile at runtime
// via D3DCompile loaded dynamically from d3dcompiler_47.dll (or older variants).
//
// This keeps the suite buildable without the legacy DXSDK, at the cost of
// requiring a shader compiler DLL at runtime (place d3dcompiler_47.dll next to
// the test binaries if the guest doesn't already have it).

namespace aerogpu_test {

typedef HRESULT(WINAPI* D3DCompileProc)(LPCVOID pSrcData,
                                       SIZE_T SrcDataSize,
                                       LPCSTR pSourceName,
                                       const D3D_SHADER_MACRO* pDefines,
                                       ID3DInclude* pInclude,
                                       LPCSTR pEntryPoint,
                                       LPCSTR pTarget,
                                       UINT Flags1,
                                       UINT Flags2,
                                       ID3DBlob** ppCode,
                                       ID3DBlob** ppErrorMsgs);

static inline D3DCompileProc GetD3DCompile(std::string* err) {
  static D3DCompileProc proc = NULL;
  static bool attempted = false;
  static std::string cached_err;
  if (attempted) {
    if (!proc && err) {
      *err = cached_err;
    }
    return proc;
  }
  attempted = true;

  const wchar_t* dlls[] = {
      L"d3dcompiler_47.dll",
      L"d3dcompiler_46.dll",
      L"d3dcompiler_45.dll",
      L"d3dcompiler_44.dll",
      L"d3dcompiler_43.dll",
  };

  HMODULE mod = NULL;
  for (size_t i = 0; i < ARRAYSIZE(dlls); ++i) {
    mod = LoadLibraryW(dlls[i]);
    if (mod) {
      break;
    }
  }
  if (!mod) {
    cached_err =
        "failed to load a D3D shader compiler DLL (d3dcompiler_47.dll not found). "
        "Install a Windows update that provides it (e.g. KB4019990) or copy "
        "d3dcompiler_47.dll next to the test binaries.";
    if (err) {
      *err = cached_err;
    }
    return NULL;
  }

  proc = (D3DCompileProc)GetProcAddress(mod, "D3DCompile");
  if (!proc) {
    cached_err = "GetProcAddress(D3DCompile) failed";
    if (err) {
      *err = cached_err;
    }
    return NULL;
  }
  return proc;
}

static inline bool CompileHlslToBytecode(const char* source,
                                         size_t source_len,
                                         const char* source_name,
                                         const char* entrypoint,
                                         const char* target,
                                         std::vector<unsigned char>* out,
                                         std::string* err) {
  if (!source || source_len == 0 || !entrypoint || !target || !out) {
    if (err) {
      *err = "CompileHlslToBytecode: invalid parameters";
    }
    return false;
  }

  std::string load_err;
  D3DCompileProc compile = GetD3DCompile(&load_err);
  if (!compile) {
    if (err) {
      *err = load_err;
    }
    return false;
  }

  ComPtr<ID3DBlob> code;
  ComPtr<ID3DBlob> errors;
  const UINT flags1 = D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3;
  HRESULT hr = compile(source,
                       source_len,
                       source_name ? source_name : "<memory>",
                       NULL,
                       NULL,
                       entrypoint,
                       target,
                       flags1,
                       0,
                       code.put(),
                       errors.put());
  if (FAILED(hr)) {
    std::string msg = HresultToString(hr);
    if (errors && errors->GetBufferPointer() && errors->GetBufferSize()) {
      msg += ": ";
      msg += std::string((const char*)errors->GetBufferPointer(),
                         (const char*)errors->GetBufferPointer() + errors->GetBufferSize());
    }
    if (err) {
      *err = msg;
    }
    return false;
  }

  if (!code || !code->GetBufferPointer() || code->GetBufferSize() == 0) {
    if (err) {
      *err = "D3DCompile returned an empty blob";
    }
    return false;
  }

  out->assign((const unsigned char*)code->GetBufferPointer(),
              (const unsigned char*)code->GetBufferPointer() + code->GetBufferSize());
  return true;
}

}  // namespace aerogpu_test

