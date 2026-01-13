#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_cmd.h"

namespace {

using namespace aerogpu::d3d10_11;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool ValidateStream(const uint8_t* buf, size_t len) {
  if (!Check(buf != nullptr, "stream buffer must be non-null")) {
    return false;
  }
  if (!Check(len >= sizeof(aerogpu_cmd_stream_header), "stream must contain header")) {
    return false;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "stream magic")) {
    return false;
  }
  if (!Check(stream->abi_version == AEROGPU_ABI_VERSION_U32, "stream abi_version")) {
    return false;
  }
  if (!Check(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header), "stream size_bytes >= header")) {
    return false;
  }
  if (!Check(stream->size_bytes <= len, "stream size_bytes within submitted length")) {
    return false;
  }
  return true;
}

const aerogpu_cmd_hdr* FindLastOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return nullptr;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t stream_len = static_cast<size_t>(stream->size_bytes);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const aerogpu_cmd_hdr* last = nullptr;
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      last = hdr;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return last;
}

#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
struct Harness {
  std::vector<uint8_t> last_stream;
  std::vector<HRESULT> errors;

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                  const void* cmd_stream,
                                                  uint32_t cmd_stream_size_bytes,
                                                  const AEROGPU_WDDM_SUBMIT_ALLOCATION*,
                                                  uint32_t,
                                                  uint64_t*) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    return S_OK;
  }

  static void AEROGPU_APIENTRY SetError(void* user, HRESULT hr) {
    if (!user) {
      return;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    h->errors.push_back(hr);
  }
};

struct TestDevice {
  Harness harness;

  D3D10DDI_HADAPTER hAdapter = {};
  D3D10DDI_ADAPTERFUNCS adapter_funcs = {};

  D3D10DDI_HDEVICE hDevice = {};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs = {};
  std::vector<uint8_t> device_mem;

  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks = {};
};

bool InitTestDevice(TestDevice* out) {
  if (!out) {
    return false;
  }

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;

  D3D10DDIARG_OPENADAPTER open = {};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create = {};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
    return false;
  }

  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = &out->callbacks;

  hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(hr == S_OK, "CreateDevice")) {
    out->adapter_funcs.pfnCloseAdapter(out->hAdapter);
    out->hAdapter = {};
    return false;
  }

  out->hDevice = create.hDevice;
  out->harness.errors.clear();
  out->harness.last_stream.clear();
  return true;
}

void DestroyTestDevice(TestDevice* dev) {
  if (!dev) {
    return;
  }
  if (dev->device_funcs.pfnDestroyDevice) {
    dev->device_funcs.pfnDestroyDevice(dev->hDevice);
  }
  if (dev->adapter_funcs.pfnCloseAdapter) {
    dev->adapter_funcs.pfnCloseAdapter(dev->hAdapter);
  }
  dev->hDevice = {};
  dev->hAdapter = {};
}
#endif // !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)

bool TestMultiViewportReportsNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  // Two distinct viewports: unsupported by the protocol (single-viewport only).
  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/1.0f,
          /*TopLeftY=*/2.0f,
          /*Width=*/3.0f,
          /*Height=*/4.0f,
          /*MinDepth=*/0.0f,
          /*MaxDepth=*/1.0f,
      },
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/10.0f,
          /*TopLeftY=*/20.0f,
          /*Width=*/30.0f,
          /*Height=*/40.0f,
          /*MinDepth=*/0.25f,
          /*MaxDepth=*/0.75f,
      },
  };

  validate_and_emit_viewports_locked(&dev,
                                     /*num_viewports=*/2,
                                     viewports,
                                     [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.size() == 1, "SetViewports(2 distinct) should report exactly one error")) {
    return false;
  }
  if (!Check(errors[0] == E_NOTIMPL, "SetViewports(2 distinct) should report E_NOTIMPL")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    return false;
  }
  if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_set_viewport), "SET_VIEWPORT packet size")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    return false;
  }
  if (!Check(cmd->min_depth_f32 == f32_bits(vp0.MinDepth), "SET_VIEWPORT min_depth matches first viewport")) {
    return false;
  }
  if (!Check(cmd->max_depth_f32 == f32_bits(vp0.MaxDepth), "SET_VIEWPORT max_depth matches first viewport")) {
    return false;
  }

  return true;
}

bool TestMultiViewportIdenticalDoesNotReportNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/1.0f,
          /*TopLeftY=*/2.0f,
          /*Width=*/3.0f,
          /*Height=*/4.0f,
          /*MinDepth=*/0.0f,
          /*MaxDepth=*/1.0f,
      },
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/1.0f,
          /*TopLeftY=*/2.0f,
          /*Width=*/3.0f,
          /*Height=*/4.0f,
          /*MinDepth=*/0.0f,
          /*MaxDepth=*/1.0f,
      },
  };

  validate_and_emit_viewports_locked(&dev,
                                     /*num_viewports=*/2,
                                     viewports,
                                     [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "SetViewports(2 identical) should not report errors")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    return false;
  }
  if (!Check(cmd->min_depth_f32 == f32_bits(vp0.MinDepth), "SET_VIEWPORT min_depth matches first viewport")) {
    return false;
  }
  if (!Check(cmd->max_depth_f32 == f32_bits(vp0.MaxDepth), "SET_VIEWPORT max_depth matches first viewport")) {
    return false;
  }

  return true;
}

bool TestMultiViewportDisabledExtraDoesNotReportNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  // Second viewport has 0x0 dimensions: treat it as disabled/unused.
  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/1.0f,
          /*TopLeftY=*/2.0f,
          /*Width=*/3.0f,
          /*Height=*/4.0f,
          /*MinDepth=*/0.0f,
          /*MaxDepth=*/1.0f,
      },
      AEROGPU_DDI_VIEWPORT{
          /*TopLeftX=*/10.0f,
          /*TopLeftY=*/20.0f,
          /*Width=*/0.0f,
          /*Height=*/0.0f,
          /*MinDepth=*/0.25f,
          /*MaxDepth=*/0.75f,
      },
  };

  validate_and_emit_viewports_locked(&dev,
                                     /*num_viewports=*/2,
                                     viewports,
                                     [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "SetViewports(1 active + 1 disabled) should not report errors")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    return false;
  }

  return true;
}

struct TestRect {
  int32_t left;
  int32_t top;
  int32_t right;
  int32_t bottom;
};

bool TestMultiScissorReportsNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  const TestRect rects[2] = {
      TestRect{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      TestRect{/*left=*/10, /*top=*/20, /*right=*/30, /*bottom=*/40},
  };

  validate_and_emit_scissor_rects_locked(&dev,
                                         /*num_rects=*/2,
                                         rects,
                                         [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.size() == 1, "SetScissorRects(2 distinct) should report exactly one error")) {
    return false;
  }
  if (!Check(errors[0] == E_NOTIMPL, "SetScissorRects(2 distinct) should report E_NOTIMPL")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    return false;
  }
  if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_set_scissor), "SET_SCISSOR packet size")) {
    return false;
  }

  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    return false;
  }

  return true;
}

bool TestMultiScissorIdenticalDoesNotReportNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  const TestRect rects[2] = {
      TestRect{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      TestRect{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
  };

  validate_and_emit_scissor_rects_locked(&dev,
                                         /*num_rects=*/2,
                                         rects,
                                         [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "SetScissorRects(2 identical) should not report errors")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    return false;
  }

  return true;
}

bool TestMultiScissorDisabledExtraDoesNotReportNotImplAndEmitsFirst() {
  Device dev{};
  std::vector<HRESULT> errors;

  const TestRect rects[2] = {
      TestRect{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      // Disabled/empty rect: width=0, height=0.
      TestRect{/*left=*/10, /*top=*/20, /*right=*/10, /*bottom=*/20},
  };

  validate_and_emit_scissor_rects_locked(&dev,
                                         /*num_rects=*/2,
                                         rects,
                                         [&](HRESULT hr) { errors.push_back(hr); });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "SetScissorRects(1 active + 1 disabled) should not report errors")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    return false;
  }

  return true;
}

bool TestViewportAndScissorDisableEncodesDefaults() {
  Device dev{};
  std::vector<HRESULT> errors;

  validate_and_emit_viewports_locked(&dev,
                                     /*num_viewports=*/0,
                                     /*viewports=*/static_cast<const AEROGPU_DDI_VIEWPORT*>(nullptr),
                                     [&](HRESULT hr) {
    errors.push_back(hr);
  });
  validate_and_emit_scissor_rects_locked(&dev,
                                         /*num_rects=*/0,
                                         /*rects=*/static_cast<const TestRect*>(nullptr),
                                         [&](HRESULT hr) {
    errors.push_back(hr);
  });
  dev.cmd.finalize();

  if (!Check(errors.empty(), "Disabling viewport/scissor should not report errors")) {
    return false;
  }

  const uint8_t* stream = dev.cmd.data();
  const size_t stream_len = dev.cmd.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    return false;
  }

  const auto* vp_hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(vp_hdr != nullptr, "expected SET_VIEWPORT (disable) to be emitted")) {
    return false;
  }
  const auto* vp_cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(vp_hdr);
  if (!Check(vp_cmd->width_f32 == f32_bits(0.0f) && vp_cmd->height_f32 == f32_bits(0.0f),
             "Disable viewport encodes 0x0 dimensions")) {
    return false;
  }

  const auto* sc_hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(sc_hdr != nullptr, "expected SET_SCISSOR (disable) to be emitted")) {
    return false;
  }
  const auto* sc_cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(sc_hdr);
  if (!Check(sc_cmd->width == 0 && sc_cmd->height == 0, "Disable scissor encodes 0x0 dimensions")) {
    return false;
  }

  return true;
}

#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
bool TestPortableUmdMultiViewportReportsNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-viewport)")) {
    return false;
  }

  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/1.0f, /*TopLeftY=*/2.0f, /*Width=*/3.0f, /*Height=*/4.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/10.0f, /*TopLeftY=*/20.0f, /*Width=*/30.0f, /*Height=*/40.0f, /*MinDepth=*/0.25f, /*MaxDepth=*/0.75f},
  };

  dev.device_funcs.pfnSetViewports(dev.hDevice, /*num_viewports=*/2, viewports);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetViewports")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.size() == 1, "Portable SetViewports(2 distinct) should report exactly one error")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_NOTIMPL, "Portable SetViewports(2 distinct) should report E_NOTIMPL")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdMultiViewportIdenticalDoesNotReportNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-viewport identical)")) {
    return false;
  }

  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/1.0f, /*TopLeftY=*/2.0f, /*Width=*/3.0f, /*Height=*/4.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/1.0f, /*TopLeftY=*/2.0f, /*Width=*/3.0f, /*Height=*/4.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
  };

  dev.device_funcs.pfnSetViewports(dev.hDevice, /*num_viewports=*/2, viewports);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetViewports(identical)")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.empty(), "Portable SetViewports(2 identical) should not report errors")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdMultiViewportDisabledExtraDoesNotReportNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-viewport disabled extra)")) {
    return false;
  }

  const AEROGPU_DDI_VIEWPORT viewports[2] = {
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/1.0f, /*TopLeftY=*/2.0f, /*Width=*/3.0f, /*Height=*/4.0f, /*MinDepth=*/0.0f, /*MaxDepth=*/1.0f},
      // Disabled: 0x0 dimensions.
      AEROGPU_DDI_VIEWPORT{/*TopLeftX=*/10.0f, /*TopLeftY=*/20.0f, /*Width=*/0.0f, /*Height=*/0.0f, /*MinDepth=*/0.25f, /*MaxDepth=*/0.75f},
  };

  dev.device_funcs.pfnSetViewports(dev.hDevice, /*num_viewports=*/2, viewports);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetViewports(disabled extra)")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.empty(), "Portable SetViewports(disabled extra) should not report errors")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(hdr != nullptr, "expected SET_VIEWPORT to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
  const auto& vp0 = viewports[0];
  if (!Check(cmd->x_f32 == f32_bits(vp0.TopLeftX), "SET_VIEWPORT x matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y_f32 == f32_bits(vp0.TopLeftY), "SET_VIEWPORT y matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width_f32 == f32_bits(vp0.Width), "SET_VIEWPORT width matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height_f32 == f32_bits(vp0.Height), "SET_VIEWPORT height matches first viewport")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdMultiScissorReportsNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-scissor)")) {
    return false;
  }

  const AEROGPU_DDI_RECT rects[2] = {
      AEROGPU_DDI_RECT{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      AEROGPU_DDI_RECT{/*left=*/10, /*top=*/20, /*right=*/30, /*bottom=*/40},
  };

  dev.device_funcs.pfnSetScissorRects(dev.hDevice, /*num_rects=*/2, rects);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetScissorRects")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.size() == 1, "Portable SetScissorRects(2 distinct) should report exactly one error")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_NOTIMPL, "Portable SetScissorRects(2 distinct) should report E_NOTIMPL")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdMultiScissorIdenticalDoesNotReportNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-scissor identical)")) {
    return false;
  }

  const AEROGPU_DDI_RECT rects[2] = {
      AEROGPU_DDI_RECT{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      AEROGPU_DDI_RECT{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
  };

  dev.device_funcs.pfnSetScissorRects(dev.hDevice, /*num_rects=*/2, rects);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetScissorRects(identical)")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.empty(), "Portable SetScissorRects(2 identical) should not report errors")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdMultiScissorDisabledExtraDoesNotReportNotImplAndEmitsFirst() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable multi-scissor disabled extra)")) {
    return false;
  }

  const AEROGPU_DDI_RECT rects[2] = {
      AEROGPU_DDI_RECT{/*left=*/1, /*top=*/2, /*right=*/3, /*bottom=*/4},
      // Disabled/empty rect: width=0, height=0.
      AEROGPU_DDI_RECT{/*left=*/10, /*top=*/20, /*right=*/10, /*bottom=*/20},
  };

  dev.device_funcs.pfnSetScissorRects(dev.hDevice, /*num_rects=*/2, rects);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after SetScissorRects(disabled extra)")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.empty(), "Portable SetScissorRects(disabled extra) should not report errors")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(hdr != nullptr, "expected SET_SCISSOR to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
  const auto& r0 = rects[0];
  if (!Check(cmd->x == r0.left, "SET_SCISSOR x matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->y == r0.top, "SET_SCISSOR y matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->width == (r0.right - r0.left), "SET_SCISSOR width matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }
  if (!Check(cmd->height == (r0.bottom - r0.top), "SET_SCISSOR height matches first rect")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}

bool TestPortableUmdDisableEncodesDefaultsAndDoesNotReportErrors() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice(portable disable viewport/scissor)")) {
    return false;
  }

  dev.device_funcs.pfnSetViewports(dev.hDevice, /*num_viewports=*/0, /*viewports=*/nullptr);
  dev.device_funcs.pfnSetScissorRects(dev.hDevice, /*num_rects=*/0, /*rects=*/nullptr);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after disable viewport/scissor")) {
    DestroyTestDevice(&dev);
    return false;
  }

  if (!Check(dev.harness.errors.empty(), "Portable disable viewport/scissor should not report errors")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = dev.harness.last_stream.size();
  if (!Check(ValidateStream(stream, stream_len), "ValidateStream")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const auto* vp_hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_VIEWPORT);
  if (!Check(vp_hdr != nullptr, "expected SET_VIEWPORT (disable) to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* vp_cmd = reinterpret_cast<const aerogpu_cmd_set_viewport*>(vp_hdr);
  if (!Check(vp_cmd->width_f32 == f32_bits(0.0f) && vp_cmd->height_f32 == f32_bits(0.0f),
             "Disable viewport encodes 0x0 dimensions")) {
    DestroyTestDevice(&dev);
    return false;
  }

  const auto* sc_hdr = FindLastOpcode(stream, stream_len, AEROGPU_CMD_SET_SCISSOR);
  if (!Check(sc_hdr != nullptr, "expected SET_SCISSOR (disable) to be emitted")) {
    DestroyTestDevice(&dev);
    return false;
  }
  const auto* sc_cmd = reinterpret_cast<const aerogpu_cmd_set_scissor*>(sc_hdr);
  if (!Check(sc_cmd->width == 0 && sc_cmd->height == 0, "Disable scissor encodes 0x0 dimensions")) {
    DestroyTestDevice(&dev);
    return false;
  }

  DestroyTestDevice(&dev);
  return true;
}
#endif // !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)

} // namespace

int main() {
  bool ok = true;
  ok &= TestMultiViewportReportsNotImplAndEmitsFirst();
  ok &= TestMultiViewportIdenticalDoesNotReportNotImplAndEmitsFirst();
  ok &= TestMultiViewportDisabledExtraDoesNotReportNotImplAndEmitsFirst();
  ok &= TestMultiScissorReportsNotImplAndEmitsFirst();
  ok &= TestMultiScissorIdenticalDoesNotReportNotImplAndEmitsFirst();
  ok &= TestMultiScissorDisabledExtraDoesNotReportNotImplAndEmitsFirst();
  ok &= TestViewportAndScissorDisableEncodesDefaults();
#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
  ok &= TestPortableUmdMultiViewportReportsNotImplAndEmitsFirst();
  ok &= TestPortableUmdMultiViewportIdenticalDoesNotReportNotImplAndEmitsFirst();
  ok &= TestPortableUmdMultiViewportDisabledExtraDoesNotReportNotImplAndEmitsFirst();
  ok &= TestPortableUmdMultiScissorReportsNotImplAndEmitsFirst();
  ok &= TestPortableUmdMultiScissorIdenticalDoesNotReportNotImplAndEmitsFirst();
  ok &= TestPortableUmdMultiScissorDisabledExtraDoesNotReportNotImplAndEmitsFirst();
  ok &= TestPortableUmdDisableEncodesDefaultsAndDoesNotReportErrors();
#endif
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_viewport_scissor_validation_tests\n");
  return 0;
}
