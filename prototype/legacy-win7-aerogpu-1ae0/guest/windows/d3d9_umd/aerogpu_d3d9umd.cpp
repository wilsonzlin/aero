#include <windows.h>

#include <d3dkmthk.h>
#include <d3dumddi.h>

#include "../common/aerogpu_protocol.h"

// This UMD is intentionally minimal. It is structured as a thin command
// serializer that forwards an opaque D3D9 stream to the KMD via D3DKMT_ESCAPE.
//
// The host-side D3D9→WebGPU translator owns the stream format; the only guest
// ABI surface is the aerogpu_protocol.h command envelope + escape packet.

namespace {

struct AerogpuAdapter {
  D3DKMT_HANDLE hAdapter;
};

HRESULT SubmitStream(D3DKMT_HANDLE hAdapter, const void *stream, uint32_t streamBytes, uint64_t *outFence) {
  if (outFence != nullptr) {
    *outFence = 0;
  }

  const uint32_t packetSize =
      sizeof(aerogpu_escape_packet_t) + sizeof(aerogpu_escape_submit_t) + streamBytes;

  aerogpu_escape_packet_t *packet =
      (aerogpu_escape_packet_t *)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, packetSize);
  if (packet == nullptr) {
    return E_OUTOFMEMORY;
  }

  packet->magic = AEROGPU_ESCAPE_MAGIC;
  packet->version = AEROGPU_ESCAPE_VERSION;
  packet->op = AEROGPU_ESCAPE_SUBMIT;
  packet->size_bytes = packetSize;

  auto *submit = (aerogpu_escape_submit_t *)(packet + 1);
  submit->fence_value = 0;
  submit->stream_bytes = streamBytes;

  void *dst = (uint8_t *)packet + sizeof(aerogpu_escape_packet_t) + sizeof(aerogpu_escape_submit_t);
  CopyMemory(dst, stream, streamBytes);

  D3DKMT_ESCAPE esc = {};
  esc.hAdapter = hAdapter;
  esc.Type = D3DKMT_ESCAPE_DRIVERPRIVATE;
  esc.pPrivateDriverData = packet;
  esc.PrivateDriverDataSize = packetSize;

  NTSTATUS st = D3DKMTEscape(&esc);

  if (st == STATUS_SUCCESS && outFence != nullptr) {
    *outFence = submit->fence_value;
  }

  HeapFree(GetProcessHeap(), 0, packet);

  return HRESULT_FROM_NT(st);
}

} // namespace

extern "C" HRESULT APIENTRY OpenAdapter(_Inout_ D3DDDIARG_OPENADAPTER *pOpenAdapter) {
  if (pOpenAdapter == nullptr) {
    return E_INVALIDARG;
  }

  // v1: this is a stub that validates that the runtime can load the DLL.
  // A full implementation will populate the adapter and device function tables
  // and drive all D3D9 rendering through SubmitStream().
  UNREFERENCED_PARAMETER(pOpenAdapter);

  return E_NOTIMPL;
}

