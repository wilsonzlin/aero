// AeroGPU KMD - Win7 WDK 7.1 ABI probe (DXGK vblank interrupt ABI)
//
// This program is intended to be built inside a Windows 7 WDK 7.1 environment
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

  return 0;
}

