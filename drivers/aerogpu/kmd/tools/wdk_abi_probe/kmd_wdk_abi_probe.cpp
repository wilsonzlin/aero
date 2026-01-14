// AeroGPU KMD - Win7 WDK ABI probe (DXGK vblank interrupt ABI)
//
// This program is intended to be built inside a Win7-era WDK environment
// to capture ABI-critical structure layouts and enum values used by the Win7
// WDDM 1.1 display miniport interface.
//
// It is tooling-only and is not built as part of the normal repo build.
// See README.md in this directory for build steps.

#include <stddef.h>
#include <stdio.h>

// The DXGK DDI types live in kernel-mode headers.
// For this probe we only need type definitions, not linking against kernel libs.
#include <ntddk.h>
#include <d3dkmddi.h>

// MSVC-compatible printf format for size_t.
#if defined(_MSC_VER)
#define AEROGPU_PRIuSIZE "%Iu"
#else
#define AEROGPU_PRIuSIZE "%zu"
#endif

static void print_header(const char* title) { printf("\n== %s ==\n", title); }

static void print_sizeof(const char* type_name, size_t size) { printf("sizeof(%s) = " AEROGPU_PRIuSIZE "\n", type_name, size); }

static void print_offsetof(const char* type_name, const char* member_name, size_t off) {
  printf("  offsetof(%s, %s) = " AEROGPU_PRIuSIZE "\n", type_name, member_name, off);
}

static void probe_notify_interrupt() {
  print_header("DXGKARGCB_NOTIFY_INTERRUPT");

  print_sizeof("DXGKARGCB_NOTIFY_INTERRUPT", sizeof(DXGKARGCB_NOTIFY_INTERRUPT));
  print_offsetof("DXGKARGCB_NOTIFY_INTERRUPT", "InterruptType", offsetof(DXGKARGCB_NOTIFY_INTERRUPT, InterruptType));
  print_offsetof("DXGKARGCB_NOTIFY_INTERRUPT", "DmaCompleted", offsetof(DXGKARGCB_NOTIFY_INTERRUPT, DmaCompleted));
  print_offsetof("DXGKARGCB_NOTIFY_INTERRUPT", "CrtcVsync", offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync));
  print_offsetof("DXGKARGCB_NOTIFY_INTERRUPT", "CrtcVsync.VidPnSourceId",
                 offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync.VidPnSourceId));

  // Sizes of the anonymous union members we care about.
  print_sizeof("DXGKARGCB_NOTIFY_INTERRUPT.DmaCompleted", sizeof(((DXGKARGCB_NOTIFY_INTERRUPT*)0)->DmaCompleted));
  print_sizeof("DXGKARGCB_NOTIFY_INTERRUPT.CrtcVsync", sizeof(((DXGKARGCB_NOTIFY_INTERRUPT*)0)->CrtcVsync));
}

static void probe_interrupt_type_enums() {
  print_header("DXGK_INTERRUPT_TYPE values");

  // Vblank/vsync (Win7 WDDM 1.1): dxgkrnl uses DXGK_INTERRUPT_TYPE_CRTC_VSYNC.
  printf("DXGK_INTERRUPT_TYPE_CRTC_VSYNC = %u\n", (unsigned)DXGK_INTERRUPT_TYPE_CRTC_VSYNC);

  // Include DMA_COMPLETED as a sanity anchor (used by AeroGPU fences).
  printf("DXGK_INTERRUPT_TYPE_DMA_COMPLETED = %u\n", (unsigned)DXGK_INTERRUPT_TYPE_DMA_COMPLETED);
}

static void probe_allocation_flag_masks() {
  print_header("DXGK_ALLOCATIONINFO::Flags masks");

  typedef decltype(((DXGK_ALLOCATIONINFO*)0)->Flags) FlagsT;

  print_sizeof("DXGK_ALLOCATIONINFO", sizeof(DXGK_ALLOCATIONINFO));
  print_offsetof("DXGK_ALLOCATIONINFO", "Size", offsetof(DXGK_ALLOCATIONINFO, Size));
  print_offsetof("DXGK_ALLOCATIONINFO", "Flags", offsetof(DXGK_ALLOCATIONINFO, Flags));
  print_offsetof("DXGK_ALLOCATIONINFO", "SegmentId", offsetof(DXGK_ALLOCATIONINFO, SegmentId));
  print_offsetof("DXGK_ALLOCATIONINFO", "pPrivateDriverData", offsetof(DXGK_ALLOCATIONINFO, pPrivateDriverData));
  print_offsetof("DXGK_ALLOCATIONINFO", "PrivateDriverDataSize", offsetof(DXGK_ALLOCATIONINFO, PrivateDriverDataSize));

  print_sizeof("DXGK_ALLOCATIONINFO::Flags", sizeof(FlagsT));

#if defined(_MSC_VER)
  // Print the bitmask value for each named flag as exposed by this header set.
  // This is useful for decoding the `flags_in`/`flags_out` values dumped by
  // `aerogpu_dbgctl --dump-createalloc` without having to rely on guesswork.
  //
  // NOTE: Some flags may be multi-bit fields; for those, assigning 1 prints the
  // lowest bit in the field.
  printf("  DXGK_ALLOCATIONINFOFLAGS masks (field -> Flags.Value):\n");

  #define PRINT_MASK(FieldName)                                                      \
    __if_exists(FlagsT::FieldName) {                                                \
      FlagsT f = {};                                                                \
      f.Value = 0;                                                                  \
      f.FieldName = 1;                                                              \
      printf("    %-28s 0x%08X\n", #FieldName, (unsigned)f.Value);                  \
    }                                                                               \
    __if_not_exists(FlagsT::FieldName) {                                            \
      printf("    %-28s <n/a>\n", #FieldName);                                      \
    }

  // Common Win7-era bits we care about (Present/backbuffer stability).
  PRINT_MASK(Primary);
  PRINT_MASK(CpuVisible);
  PRINT_MASK(Aperture);

  // Additional flags that may show up in traces (header-dependent).
  PRINT_MASK(NonLocalOnly);
  PRINT_MASK(Swizzled);
  PRINT_MASK(ExistingSysMem);
  PRINT_MASK(Protected);
  PRINT_MASK(Cached);
  PRINT_MASK(WriteCombined);
  PRINT_MASK(Overlay);
  PRINT_MASK(Capture);
  PRINT_MASK(RenderTarget);
  PRINT_MASK(FlipChain);
  PRINT_MASK(FrontBuffer);
  PRINT_MASK(BackBuffer);
  PRINT_MASK(HistoryBuffer);
  PRINT_MASK(IndicationOnly);
  PRINT_MASK(Immutable);
  PRINT_MASK(Invisible);
  PRINT_MASK(Tiled);

  #undef PRINT_MASK
#endif
}

static void probe_createallocation_flag_masks() {
  print_header("DXGKARG_CREATEALLOCATION::Flags masks");

  typedef decltype(((DXGKARG_CREATEALLOCATION*)0)->Flags) FlagsT;
  print_sizeof("DXGKARG_CREATEALLOCATION", sizeof(DXGKARG_CREATEALLOCATION));
  print_offsetof("DXGKARG_CREATEALLOCATION", "Flags", offsetof(DXGKARG_CREATEALLOCATION, Flags));
  print_offsetof("DXGKARG_CREATEALLOCATION", "NumAllocations", offsetof(DXGKARG_CREATEALLOCATION, NumAllocations));
  print_offsetof("DXGKARG_CREATEALLOCATION", "pAllocationInfo", offsetof(DXGKARG_CREATEALLOCATION, pAllocationInfo));
  print_sizeof("DXGKARG_CREATEALLOCATION::Flags", sizeof(FlagsT));

#if defined(_MSC_VER)
  printf("  DXGK_CREATEALLOCATIONFLAGS masks (field -> Flags.Value):\n");

  #define PRINT_MASK(FieldName)                                                      \
    __if_exists(FlagsT::FieldName) {                                                \
      FlagsT f = {};                                                                \
      f.Value = 0;                                                                  \
      f.FieldName = 1;                                                              \
      printf("    %-28s 0x%08X\n", #FieldName, (unsigned)f.Value);                  \
    }                                                                               \
    __if_not_exists(FlagsT::FieldName) {                                            \
      printf("    %-28s <n/a>\n", #FieldName);                                      \
    }

  // Common fields referenced by bring-up debugging.
  PRINT_MASK(CreateResource);
  PRINT_MASK(CreateShared);

  // Other known fields (header-dependent).
  PRINT_MASK(NonSystem);
  PRINT_MASK(Resize);
  PRINT_MASK(OpenSharedResource);

  #undef PRINT_MASK
#endif
}

static void probe_allocation_list_flag_masks() {
  print_header("DXGK_ALLOCATIONLIST::Flags masks");

  typedef decltype(((DXGK_ALLOCATIONLIST*)0)->Flags) FlagsT;

  print_sizeof("DXGK_ALLOCATIONLIST", sizeof(DXGK_ALLOCATIONLIST));
  print_offsetof("DXGK_ALLOCATIONLIST", "hAllocation", offsetof(DXGK_ALLOCATIONLIST, hAllocation));
  print_offsetof("DXGK_ALLOCATIONLIST", "PhysicalAddress", offsetof(DXGK_ALLOCATIONLIST, PhysicalAddress));
  print_offsetof("DXGK_ALLOCATIONLIST", "SegmentId", offsetof(DXGK_ALLOCATIONLIST, SegmentId));
  print_offsetof("DXGK_ALLOCATIONLIST", "Flags", offsetof(DXGK_ALLOCATIONLIST, Flags));

  print_sizeof("DXGK_ALLOCATIONLIST::Flags", sizeof(FlagsT));

#if defined(_MSC_VER)
  printf("  DXGK_ALLOCATIONLIST_FLAGS masks (field -> Flags.Value):\n");

  #define PRINT_MASK(FieldName)                                                      \
    __if_exists(FlagsT::FieldName) {                                                \
      FlagsT f = {};                                                                \
      f.Value = 0;                                                                  \
      f.FieldName = 1;                                                              \
      printf("    %-28s 0x%08X\n", #FieldName, (unsigned)f.Value);                  \
    }                                                                               \
    __if_not_exists(FlagsT::FieldName) {                                            \
      printf("    %-28s <n/a>\n", #FieldName);                                      \
    }

  // The critical bit for AeroGPU alloc-table READONLY propagation:
  // `WriteOperation==1` indicates the DMA buffer writes to the allocation.
  PRINT_MASK(WriteOperation);

  // Other flags may exist depending on header vintage; print a few common ones.
  PRINT_MASK(Accessed);
  PRINT_MASK(UseResidentPriority);

  #undef PRINT_MASK
#endif
}

static void probe_commitvidpn() {
  print_header("DXGKARG_COMMITVIDPN");
  print_sizeof("DXGKARG_COMMITVIDPN", sizeof(DXGKARG_COMMITVIDPN));

#if defined(_MSC_VER)
  __if_exists(DXGKARG_COMMITVIDPN::hFunctionalVidPn) {
    print_offsetof("DXGKARG_COMMITVIDPN", "hFunctionalVidPn", offsetof(DXGKARG_COMMITVIDPN, hFunctionalVidPn));
  }
  __if_not_exists(DXGKARG_COMMITVIDPN::hFunctionalVidPn) { printf("  offsetof(DXGKARG_COMMITVIDPN, hFunctionalVidPn) = <n/a>\n"); }

  __if_exists(DXGKARG_COMMITVIDPN::AffectedVidPnSourceId) {
    print_offsetof("DXGKARG_COMMITVIDPN", "AffectedVidPnSourceId", offsetof(DXGKARG_COMMITVIDPN, AffectedVidPnSourceId));
  }
  __if_not_exists(DXGKARG_COMMITVIDPN::AffectedVidPnSourceId) {
    printf("  offsetof(DXGKARG_COMMITVIDPN, AffectedVidPnSourceId) = <n/a>\n");
  }
#else
  print_offsetof("DXGKARG_COMMITVIDPN", "hFunctionalVidPn", offsetof(DXGKARG_COMMITVIDPN, hFunctionalVidPn));
  print_offsetof("DXGKARG_COMMITVIDPN", "AffectedVidPnSourceId", offsetof(DXGKARG_COMMITVIDPN, AffectedVidPnSourceId));
#endif
}

static void probe_vidpn_source_mode() {
  print_header("D3DKMDT_VIDPN_SOURCE_MODE");
  print_sizeof("D3DKMDT_VIDPN_SOURCE_MODE", sizeof(D3DKMDT_VIDPN_SOURCE_MODE));
  print_offsetof("D3DKMDT_VIDPN_SOURCE_MODE", "Type", offsetof(D3DKMDT_VIDPN_SOURCE_MODE, Type));
  print_offsetof("D3DKMDT_VIDPN_SOURCE_MODE", "Format", offsetof(D3DKMDT_VIDPN_SOURCE_MODE, Format));
  print_offsetof("D3DKMDT_VIDPN_SOURCE_MODE", "Format.Graphics.PrimSurfSize",
                 offsetof(D3DKMDT_VIDPN_SOURCE_MODE, Format.Graphics.PrimSurfSize));
  print_offsetof("D3DKMDT_VIDPN_SOURCE_MODE", "Format.Graphics.PrimSurfSize.cx",
                 offsetof(D3DKMDT_VIDPN_SOURCE_MODE, Format.Graphics.PrimSurfSize.cx));
  print_offsetof("D3DKMDT_VIDPN_SOURCE_MODE", "Format.Graphics.PrimSurfSize.cy",
                 offsetof(D3DKMDT_VIDPN_SOURCE_MODE, Format.Graphics.PrimSurfSize.cy));
}

int main() {
  printf("AeroGPU KMD WDK ABI probe\n");

#if defined(_MSC_VER)
  printf("_MSC_VER = %d\n", _MSC_VER);
#endif

#if defined(_M_IX86)
  printf("arch = x86\n");
#elif defined(_M_X64)
  printf("arch = x64\n");
#else
  printf("arch = (unknown)\n");
#endif

  printf("sizeof(void*) = " AEROGPU_PRIuSIZE "\n", sizeof(void*));

#ifdef DXGKDDI_INTERFACE_VERSION_WDDM1_1
  printf("DXGKDDI_INTERFACE_VERSION_WDDM1_1 = %u\n", (unsigned)DXGKDDI_INTERFACE_VERSION_WDDM1_1);
#endif

  probe_interrupt_type_enums();
  probe_notify_interrupt();
  probe_allocation_flag_masks();
  probe_allocation_list_flag_masks();
  probe_createallocation_flag_masks();
  probe_commitvidpn();
  probe_vidpn_source_mode();

  return 0;
}
