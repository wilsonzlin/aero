#pragma once

/*
 * Lightweight debug logging for the AeroGPU WDDM miniport.
 *
 * This driver is expected to be brought up under WinDbg on Windows 7. DbgPrintEx
 * is the lowest-common-denominator logging facility available in WDK 7.1.
 */

#include <ntddk.h>

/*
 * Use the IHV video component id when available; fall back to IHV driver id
 * otherwise.
 */
#ifndef DPFLTR_IHVVIDEO_ID
#define DPFLTR_IHVVIDEO_ID DPFLTR_IHVDRIVER_ID
#endif

#ifndef AEROGPU_LOG_LEVEL
#define AEROGPU_LOG_LEVEL DPFLTR_INFO_LEVEL
#endif

#if DBG
#define AEROGPU_LOG(fmt, ...) \
    DbgPrintEx(DPFLTR_IHVVIDEO_ID, AEROGPU_LOG_LEVEL, "aerogpu-kmd: " fmt "\n", __VA_ARGS__)
#define AEROGPU_LOG0(msg) \
    DbgPrintEx(DPFLTR_IHVVIDEO_ID, AEROGPU_LOG_LEVEL, "aerogpu-kmd: %s\n", msg)
#else
#define AEROGPU_LOG(...) ((void)0)
#define AEROGPU_LOG0(...) ((void)0)
#endif

