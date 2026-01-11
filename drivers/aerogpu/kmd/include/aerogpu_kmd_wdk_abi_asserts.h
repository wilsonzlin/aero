#pragma once

/*
 * Optional compile-time ABI assertions for Win7 WDDM 1.1 KMD builds.
 *
 * The AeroGPU miniport is built with the WDK 10 toolchain, but it targets the
 * Win7 (WDDM 1.1) dxgkrnl ABI. This header provides a place to "freeze" ABI-
 * critical sizes/offsets/enums (captured from a Win7-era header set via the
 * probe tool) so future header/toolchain drift is caught at compile time.
 *
 * This file is intentionally inert unless AEROGPU_KMD_USE_WDK_DDI is defined
 * and truthy (treat `AEROGPU_KMD_USE_WDK_DDI=0` as disabled).
 */

#if !(defined(AEROGPU_KMD_USE_WDK_DDI) && AEROGPU_KMD_USE_WDK_DDI)
/* Repo-local builds may not have the WDK headers; keep this header inert. */
#else

#include <stddef.h>

#include <d3dkmddi.h>

/*
 * Some Win7-era WDK toolchains may not support C11 static_assert; use the traditional
 * typedef trick. Prefer C_ASSERT if the WDK provides it.
 */
#ifndef C_ASSERT
#define C_ASSERT(expr) typedef char aerogpu_c_assert_##__LINE__[(expr) ? 1 : -1]
#endif

#define AEROGPU_ABI_STATIC_ASSERT(expr, msg) C_ASSERT(expr)

/*
 * Internal invariants we rely on when forming vblank notifications.
 *
 * These do not encode absolute offsets (those are captured below via optional
 * expected-value macros); they just ensure the active header set is self-
 * consistent.
 */
AEROGPU_ABI_STATIC_ASSERT(offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync) == offsetof(DXGKARGCB_NOTIFY_INTERRUPT, DmaCompleted),
                          "DXGKARGCB_NOTIFY_INTERRUPT anonymous union offset mismatch");
AEROGPU_ABI_STATIC_ASSERT(offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync.VidPnSourceId) == offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync),
                          "DXGKARGCB_NOTIFY_INTERRUPT.CrtcVsync.VidPnSourceId must be at union base offset");

/* ------------------------------------------------------------------------- */
/* Optional expected-value checks (define macros to enable)                    */
/* ------------------------------------------------------------------------- */

/* Example:
 *   /DAEROGPU_KMD_WDK_ABI_EXPECT_SIZEOF_DXGKARGCB_NOTIFY_INTERRUPT=...
 */
#if defined(AEROGPU_KMD_WDK_ABI_EXPECT_SIZEOF_DXGKARGCB_NOTIFY_INTERRUPT)
AEROGPU_ABI_STATIC_ASSERT(sizeof(DXGKARGCB_NOTIFY_INTERRUPT) == AEROGPU_KMD_WDK_ABI_EXPECT_SIZEOF_DXGKARGCB_NOTIFY_INTERRUPT,
                          "sizeof(DXGKARGCB_NOTIFY_INTERRUPT) does not match expected value");
#endif

/* Example:
 *   /DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync=...
 */
#if defined(AEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync)
AEROGPU_ABI_STATIC_ASSERT(offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync) ==
                              AEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync,
                          "offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync) does not match expected value");
#endif

/* Example:
 *   /DAEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync_VidPnSourceId=...
 */
#if defined(AEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync_VidPnSourceId)
AEROGPU_ABI_STATIC_ASSERT(offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync.VidPnSourceId) ==
                              AEROGPU_KMD_WDK_ABI_EXPECT_OFFSETOF_DXGKARGCB_NOTIFY_INTERRUPT_CrtcVsync_VidPnSourceId,
                          "offsetof(DXGKARGCB_NOTIFY_INTERRUPT, CrtcVsync.VidPnSourceId) does not match expected value");
#endif

/* Example:
 *   /DAEROGPU_KMD_WDK_ABI_EXPECT_DXGK_INTERRUPT_TYPE_CRTC_VSYNC=...
 */
#if defined(AEROGPU_KMD_WDK_ABI_EXPECT_DXGK_INTERRUPT_TYPE_CRTC_VSYNC)
AEROGPU_ABI_STATIC_ASSERT((ULONG)DXGK_INTERRUPT_TYPE_CRTC_VSYNC == AEROGPU_KMD_WDK_ABI_EXPECT_DXGK_INTERRUPT_TYPE_CRTC_VSYNC,
                          "DXGK_INTERRUPT_TYPE_CRTC_VSYNC does not match expected value");
#endif

#endif /* AEROGPU_KMD_USE_WDK_DDI */
