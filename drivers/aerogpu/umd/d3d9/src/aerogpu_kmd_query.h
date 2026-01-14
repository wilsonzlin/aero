#pragma once

#include <cstdint>
#include <mutex>

#include "../../../protocol/aerogpu_umd_private.h"

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
#else
  #ifndef AEROGPU_LUID_DEFINED
    #define AEROGPU_LUID_DEFINED
typedef struct _LUID {
  uint32_t LowPart;
  int32_t HighPart;
} LUID;
  #endif
#endif

namespace aerogpu {

// Small helper for querying AeroGPU KMD state via DxgkDdiEscape / D3DKMTEscape.
//
// On Windows 7, D3DKMT* functions are exported by gdi32.dll and are reachable from
// user mode. We resolve the symbols once during Init and keep the adapter handle
// open for low overhead (~60Hz polling from DWM/present thread).
class AerogpuKmdQuery {
 public:
  AerogpuKmdQuery();
  ~AerogpuKmdQuery() noexcept;

  AerogpuKmdQuery(const AerogpuKmdQuery&) = delete;
  AerogpuKmdQuery& operator=(const AerogpuKmdQuery&) = delete;

  // Initializes the helper for a given adapter LUID. Preferred path: use
  // D3DKMTOpenAdapterFromLuid. If unavailable, falls back to matching the LUID
  // using D3DKMTOpenAdapterFromHdc (enumerating display devices).
  bool InitFromLuid(LUID adapter_luid);

#if defined(_WIN32)
  // Convenience init when the caller already has an HDC (e.g. D3D9 OpenAdapter2
  // on Win7). This avoids requiring the caller to translate HDC -> LUID first.
  bool InitFromHdc(HDC hdc);
#endif

  void Shutdown();

  // Queries the last fence values observed by the KMD.
  //
  // NOTE: `last_submitted` is an adapter-global value (shared across all guest
  // processes using the same adapter). It must not be used to infer the fence
  // ID for a specific user-mode submission under multi-process workloads (DWM +
  // apps); per-submission fence IDs must come from the D3D runtime callbacks
  // (for example `SubmissionFenceId` / `NewFenceValue`).
  // `last_completed` is still useful for polling overall forward progress.
  // Returns false if the query path is unavailable (missing exports, adapter
  // open failure, or escape failure).
  bool QueryFence(uint64_t* last_submitted, uint64_t* last_completed);

  // Sends a driver-private Escape packet to the AeroGPU KMD.
  //
  // `data` must point to a packed, pointer-free buffer whose first bytes are
  // `aerogpu_escape_header` (see `drivers/aerogpu/protocol/aerogpu_escape.h`).
  // The buffer may be in/out depending on the opcode.
  //
  // Returns false if the escape path is unavailable (missing exports, adapter
  // open failure, or escape failure).
  bool SendEscape(void* data, uint32_t size);

  // Returns the D3DKMT adapter handle opened by InitFromLuid/InitFromHdc, or 0
  // if the helper is not initialized. This can be used with other D3DKMT calls
  // like D3DKMTWaitForSynchronizationObject.
  uint32_t GetKmtAdapterHandle();

  // Returns the VidPnSourceId associated with the opened adapter when known.
  //
  // This is primarily used for best-effort vblank waits via D3DKMTGetScanLine.
  // Some open paths (e.g. D3DKMTOpenAdapterFromLuid) do not directly provide a
  // VidPnSourceId; in those cases this returns false and callers should fall
  // back to a time-based sleep.
  bool GetVidPnSourceId(uint32_t* out_vid_pn_source_id);
#if defined(_WIN32)
  // Waits for a monitored-fence synchronization object to reach `fence_value`.
  //
  // `timeout_ms` is in milliseconds:
  // - 0: poll (do not block)
  // - INFINITE (0xFFFFFFFF): "infinite" wait (translated to ~0ull for the KMT ABI)
  //
  // Returns the NTSTATUS result of `D3DKMTWaitForSynchronizationObject`, or
  // STATUS_NOT_SUPPORTED if the thunk is unavailable.
  long WaitForSyncObject(uint32_t sync_object, uint64_t fence_value, uint32_t timeout_ms);
#endif

  // Waits until the completed fence is >= `fence`, or until `timeout_ms`
  // elapses. Uses cooperative polling (Sleep(0/1)), not a busy spin.
  bool WaitForFence(uint64_t fence, uint32_t timeout_ms);

  // Queries the AeroGPU UMDRIVERPRIVATE discovery blob from the KMD.
  //
  // Returns false if the query path is unavailable (missing exports, adapter
  // open failure, or query failure).
  bool QueryUmdPrivate(aerogpu_umd_private_v1* out);

  // Queries the KMD-advertised maximum allocation-list slot id
  // (DXGK_DRIVERCAPS::MaxAllocationListSlotId).
  //
  // Returns false if the query path is unavailable (missing exports, adapter
  // open failure, or query failure).
  bool QueryMaxAllocationListSlotId(uint32_t* out_max_slot_id);

  // Best-effort vblank wait using `D3DKMTGetScanLine` polling.
  //
  // Returns false if the scanline query path is unavailable. Otherwise waits
  // until the next vblank transition (or until `timeout_ms` elapses) and returns
  // true.
  bool WaitForVBlank(uint32_t vid_pn_source_id, uint32_t timeout_ms);

  // Best-effort scanline query using `D3DKMTGetScanLine`.
  //
  // Returns true and fills `out_in_vblank` / `out_scan_line` (when non-null) when
  // the scanline query path is available. On failure, output pointers (when
  // non-null) are set to `{false, 0}` and false is returned.
  bool GetScanLine(uint32_t vid_pn_source_id, bool* out_in_vblank, uint32_t* out_scan_line);

 private:
  void ShutdownLocked();

#if defined(_WIN32)
  using NTSTATUS = long;
  using D3DKMT_HANDLE = uint32_t;

  struct D3DKMT_OPENADAPTERFROMLUID;
  struct D3DKMT_OPENADAPTERFROMHDC;
  struct D3DKMT_CLOSEADAPTER;
  struct D3DKMT_QUERYADAPTERINFO;
  struct D3DKMT_ESCAPE;
  struct D3DKMT_GETSCANLINE;

  using PFND3DKMTOpenAdapterFromLuid = NTSTATUS(__stdcall*)(D3DKMT_OPENADAPTERFROMLUID* pData);
  using PFND3DKMTOpenAdapterFromHdc = NTSTATUS(__stdcall*)(D3DKMT_OPENADAPTERFROMHDC* pData);
  using PFND3DKMTCloseAdapter = NTSTATUS(__stdcall*)(D3DKMT_CLOSEADAPTER* pData);
  using PFND3DKMTQueryAdapterInfo = NTSTATUS(__stdcall*)(D3DKMT_QUERYADAPTERINFO* pData);
  using PFND3DKMTEscape = NTSTATUS(__stdcall*)(D3DKMT_ESCAPE* pData);
  using PFND3DKMTGetScanLine = NTSTATUS(__stdcall*)(D3DKMT_GETSCANLINE* pData);

  bool ProbeUmdPrivateTypeLocked();
  bool ProbeDriverCapsTypeLocked();

  HMODULE gdi32_ = nullptr;
  PFND3DKMTOpenAdapterFromLuid open_adapter_from_luid_ = nullptr;
  PFND3DKMTOpenAdapterFromHdc open_adapter_from_hdc_ = nullptr;
  PFND3DKMTCloseAdapter close_adapter_ = nullptr;
  PFND3DKMTQueryAdapterInfo query_adapter_info_ = nullptr;
  PFND3DKMTEscape escape_ = nullptr;
  PFND3DKMTGetScanLine get_scanline_ = nullptr;
  FARPROC wait_for_sync_object_ = nullptr;

  D3DKMT_HANDLE adapter_ = 0;
  LUID adapter_luid_ = {};
  unsigned int vid_pn_source_id_ = 0;
  bool vid_pn_source_id_valid_ = false;

  bool umdriverprivate_type_known_ = false;
  unsigned int umdriverprivate_type_ = 0;

  // Best-effort numeric constant discovery for KMTQAITYPE_DRIVERCAPS. We avoid
  // including WDK headers in the repo build and instead probe a small range.
  bool drivercaps_type_known_ = false;
  unsigned int drivercaps_type_ = 0;
  // Some toolchains disagree on 64-bit alignment rules on x86. Record whether
  // the returned DRIVERCAPS blob uses the expected 4-byte padding after
  // WDDMVersion (pad=4 => HighestAcceptableAddress at offset 8).
  unsigned int drivercaps_wddmversion_padding_bytes_ = 4;

  // Guards the handle + function pointer lifetime for Shutdown vs. Query.
  // Queries are expected at ~60Hz so a lightweight mutex is fine.
  std::mutex mutex_;
#endif
};

} // namespace aerogpu
