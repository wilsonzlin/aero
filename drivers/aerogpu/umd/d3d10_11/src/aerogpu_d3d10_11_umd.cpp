// AeroGPU Windows 7 D3D10/11 UMD (minimal milestone implementation).
//
// This file intentionally focuses on the smallest working surface area required
// for D3D11 FL10_0 triangle-style samples. The DDI surface area is large; the
// code below provides:
//   - exported OpenAdapter10/OpenAdapter10_2/OpenAdapter11 entrypoints
//   - minimal adapter + device objects
//   - minimal resource/shader/input layout/RTV creation
//   - state binding + draw + present
//
// The command stream emitted by the device is defined in:
//   drivers/aerogpu/protocol/aerogpu_protocol.h

#include "../include/aerogpu_d3d10_11_umd.h"

#include <new>
#include <string.h>

#include <vector>

namespace {

constexpr uint32_t kInvalidAllocIndex = 0;
constexpr uint32_t kInvalidShaderId = 0;

// FNV-1a 32-bit hash for stable semantic name IDs.
uint32_t HashSemanticName(const char *s) {
  if (!s) {
    return 0;
  }
  uint32_t hash = 2166136261u;
  for (const unsigned char *p = reinterpret_cast<const unsigned char *>(s); *p; ++p) {
    hash ^= *p;
    hash *= 16777619u;
  }
  return hash;
}

struct AeroGpuCommandStream {
  std::vector<uint8_t> bytes;

  void Clear() { bytes.clear(); }

  void Append(const void *data, size_t size) {
    const auto *p = static_cast<const uint8_t *>(data);
    bytes.insert(bytes.end(), p, p + size);
  }

  template <typename Payload>
  void EmitSimple(uint32_t opcode, const Payload &payload) {
    AEROGPU_CMD_HEADER hdr = {};
    hdr.opcode = opcode;
    hdr.size_bytes = static_cast<uint32_t>(sizeof(AEROGPU_CMD_HEADER) + sizeof(Payload));
    Append(&hdr, sizeof(hdr));
    Append(&payload, sizeof(payload));
  }

  void EmitWithTrailingBytes(uint32_t opcode,
                             const void *payload,
                             size_t payload_size,
                             const void *trailing,
                             size_t trailing_size) {
    AEROGPU_CMD_HEADER hdr = {};
    hdr.opcode = opcode;
    hdr.size_bytes = static_cast<uint32_t>(sizeof(AEROGPU_CMD_HEADER) + payload_size + trailing_size);
    Append(&hdr, sizeof(hdr));
    Append(payload, payload_size);
    if (trailing_size) {
      Append(trailing, trailing_size);
    }
  }

  // Stub submission point. The expectation is that integration code will wire
  // this to the AeroGPU KMD submission path (e.g. D3DKMTSubmitCommand on Win7).
  HRESULT Submit() {
    // For now we treat submission as a successful no-op and clear the buffer.
    Clear();
    return S_OK;
  }
};

struct AeroGpuAdapter {
  uint32_t next_alloc_index = 1;
  uint32_t next_shader_id = 1;
};

struct AeroGpuResource {
  uint32_t alloc_index = kInvalidAllocIndex;
  uint32_t kind = 0; // AEROGPU_RESOURCE_KIND
  uint32_t dxgi_format = 0;
};

struct AeroGpuShader {
  uint32_t shader_id = kInvalidShaderId;
  uint32_t stage = 0; // AEROGPU_SHADER_STAGE
};

struct AeroGpuInputLayout {
  std::vector<AEROGPU_INPUT_ELEMENT> elements;
};

struct AeroGpuRenderTargetView {
  uint32_t alloc_index = kInvalidAllocIndex;
};

struct AeroGpuDevice {
  AeroGpuAdapter *adapter = nullptr;
  AeroGpuCommandStream cs;

  uint32_t current_rtv_alloc = kInvalidAllocIndex;

  uint32_t current_vb_alloc = kInvalidAllocIndex;
  uint32_t current_vb_stride = 0;
  uint32_t current_vb_offset = 0;

  uint32_t current_ib_alloc = kInvalidAllocIndex;
  uint32_t current_ib_format = 0;
  uint32_t current_ib_offset = 0;

  uint32_t current_vs_id = kInvalidShaderId;
  uint32_t current_ps_id = kInvalidShaderId;

  bool viewport_set = false;
  AEROGPU_DDI_VIEWPORT viewport = {};

  HRESULT FlushAndSubmitIfNeeded() { return cs.Submit(); }
};

template <typename THandle, typename TObject>
TObject *FromHandle(THandle h) {
  return reinterpret_cast<TObject *>(h.pDrvPrivate);
}

// -------------------------------------------------------------------------------------------------
// Device DDI (implemented as plain functions to ensure the correct calling convention)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERESOURCE *) {
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                        const AEROGPU_DDIARG_CREATERESOURCE *pDesc,
                                        D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *res = new (hResource.pDrvPrivate) AeroGpuResource();

  res->alloc_index = dev->adapter->next_alloc_index++;

  AEROGPU_CMD_CREATE_RESOURCE_PAYLOAD payload = {};
  payload.alloc_index = res->alloc_index;
  payload.bind_flags = pDesc->BindFlags;
  payload.misc_flags = pDesc->MiscFlags;

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    res->kind = AEROGPU_RESOURCE_KIND_BUFFER;
    payload.kind = AEROGPU_RESOURCE_KIND_BUFFER;
    payload.size_bytes = pDesc->ByteWidth;
    payload.stride_bytes = pDesc->StructureByteStride;
  } else if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    // Initial milestone only supports non-array, non-mipped textures.
    if (pDesc->MipLevels != 1 || pDesc->ArraySize != 1) {
      return E_NOTIMPL;
    }
    res->kind = AEROGPU_RESOURCE_KIND_TEX2D;
    res->dxgi_format = pDesc->Format;
    payload.kind = AEROGPU_RESOURCE_KIND_TEX2D;
    payload.width = pDesc->Width;
    payload.height = pDesc->Height;
    payload.mip_levels = pDesc->MipLevels;
    payload.array_size = pDesc->ArraySize;
    payload.dxgi_format = pDesc->Format;
  } else {
    return E_NOTIMPL;
  }

  dev->cs.EmitSimple(AEROGPU_CMD_CREATE_RESOURCE, payload);

  // Upload initial data if provided.
  if (pDesc->pInitialData && pDesc->InitialDataCount) {
    if (res->kind == AEROGPU_RESOURCE_KIND_BUFFER) {
      const auto &sd = pDesc->pInitialData[0];
      if (!sd.pSysMem || pDesc->InitialDataCount != 1) {
        return E_INVALIDARG;
      }

      AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD up = {};
      up.alloc_index = res->alloc_index;
      up.dst_offset_bytes = 0;
      up.data_size_bytes = pDesc->ByteWidth;
      dev->cs.EmitWithTrailingBytes(AEROGPU_CMD_UPLOAD_RESOURCE, &up, sizeof(up), sd.pSysMem, up.data_size_bytes);
    } else if (res->kind == AEROGPU_RESOURCE_KIND_TEX2D) {
      if (pDesc->InitialDataCount != 1) {
        return E_NOTIMPL;
      }
      const auto &sd = pDesc->pInitialData[0];
      if (!sd.pSysMem) {
        return E_INVALIDARG;
      }
      // For the initial milestone we treat texture upload as opaque bytes.
      // The host translator is expected to interpret bytes based on
      // width/height/format.
      const uint32_t data_size = sd.SysMemSlicePitch ? sd.SysMemSlicePitch : (sd.SysMemPitch * pDesc->Height);
      AEROGPU_CMD_UPLOAD_RESOURCE_PAYLOAD up = {};
      up.alloc_index = res->alloc_index;
      up.dst_offset_bytes = 0;
      up.data_size_bytes = data_size;
      dev->cs.EmitWithTrailingBytes(AEROGPU_CMD_UPLOAD_RESOURCE, &up, sizeof(up), sd.pSysMem, up.data_size_bytes);
    }
  }

  return S_OK;
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (res->alloc_index != kInvalidAllocIndex) {
    AEROGPU_CMD_DESTROY_RESOURCE_PAYLOAD p = {};
    p.alloc_index = res->alloc_index;
    dev->cs.EmitSimple(AEROGPU_CMD_DESTROY_RESOURCE, p);
  }
  res->~AeroGpuResource();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADER *) {
  return sizeof(AeroGpuShader);
}

static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const AEROGPU_DDIARG_CREATESHADER *pDesc,
                                  D3D10DDI_HSHADER hShader,
                                  uint32_t stage) {
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate || !pDesc->pCode || !pDesc->CodeSize) {
    return E_INVALIDARG;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->shader_id = dev->adapter->next_shader_id++;
  sh->stage = stage;

  AEROGPU_CMD_CREATE_SHADER_PAYLOAD payload = {};
  payload.shader_id = sh->shader_id;
  payload.stage = stage;
  payload.dxbc_size_bytes = pDesc->CodeSize;
  dev->cs.EmitWithTrailingBytes(AEROGPU_CMD_CREATE_SHADER, &payload, sizeof(payload), pDesc->pCode, pDesc->CodeSize);

  return S_OK;
}

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const AEROGPU_DDIARG_CREATESHADER *pDesc,
                                            D3D10DDI_HSHADER hShader) {
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_VS);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATESHADER *pDesc,
                                           D3D10DDI_HSHADER hShader) {
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_PS);
}

void AEROGPU_APIENTRY DestroyShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *sh = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader);
  if (sh->shader_id != kInvalidShaderId) {
    AEROGPU_CMD_DESTROY_SHADER_PAYLOAD p = {};
    p.shader_id = sh->shader_id;
    dev->cs.EmitSimple(AEROGPU_CMD_DESTROY_SHADER, p);
  }
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateInputLayoutSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEINPUTLAYOUT *) {
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateInputLayout(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATEINPUTLAYOUT *pDesc,
                                           D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate || (!pDesc->NumElements && pDesc->pElements)) {
    return E_INVALIDARG;
  }
  auto *layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->elements.reserve(pDesc->NumElements);
  for (uint32_t i = 0; i < pDesc->NumElements; ++i) {
    const auto &e = pDesc->pElements[i];
    AEROGPU_INPUT_ELEMENT out = {};
    out.semantic_name_hash = HashSemanticName(e.SemanticName);
    out.semantic_index = e.SemanticIndex;
    out.format_dxgi = e.Format;
    out.input_slot = e.InputSlot;
    out.aligned_byte_offset = e.AlignedByteOffset;
    out.input_slot_class = e.InputSlotClass;
    out.instance_data_step_rate = e.InstanceDataStepRate;
    layout->elements.push_back(out);
  }
  return S_OK;
}

void AEROGPU_APIENTRY DestroyInputLayout(D3D10DDI_HDEVICE, D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hLayout.pDrvPrivate) {
    return;
  }
  auto *layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  layout->~AeroGpuInputLayout();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERENDERTARGETVIEW *) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRTV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATERENDERTARGETVIEW *pDesc,
                                   D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto *res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto *rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->alloc_index = res->alloc_index;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRTV(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto *rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

void AEROGPU_APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hDevice.pDrvPrivate || !hRtv.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  dev->current_rtv_alloc = rtv->alloc_index;

  AEROGPU_CMD_SET_RENDER_TARGET_PAYLOAD p = {};
  p.rtv_alloc_index = rtv->alloc_index;
  dev->cs.EmitSimple(AEROGPU_CMD_SET_RENDER_TARGET, p);
}

void AEROGPU_APIENTRY ClearRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW, const float rgba[4]) {
  if (!hDevice.pDrvPrivate || !rgba) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  AEROGPU_CMD_CLEAR_RTV_PAYLOAD p = {};
  p.rgba[0] = rgba[0];
  p.rgba[1] = rgba[1];
  p.rgba[2] = rgba[2];
  p.rgba[3] = rgba[3];
  dev->cs.EmitSimple(AEROGPU_CMD_CLEAR_RTV, p);
}

void AEROGPU_APIENTRY SetInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate || !hLayout.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);

  AEROGPU_CMD_SET_INPUT_LAYOUT_PAYLOAD payload = {};
  payload.element_count = static_cast<uint32_t>(layout->elements.size());
  dev->cs.EmitWithTrailingBytes(AEROGPU_CMD_SET_INPUT_LAYOUT,
                                &payload,
                                sizeof(payload),
                                layout->elements.data(),
                                layout->elements.size() * sizeof(AEROGPU_INPUT_ELEMENT));
}

void AEROGPU_APIENTRY SetVertexBuffer(D3D10DDI_HDEVICE hDevice,
                                      D3D10DDI_HRESOURCE hBuffer,
                                      uint32_t stride,
                                      uint32_t offset) {
  if (!hDevice.pDrvPrivate || !hBuffer.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *buf = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer);
  dev->current_vb_alloc = buf->alloc_index;
  dev->current_vb_stride = stride;
  dev->current_vb_offset = offset;

  AEROGPU_CMD_SET_VERTEX_BUFFER_PAYLOAD p = {};
  p.alloc_index = buf->alloc_index;
  p.stride_bytes = stride;
  p.offset_bytes = offset;
  dev->cs.EmitSimple(AEROGPU_CMD_SET_VERTEX_BUFFER, p);
}

void AEROGPU_APIENTRY SetIndexBuffer(D3D10DDI_HDEVICE hDevice,
                                     D3D10DDI_HRESOURCE hBuffer,
                                     uint32_t format,
                                     uint32_t offset) {
  if (!hDevice.pDrvPrivate || !hBuffer.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *buf = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer);
  dev->current_ib_alloc = buf->alloc_index;
  dev->current_ib_format = format;
  dev->current_ib_offset = offset;

  AEROGPU_CMD_SET_INDEX_BUFFER_PAYLOAD p = {};
  p.alloc_index = buf->alloc_index;
  p.index_format_dxgi = format;
  p.offset_bytes = offset;
  dev->cs.EmitSimple(AEROGPU_CMD_SET_INDEX_BUFFER, p);
}

void AEROGPU_APIENTRY SetViewport(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDI_VIEWPORT *pVp) {
  if (!hDevice.pDrvPrivate || !pVp) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->viewport_set = true;
  dev->viewport = *pVp;

  AEROGPU_CMD_SET_VIEWPORT_PAYLOAD p = {};
  p.x = pVp->TopLeftX;
  p.y = pVp->TopLeftY;
  p.width = pVp->Width;
  p.height = pVp->Height;
  p.min_depth = pVp->MinDepth;
  p.max_depth = pVp->MaxDepth;
  dev->cs.EmitSimple(AEROGPU_CMD_SET_VIEWPORT, p);
}

void AEROGPU_APIENTRY SetDrawState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hVs, D3D10DDI_HSHADER hPs) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);

  uint32_t vs_id = kInvalidShaderId;
  uint32_t ps_id = kInvalidShaderId;
  if (hVs.pDrvPrivate) {
    vs_id = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hVs)->shader_id;
  }
  if (hPs.pDrvPrivate) {
    ps_id = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hPs)->shader_id;
  }

  dev->current_vs_id = vs_id;
  dev->current_ps_id = ps_id;

  AEROGPU_CMD_BIND_SHADERS_PAYLOAD p = {};
  p.vs_shader_id = vs_id;
  p.ps_shader_id = ps_id;
  dev->cs.EmitSimple(AEROGPU_CMD_BIND_SHADERS, p);
}

void AEROGPU_APIENTRY Draw(D3D10DDI_HDEVICE hDevice, uint32_t vertex_count, uint32_t start_vertex) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  AEROGPU_CMD_DRAW_PAYLOAD p = {};
  p.vertex_count = vertex_count;
  p.start_vertex_location = start_vertex;
  dev->cs.EmitSimple(AEROGPU_CMD_DRAW, p);
}

void AEROGPU_APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, uint32_t index_count, uint32_t start_index, int32_t base_vertex) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  AEROGPU_CMD_DRAW_INDEXED_PAYLOAD p = {};
  p.index_count = index_count;
  p.start_index_location = start_index;
  p.base_vertex_location = base_vertex;
  dev->cs.EmitSimple(AEROGPU_CMD_DRAW_INDEXED, p);
}

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDIARG_PRESENT *pPresent) {
  if (!hDevice.pDrvPrivate || !pPresent || !pPresent->hBackBuffer.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto *dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto *bb = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pPresent->hBackBuffer);

  const uint32_t sync = (pPresent->SyncInterval != 0) ? 1u : 0u;

  AEROGPU_CMD_PRESENT_PAYLOAD p = {};
  p.backbuffer_alloc_index = bb->alloc_index;
  p.sync_interval = sync;
  dev->cs.EmitSimple(AEROGPU_CMD_PRESENT, p);
  return dev->FlushAndSubmitIfNeeded();
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE *) {
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE *pCreateDevice) {
  if (!pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto *adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto *device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;

  // Fill the device function table.
  AEROGPU_D3D10_11_DEVICEFUNCS funcs = {};
  funcs.pfnDestroyDevice = &DestroyDevice;

  funcs.pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  funcs.pfnCreateResource = &CreateResource;
  funcs.pfnDestroyResource = &DestroyResource;

  funcs.pfnCalcPrivateShaderSize = &CalcPrivateShaderSize;
  funcs.pfnCreateVertexShader = &CreateVertexShader;
  funcs.pfnCreatePixelShader = &CreatePixelShader;
  funcs.pfnDestroyShader = &DestroyShader;

  funcs.pfnCalcPrivateInputLayoutSize = &CalcPrivateInputLayoutSize;
  funcs.pfnCreateInputLayout = &CreateInputLayout;
  funcs.pfnDestroyInputLayout = &DestroyInputLayout;

  funcs.pfnCalcPrivateRTVSize = &CalcPrivateRTVSize;
  funcs.pfnCreateRTV = &CreateRTV;
  funcs.pfnDestroyRTV = &DestroyRTV;

  funcs.pfnSetRenderTargets = &SetRenderTargets;
  funcs.pfnClearRTV = &ClearRTV;

  funcs.pfnSetInputLayout = &SetInputLayout;
  funcs.pfnSetVertexBuffer = &SetVertexBuffer;
  funcs.pfnSetIndexBuffer = &SetIndexBuffer;
  funcs.pfnSetViewport = &SetViewport;
  funcs.pfnSetDrawState = &SetDrawState;

  funcs.pfnDraw = &Draw;
  funcs.pfnDrawIndexed = &DrawIndexed;
  funcs.pfnPresent = &Present;

  *pCreateDevice->pDeviceFuncs = funcs;
  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  auto *adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Exported OpenAdapter entrypoints
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapterCommon(D3D10DDIARG_OPENADAPTER *pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  // Allocate adapter object.
  auto *adapter = new AeroGpuAdapter();
  pOpenData->hAdapter.pDrvPrivate = adapter;

  D3D10DDI_ADAPTERFUNCS funcs = {};
  funcs.pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
  funcs.pfnCreateDevice = &CreateDevice;
  funcs.pfnCloseAdapter = &CloseAdapter;

  *pOpenData->pAdapterFuncs = funcs;
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER *pOpenData) { return OpenAdapterCommon(pOpenData); }

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER *pOpenData) { return OpenAdapterCommon(pOpenData); }

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER *pOpenData) { return OpenAdapterCommon(pOpenData); }

} // extern "C"
