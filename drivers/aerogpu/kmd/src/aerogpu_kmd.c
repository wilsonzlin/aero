#include "aerogpu_ring.h"

#include "aerogpu_kmd.h"
#include <ntintsafe.h>
#include "aerogpu_kmd_wdk_abi_asserts.h"
#include "aerogpu_dbgctl_escape.h"
#include "aerogpu_cmd.h"
#include "aerogpu_umd_private.h"
#include "aerogpu_wddm_alloc.h"
#include "aerogpu_win7_abi.h"

/* See AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE. */
extern POBJECT_TYPE* MmSectionObjectType;

#define AEROGPU_VBLANK_PERIOD_NS_DEFAULT 16666667u

/* ---- Dbgctl READ_GPA security gating ------------------------------------ */

/*
 * These functions are exported by ntoskrnl but are not declared in all header
 * sets we build against (the AeroGPU Win7 KMD is built with a newer WDK).
 *
 * Declare minimal prototypes to avoid pulling in additional headers.
 */
extern BOOLEAN NTAPI SeSinglePrivilegeCheck(_In_ LUID PrivilegeValue, _In_ KPROCESSOR_MODE PreviousMode);
extern BOOLEAN NTAPI SeTokenIsAdmin(_In_ PACCESS_TOKEN Token);
extern VOID NTAPI PsDereferencePrimaryToken(_In_ PACCESS_TOKEN PrimaryToken);

/*
 * AeroGPU exposes a single system-memory-backed segment (Aperture + CpuVisible).
 *
 * Historically this was hard-coded to 512MiB, which is sufficient for bring-up
 * but can cause D3D9/D3D11 workloads to fail allocations due to an artificially
 * small WDDM segment budget.
 *
 * Allow tuning via registry:
 *   HKR\Parameters\NonLocalMemorySizeMB (REG_DWORD, megabytes)
 *
 * This value controls the segment size reported via DXGKQAITYPE_QUERYSEGMENT and
 * DXGKQAITYPE_GETSEGMENTGROUPSIZE. It is a budget hint to dxgkrnl (not dedicated
 * VRAM); allocations are backed by pageable guest system memory and consumed by
 * the emulator via physical addresses.
 *
 * Clamp values to avoid unrealistic budgets and keep Win7 x86 guests safe.
 */
#define AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_MIN 128u
#if defined(_WIN64)
#define AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_DEFAULT 512u
#define AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_MAX 2048u
#else
#define AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_DEFAULT 512u
#define AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_MAX 1024u
#endif

/* Internal-only bits stored in AEROGPU_ALLOCATION::Flags (not exposed to UMD). */
#define AEROGPU_KMD_ALLOC_FLAG_OPENED 0x80000000u
#define AEROGPU_KMD_ALLOC_FLAG_PRIMARY 0x40000000u

/*
 * DXGI_FORMAT subset used by KMD-only helpers.
 *
 * The AeroGPU allocation private-data v2 blob stores the DXGI_FORMAT numeric
 * value for Texture2D allocations. Win7's dxgkrnl can optionally pre-populate
 * DXGKARG_LOCK::Pitch/SlicePitch for surface locks; AeroGPU overrides these to
 * match the UMD-selected packed layout, which requires being able to compute the
 * number of rows in the mip0 layout for block-compressed formats.
 */
#define AEROGPU_DXGI_FORMAT_BC1_TYPELESS 70u
#define AEROGPU_DXGI_FORMAT_BC1_UNORM 71u
#define AEROGPU_DXGI_FORMAT_BC1_UNORM_SRGB 72u
#define AEROGPU_DXGI_FORMAT_BC2_TYPELESS 73u
#define AEROGPU_DXGI_FORMAT_BC2_UNORM 74u
#define AEROGPU_DXGI_FORMAT_BC2_UNORM_SRGB 75u
#define AEROGPU_DXGI_FORMAT_BC3_TYPELESS 76u
#define AEROGPU_DXGI_FORMAT_BC3_UNORM 77u
#define AEROGPU_DXGI_FORMAT_BC3_UNORM_SRGB 78u
#define AEROGPU_DXGI_FORMAT_BC7_TYPELESS 97u
#define AEROGPU_DXGI_FORMAT_BC7_UNORM 98u
#define AEROGPU_DXGI_FORMAT_BC7_UNORM_SRGB 99u

static __forceinline BOOLEAN AeroGpuDxgiFormatIsBlockCompressed(_In_ ULONG DxgiFormat)
{
    switch (DxgiFormat) {
    case AEROGPU_DXGI_FORMAT_BC1_TYPELESS:
    case AEROGPU_DXGI_FORMAT_BC1_UNORM:
    case AEROGPU_DXGI_FORMAT_BC1_UNORM_SRGB:
    case AEROGPU_DXGI_FORMAT_BC2_TYPELESS:
    case AEROGPU_DXGI_FORMAT_BC2_UNORM:
    case AEROGPU_DXGI_FORMAT_BC2_UNORM_SRGB:
    case AEROGPU_DXGI_FORMAT_BC3_TYPELESS:
    case AEROGPU_DXGI_FORMAT_BC3_UNORM:
    case AEROGPU_DXGI_FORMAT_BC3_UNORM_SRGB:
    case AEROGPU_DXGI_FORMAT_BC7_TYPELESS:
    case AEROGPU_DXGI_FORMAT_BC7_UNORM:
    case AEROGPU_DXGI_FORMAT_BC7_UNORM_SRGB:
        return TRUE;
    default:
        return FALSE;
    }
}

/*
 * Hard cap on per-submit allocation-list sizes we will process when building submission metadata
 * (AEROGPU_SUBMISSION_META), legacy submission descriptors, and per-submit allocation tables.
 *
 * Win7's driver caps advertise `MaxAllocationListSlotId = 0xFFFF`. Submissions are typically far
 * smaller, but keep our cap aligned with the public contract while still preventing absurd values
 * from driving integer overflows / unbounded allocations.
 */
#define AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT 0xFFFFu

/*
 * Retain a small window of recently retired submissions so dbgctl tooling can
 * still dump the most recent command stream bytes even if the submission has
 * already completed (common when debugging intermittent rendering issues on a
 * fast emulator/backend).
 *
 * These buffers are physically-contiguous, non-paged allocations; keep the
 * window small and enforce a tight total cap.
 */
#define AEROGPU_DBGCTL_RECENT_SUBMISSIONS_MAX_COUNT 8u
#define AEROGPU_DBGCTL_RECENT_SUBMISSIONS_MAX_BYTES (4ull * 1024ull * 1024ull) /* 4 MiB */

/*
 * NTSTATUS used to surface deterministic device-lost semantics to dxgkrnl/user-mode.
 *
 * STATUS_GRAPHICS_DEVICE_REMOVED maps to DXGI_ERROR_DEVICE_REMOVED / D3DERR_DEVICELOST
 * style failures in user-mode, without requiring a GPU hang/TDR path.
 */
#ifndef STATUS_GRAPHICS_DEVICE_REMOVED
#define STATUS_GRAPHICS_DEVICE_REMOVED ((NTSTATUS)0xC01E0001L)
#endif

/*
 * Legacy device models may optionally mirror `FEATURES_LO/HI` to ease incremental
 * bring-up. See `drivers/aerogpu/protocol/aerogpu_pci.h` for AEROGPU_FEATURE_*
 * bit definitions.
 */
#define AEROGPU_KMD_LEGACY_PLAUSIBLE_FEATURES_MASK                                                               \
    (AEROGPU_FEATURE_FENCE_PAGE | AEROGPU_FEATURE_CURSOR | AEROGPU_FEATURE_SCANOUT | AEROGPU_FEATURE_VBLANK |    \
     AEROGPU_FEATURE_TRANSFER | AEROGPU_FEATURE_ERROR_INFO)

/*
 * Upper bound on the number of pending Render/Present meta handles.
 *
 * These handles are produced by DxgkDdiRender/DxgkDdiPresent and consumed by
 * DxgkDdiSubmitCommand. If SubmitCommand never arrives (or repeatedly fails
 * before taking the handle), PendingMetaHandles can otherwise grow without
 * bound and consume nonpaged resources.
 */
#define AEROGPU_PENDING_META_HANDLES_MAX_COUNT 4096u
#if defined(_WIN64)
#define AEROGPU_PENDING_META_HANDLES_MAX_BYTES (256ull * 1024ull * 1024ull) /* 256 MiB */
#else
#define AEROGPU_PENDING_META_HANDLES_MAX_BYTES (64ull * 1024ull * 1024ull) /* 64 MiB */
#endif

#if DBG
/*
 * DBG-only rate limiting for logs that can be triggered by misbehaving guests.
 *
 * We log the first few instances and then only at exponentially increasing
 * intervals (power-of-two counts) to avoid spamming the kernel debugger while
 * still leaving breadcrumbs.
 */
#define AEROGPU_LOG_RATELIMITED(counter, burst, fmt, ...)                                                    \
    do {                                                                                                      \
        LONG _n = InterlockedIncrement(&(counter));                                                           \
        if (_n <= (burst) || ((_n & (_n - 1)) == 0)) {                                                        \
            AEROGPU_LOG(fmt, __VA_ARGS__);                                                                     \
            if (_n == (burst)) {                                                                               \
                AEROGPU_LOG0("... further messages of this type suppressed (ratelimited)");                    \
            }                                                                                                  \
        }                                                                                                      \
    } while (0)
#else
#define AEROGPU_LOG_RATELIMITED(counter, burst, fmt, ...) ((void)0)
#endif

#if DBG
static volatile LONG g_PendingMetaHandleCapLogCount = 0;
#endif

/*
 * Optional CreateAllocation tracing.
 *
 * DXGI swapchain backbuffers are typically "normal" non-shared, single-allocation
 * resources, so the default CreateAllocation logging (shared or multi-allocation
 * only) may miss them. Define this to 1 in a DBG build to log the first handful
 * of CreateAllocation calls and capture the exact DXGK_ALLOCATIONINFO::Flags
 * values Win7's DXGI/D3D runtime requests for backbuffers.
 */
#ifndef AEROGPU_KMD_TRACE_CREATEALLOCATION
#define AEROGPU_KMD_TRACE_CREATEALLOCATION 0
#endif

/*
 * WDDM miniport entrypoint from dxgkrnl.
 *
 * The WDK import library provides the symbol, but it is declared here to avoid
 * relying on non-universal headers.
 */
NTSTATUS APIENTRY DxgkInitialize(_In_ PDRIVER_OBJECT DriverObject,
                                 _In_ PUNICODE_STRING RegistryPath,
                                 _Inout_ PDXGK_INITIALIZATION_DATA InitializationData);

/* ---- WDDM vblank interrupt plumbing ------------------------------------- */

/*
 * Win7 (WDDM 1.1) vblank delivery contract:
 *
 * - dxgkrnl enables/disables vblank interrupts via DxgkDdiControlInterrupt with
 *   InterruptType = DXGK_INTERRUPT_TYPE_CRTC_VSYNC.
 * - When a vblank occurs for VidPn source N, the miniport must notify dxgkrnl
 *   via DxgkCbNotifyInterrupt with:
 *     notify.InterruptType = DXGK_INTERRUPT_TYPE_CRTC_VSYNC
 *     notify.CrtcVsync.VidPnSourceId = N
 *
 * Historically this driver used a "best effort" anonymous-union write to stuff
 * VidPnSourceId into DXGKARGCB_NOTIFY_INTERRUPT, but that is brittle across WDK
 * header variants and can break Win7's D3DKMTWaitForVerticalBlankEvent /
 * IDirect3DDevice9::GetRasterStatus paths. Keep this code ABI-explicit.
 */

/* ---- EDID (single virtual monitor) ------------------------------------- */

static const UCHAR g_AeroGpuEdid[128] = {
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x04, 0xB2, 0x01, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x23, 0x01, 0x04, 0x80, 0x34, 0x1D, 0x78,
    0x06, 0xA5, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54, 0xA5, 0x4B, 0x00, 0x21,
    0x08, 0x00, 0x45, 0x40, 0x61, 0x40, 0x81, 0xC0, 0x81, 0x00, 0xD1, 0xC0,
    0xA9, 0xC0,
    0x01, 0x01, 0x01, 0x01, 0x02, 0x3A, 0x80, 0x18, 0x71, 0x38,
    0x2D, 0x40, 0x58, 0x2C, 0x45, 0x00, 0x08, 0x22, 0x21, 0x00, 0x00, 0x1E,
    0x00, 0x00, 0x00, 0xFC, 0x00, 0x41, 0x65, 0x72, 0x6F, 0x47, 0x50, 0x55,
    0x20, 0x4D, 0x6F, 0x6E, 0x69, 0x74, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x30, 0x0A,
    0x00, 0x00, 0x00, 0xFD, 0x00, 0x38, 0x4C, 0x1E, 0x53, 0x11, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x76
};

static BOOLEAN AeroGpuIsEdidValid(_In_reads_bytes_(128) const UCHAR* Edid)
{
    if (!Edid) {
        return FALSE;
    }

    /* Validate base EDID header. */
    static const UCHAR kEdidHeader[8] = {0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00};
    if (RtlCompareMemory(Edid, kEdidHeader, sizeof(kEdidHeader)) != sizeof(kEdidHeader)) {
        return FALSE;
    }

    /* Validate checksum: sum of 128 bytes must be 0 mod 256. */
    UCHAR sum = 0;
    for (ULONG i = 0; i < 128; ++i) {
        sum = (UCHAR)(sum + Edid[i]);
    }
    if (sum != 0) {
        return FALSE;
    }

    return TRUE;
}

static BOOLEAN AeroGpuTryParseEdidPreferredMode(_In_reads_bytes_(128) const UCHAR* Edid, _Out_ ULONG* Width, _Out_ ULONG* Height)
{
    if (!Edid || !Width || !Height) {
        return FALSE;
    }

    *Width = 0;
    *Height = 0;

    if (!AeroGpuIsEdidValid(Edid)) {
        return FALSE;
    }

    /*
     * Base EDID block detailed timing descriptor #1 begins at offset 54.
     * See VESA EDID 1.3/1.4: byte layout for detailed timing descriptors.
     */
    enum { kDtdOffset = 54 };
    const UCHAR* dtd = Edid + kDtdOffset;

    const USHORT pixelClock10khz = (USHORT)dtd[0] | ((USHORT)dtd[1] << 8);
    if (pixelClock10khz == 0) {
        return FALSE;
    }

    const ULONG hActive = (ULONG)dtd[2] | (((ULONG)(dtd[4] & 0xF0u)) << 4);
    const ULONG vActive = (ULONG)dtd[5] | (((ULONG)(dtd[7] & 0xF0u)) << 4);
    if (hActive == 0 || vActive == 0) {
        return FALSE;
    }

    *Width = hActive;
    *Height = vActive;
    return TRUE;
}

/* ---- Display mode list helpers ----------------------------------------- */

typedef struct _AEROGPU_DISPLAY_MODE {
    ULONG Width;
    ULONG Height;
} AEROGPU_DISPLAY_MODE;

/*
 * Registry-configurable display mode overrides.
 *
 * These are loaded once in DriverEntry from the miniport service key and applied
 * to VidPN mode enumeration and the initial cached scanout mode.
 *
 * All values are optional (0 means "unset").
 */
typedef struct _AEROGPU_DISPLAY_MODE_CONFIG {
    ULONG PreferredWidth;
    ULONG PreferredHeight;
    ULONG MaxWidth;
    ULONG MaxHeight;
} AEROGPU_DISPLAY_MODE_CONFIG;

static AEROGPU_DISPLAY_MODE_CONFIG g_AeroGpuDisplayModeConfig = {0, 0, 0, 0};

/*
 * Dbgctl escape gating.
 *
 * READ_GPA and MAP_SHARED_HANDLE are debug-only and potentially unsafe. Gate them behind
 * registry-controlled flags under the miniport service key (and require a privileged caller):
 *   HKLM\SYSTEM\CurrentControlSet\Services\aerogpu\Parameters
 *     - EnableReadGpaEscape (REG_DWORD)
 *     - EnableMapSharedHandleEscape (REG_DWORD)
 *
 * Default is disabled (0 / missing value).
 */
static ULONG g_AeroGpuEnableReadGpaEscape = 0;
static ULONG g_AeroGpuEnableMapSharedHandleEscape = 0;

#if DBG
static LONG g_AeroGpuBlockedReadGpaEscapeCount = 0;
static LONG g_AeroGpuBlockedMapSharedHandleEscapeCount = 0;
#endif

/*
 * Submission / contiguous allocation limits.
 *
 * These are loaded once in DriverEntry from the miniport service key
 * (HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters).
 */
static ULONG g_AeroGpuMaxDmaBufferBytes = AEROGPU_KMD_MAX_DMA_BUFFER_BYTES;

static __forceinline BOOLEAN AeroGpuModeWithinMax(_In_ ULONG Width, _In_ ULONG Height)
{
    if (Width == 0 || Height == 0) {
        return FALSE;
    }
    if (Width > 16384u || Height > 16384u) {
        return FALSE;
    }
    if (g_AeroGpuDisplayModeConfig.MaxWidth != 0 && Width > g_AeroGpuDisplayModeConfig.MaxWidth) {
        return FALSE;
    }
    if (g_AeroGpuDisplayModeConfig.MaxHeight != 0 && Height > g_AeroGpuDisplayModeConfig.MaxHeight) {
        return FALSE;
    }
    return TRUE;
}

static BOOLEAN AeroGpuModeListContains(_In_reads_(Count) const AEROGPU_DISPLAY_MODE* Modes, _In_ UINT Count, _In_ ULONG Width, _In_ ULONG Height)
{
    for (UINT i = 0; i < Count; ++i) {
        if (Modes[i].Width == Width && Modes[i].Height == Height) {
            return TRUE;
        }
    }
    return FALSE;
}

static BOOLEAN AeroGpuModeListContainsApprox(_In_reads_(Count) const AEROGPU_DISPLAY_MODE* Modes,
                                            _In_ UINT Count,
                                            _In_ ULONG Width,
                                            _In_ ULONG Height,
                                            _In_ ULONG TolerancePixels)
{
    for (UINT i = 0; i < Count; ++i) {
        const ULONG mw = Modes[i].Width;
        const ULONG mh = Modes[i].Height;
        const ULONG diffW = (mw > Width) ? (mw - Width) : (Width - mw);
        const ULONG diffH = (mh > Height) ? (mh - Height) : (Height - mh);
        if (diffW <= TolerancePixels && diffH <= TolerancePixels) {
            return TRUE;
        }
    }
    return FALSE;
}

static VOID AeroGpuModeListAddUnique(_Inout_updates_(Capacity) AEROGPU_DISPLAY_MODE* Modes,
                                     _Inout_ UINT* Count,
                                     _In_ UINT Capacity,
                                     _In_ ULONG Width,
                                    _In_ ULONG Height)
{
    if (!Modes || !Count) {
        return;
    }
    if (*Count >= Capacity) {
        return;
    }
    if (!AeroGpuModeWithinMax(Width, Height)) {
        return;
    }
    if (AeroGpuModeListContains(Modes, *Count, Width, Height)) {
        return;
    }

    Modes[*Count].Width = Width;
    Modes[*Count].Height = Height;
    (*Count)++;
}

static VOID AeroGpuAppendEdidStandardTimings(_In_reads_bytes_(128) const UCHAR* Edid,
                                             _Inout_updates_(Capacity) AEROGPU_DISPLAY_MODE* Modes,
                                             _Inout_ UINT* Count,
                                             _In_ UINT Capacity)
{
    if (!Edid || !Modes || !Count || Capacity == 0) {
        return;
    }
    if (!AeroGpuIsEdidValid(Edid)) {
        return;
    }

    /*
     * EDID standard timing identifiers: 8 entries at offsets 38..53 (inclusive).
     *
     * Byte 0: (horizontal_active / 8) - 31
     * Byte 1:
     *   bits 7-6: aspect ratio
     *   bits 5-0: refresh_rate - 60
     *
     * Aspect ratio encoding:
     *   - EDID 1.4:   00 = 16:10, 01 = 4:3, 10 = 5:4, 11 = 16:9
     *   - EDID <=1.3: 00 = 1:1,   01 = 4:3, 10 = 5:4, 11 = 16:9
     */
    const UCHAR edidVersion = Edid[18];
    const UCHAR edidRevision = Edid[19];
    const BOOLEAN isEdid14OrLater = (edidVersion > 1) || (edidVersion == 1 && edidRevision >= 4);
    for (UINT i = 0; i < 8; ++i) {
        const UCHAR b0 = Edid[38 + i * 2];
        const UCHAR b1 = Edid[38 + i * 2 + 1];
        if (b0 == 0x01 && b1 == 0x01) {
            continue;
        }

        /*
         * We only support a ~60 Hz scanout cadence today. Standard timing entries
         * encode refresh as (rate - 60) in the low 6 bits; require an exact 60 Hz
         * entry here and rely on the curated fallback list for additional modes.
         */
        if ((b1 & 0x3Fu) != 0) {
            continue;
        }

        const ULONG hActive = ((ULONG)b0 + 31u) * 8u;
        const ULONG aspect = (ULONG)(b1 >> 6) & 0x3u;

        ULONGLONG num = 0;
        ULONGLONG den = 0;
        switch (aspect) {
        case 0: /* EDID 1.4: 16:10, EDID <=1.3: 1:1 */
            num = isEdid14OrLater ? 10 : 1;
            den = isEdid14OrLater ? 16 : 1;
            break;
        case 1: /* 4:3 */
            num = 3;
            den = 4;
            break;
        case 2: /* 5:4 */
            num = 4;
            den = 5;
            break;
        case 3: /* 16:9 */
            num = 9;
            den = 16;
            break;
        default:
            num = 0;
            den = 0;
            break;
        }
        if (num == 0 || den == 0) {
            continue;
        }

        const ULONGLONG prod = (ULONGLONG)hActive * num;
        ULONG vActive = (ULONG)(prod / den);
        const ULONGLONG rem = prod % den;

        if (hActive == 0 || vActive == 0) {
            continue;
        }

        /*
         * The spec does not define rounding rules when the ratio doesn't divide
         * evenly; snap to a multiple of 8 lines to match how common modes are
         * represented in Windows (for example 1368x768 rather than 1368x769).
         */
        if (rem != 0) {
            const ULONG down = vActive & ~7u;
            const ULONG up = (vActive + 7u) & ~7u;

            /*
             * Choose the closest multiple-of-8 to the exact rational value
             * (hActive * num / den), not just to the floored integer.
             */
            const ULONGLONG downProd = (ULONGLONG)down * den;
            const ULONGLONG upProd = (ULONGLONG)up * den;
            const ULONGLONG diffDown = (prod > downProd) ? (prod - downProd) : (downProd - prod);
            const ULONGLONG diffUp = (prod > upProd) ? (prod - upProd) : (upProd - prod);

            const ULONG aligned = (diffUp < diffDown) ? up : down;
            if (aligned != 0) {
                vActive = aligned;
            }
        }
        if (vActive == 0) {
            continue;
        }

        /* Avoid near-duplicate modes (e.g. 1366x768 vs 1368x768). */
        if (AeroGpuModeListContainsApprox(Modes, *Count, hActive, vActive, 2u)) {
            continue;
        }

        AeroGpuModeListAddUnique(Modes, Count, Capacity, hActive, vActive);
    }
}

static UINT AeroGpuBuildModeList(_Out_writes_(Capacity) AEROGPU_DISPLAY_MODE* Modes, _In_ UINT Capacity)
{
    if (!Modes || Capacity == 0) {
        return 0;
    }

    UINT count = 0;

    /* Preferred mode: registry override -> EDID -> fallback. */
    ULONG prefW = 0;
    ULONG prefH = 0;

    if (g_AeroGpuDisplayModeConfig.PreferredWidth != 0 && g_AeroGpuDisplayModeConfig.PreferredHeight != 0) {
        prefW = g_AeroGpuDisplayModeConfig.PreferredWidth;
        prefH = g_AeroGpuDisplayModeConfig.PreferredHeight;
    } else {
        (void)AeroGpuTryParseEdidPreferredMode(g_AeroGpuEdid, &prefW, &prefH);
    }

    if (prefW != 0 && prefH != 0) {
        AeroGpuModeListAddUnique(Modes, &count, Capacity, prefW, prefH);
    }

    /*
     * Curated fallback list (all modes treated as 60 Hz, progressive).
     *
     * Keep this small/deterministic for Win7 bring-up stability.
     */
    static const AEROGPU_DISPLAY_MODE kFallback[] = {
        {640, 480},
        {800, 600},
        {1024, 768},
        {1280, 720},
        {1280, 800},
        {1366, 768},
        {1600, 900},
        {1920, 1080},
    };

    for (UINT i = 0; i < (UINT)(sizeof(kFallback) / sizeof(kFallback[0])); ++i) {
        AeroGpuModeListAddUnique(Modes, &count, Capacity, kFallback[i].Width, kFallback[i].Height);
    }

    /* Additional modes derived from EDID standard timings (best-effort). */
    AeroGpuAppendEdidStandardTimings(g_AeroGpuEdid, Modes, &count, Capacity);

    /*
     * Always keep a known-good conservative mode available unless explicitly
     * filtered out by a max-resolution cap.
     */
    if (count == 0) {
        AeroGpuModeListAddUnique(Modes, &count, Capacity, 1024, 768);
    }

    return count;
}

static BOOLEAN AeroGpuSafeAlignUpU32(_In_ ULONG Value, _In_ ULONG Alignment, _Out_ ULONG* Out)
{
    if (!Out) {
        return FALSE;
    }
    *Out = 0;

    if (Alignment == 0) {
        return FALSE;
    }
    const ULONG mask = Alignment - 1;
    if ((Alignment & mask) != 0) {
        return FALSE;
    }

    if (Value > (0xFFFFFFFFu - mask)) {
        return FALSE;
    }
    *Out = (Value + mask) & ~mask;
    return TRUE;
}

static BOOLEAN AeroGpuComputeDefaultPitchBytes(_In_ ULONG Width, _Out_ ULONG* PitchBytes)
{
    if (!PitchBytes) {
        return FALSE;
    }
    *PitchBytes = 0;

    if (Width == 0 || Width > (0xFFFFFFFFu / 4u)) {
        return FALSE;
    }

    const ULONG rowBytes = Width * 4u;

    /*
     * Align pitch conservatively. Many Windows display paths assume the primary
     * pitch is at least DWORD-aligned; we align further to 256B to avoid
     * pathological unaligned pitches.
     */
    ULONG pitch = 0;
    if (!AeroGpuSafeAlignUpU32(rowBytes, 256u, &pitch)) {
        /* Fallback: at least 4-byte alignment. */
        if (!AeroGpuSafeAlignUpU32(rowBytes, 4u, &pitch)) {
            return FALSE;
        }
    }

    *PitchBytes = pitch;
    return TRUE;
}

static __forceinline ULONG AeroGpuComputeVblankLineCountForActiveHeight(_In_ ULONG ActiveHeight)
{
    /*
     * Mirror the heuristic used by AeroGpuDdiGetScanLine so the mode timing we
     * advertise (VideoSignalInfo.TotalSize) is consistent with the scanline/vblank
     * numbers we report back to dxgkrnl.
     */
    const ULONG height = ActiveHeight ? ActiveHeight : 1u;
    ULONG vblankLines = height / 20;
    if (vblankLines < 20) {
        vblankLines = 20;
    }
    if (vblankLines > 40) {
        vblankLines = 40;
    }
    return vblankLines;
}

static __forceinline ULONG AeroGpuComputeHblankPixelCountForActiveWidth(_In_ ULONG ActiveWidth)
{
    /*
     * Conservative synthetic horizontal blanking.
     *
     * We do not model real detailed timing descriptors today, but returning
     * TotalSize.cx == ActiveSize.cx (i.e. 0 horizontal blanking) can confuse
     * parts of the Win7 display stack that expect some blanking interval.
     *
     * Use a simple heuristic tuned to produce plausible CVT-like totals for
     * common desktop modes.
     */
    const ULONG w = ActiveWidth ? ActiveWidth : 1u;
    ULONG hblank = w / 4;
    if (hblank < 8u) {
        hblank = 8u;
    }
    if (hblank > 320u) {
        hblank = 320u;
    }
    return hblank;
}

static __forceinline ULONG AeroGpuComputeTotalWidthForActiveWidth(_In_ ULONG ActiveWidth)
{
    const ULONG blank = AeroGpuComputeHblankPixelCountForActiveWidth(ActiveWidth);
    if (ActiveWidth > (0xFFFFFFFFu - blank)) {
        return ActiveWidth;
    }
    return ActiveWidth + blank;
}

static VOID AeroGpuLoadDisplayModeConfigFromRegistry(_In_opt_ PUNICODE_STRING RegistryPath)
{
    g_AeroGpuDisplayModeConfig.PreferredWidth = 0;
    g_AeroGpuDisplayModeConfig.PreferredHeight = 0;
    g_AeroGpuDisplayModeConfig.MaxWidth = 0;
    g_AeroGpuDisplayModeConfig.MaxHeight = 0;

    g_AeroGpuEnableReadGpaEscape = 0;
    g_AeroGpuEnableMapSharedHandleEscape = 0;

    if (!RegistryPath || !RegistryPath->Buffer || RegistryPath->Length == 0) {
        return;
    }

    /*
     * We read from the service key's `Parameters` subkey:
     *   HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters
     */
    static const WCHAR kSuffix[] = L"\\Parameters";
    const USHORT suffixBytes = (USHORT)sizeof(kSuffix); /* includes NUL */

    const USHORT baseBytes = RegistryPath->Length;
    const USHORT allocBytes = (USHORT)(baseBytes + suffixBytes);

    PWCHAR path = (PWCHAR)ExAllocatePoolWithTag(PagedPool, allocBytes, AEROGPU_POOL_TAG);
    if (!path) {
        return;
    }

    RtlCopyMemory(path, RegistryPath->Buffer, baseBytes);
    RtlCopyMemory((PUCHAR)path + baseBytes, kSuffix, suffixBytes);

    ULONG prefW = 0;
    ULONG prefH = 0;
    ULONG maxW = 0;
    ULONG maxH = 0;
    ULONG enableReadGpa = 0;
    ULONG enableMapSharedHandle = 0;

    RTL_QUERY_REGISTRY_TABLE table[7];
    RtlZeroMemory(table, sizeof(table));

    table[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[0].Name = L"PreferredWidth";
    table[0].EntryContext = &prefW;

    table[1].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[1].Name = L"PreferredHeight";
    table[1].EntryContext = &prefH;

    table[2].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[2].Name = L"MaxWidth";
    table[2].EntryContext = &maxW;

    table[3].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[3].Name = L"MaxHeight";
    table[3].EntryContext = &maxH;

    table[4].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[4].Name = L"EnableReadGpaEscape";
    table[4].EntryContext = &enableReadGpa;

    table[5].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[5].Name = L"EnableMapSharedHandleEscape";
    table[5].EntryContext = &enableMapSharedHandle;

    (void)RtlQueryRegistryValues(RTL_QUERY_REGISTRY_ABSOLUTE, path, table, NULL, NULL);

    ExFreePoolWithTag(path, AEROGPU_POOL_TAG);

    /* Sanitize: treat partial preferred overrides as unset. */
    if (prefW == 0 || prefH == 0) {
        prefW = 0;
        prefH = 0;
    }

    /*
     * Apply basic plausibility limits (avoid absurd allocations on bring-up).
     * 16384 is well above any mode we expose today.
     */
    if (prefW > 16384u || prefH > 16384u) {
        prefW = 0;
        prefH = 0;
    }
    if (maxW > 16384u) {
        maxW = 0;
    }
    if (maxH > 16384u) {
        maxH = 0;
    }

    g_AeroGpuDisplayModeConfig.PreferredWidth = prefW;
    g_AeroGpuDisplayModeConfig.PreferredHeight = prefH;
    g_AeroGpuDisplayModeConfig.MaxWidth = maxW;
    g_AeroGpuDisplayModeConfig.MaxHeight = maxH;

    g_AeroGpuEnableReadGpaEscape = (enableReadGpa != 0) ? 1u : 0u;
    g_AeroGpuEnableMapSharedHandleEscape = (enableMapSharedHandle != 0) ? 1u : 0u;

#if DBG
    if (prefW != 0 || prefH != 0 || maxW != 0 || maxH != 0) {
        AEROGPU_LOG("display config: Preferred=%lux%lu Max=%lux%lu", prefW, prefH, maxW, maxH);
    }
    if (g_AeroGpuEnableReadGpaEscape != 0 || g_AeroGpuEnableMapSharedHandleEscape != 0) {
        AEROGPU_LOG("dbgctl escape config: EnableReadGpaEscape=%lu EnableMapSharedHandleEscape=%lu",
                    g_AeroGpuEnableReadGpaEscape,
                    g_AeroGpuEnableMapSharedHandleEscape);
    }
#endif
}

static VOID AeroGpuLoadSubmitLimitsFromRegistry(_In_opt_ PUNICODE_STRING RegistryPath)
{
    g_AeroGpuMaxDmaBufferBytes = AEROGPU_KMD_MAX_DMA_BUFFER_BYTES;

    if (!RegistryPath || !RegistryPath->Buffer || RegistryPath->Length == 0) {
        return;
    }

    /*
     * We read from the service key's `Parameters` subkey:
     *   HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters
     */
    static const WCHAR kSuffix[] = L"\\Parameters";
    const USHORT suffixBytes = (USHORT)sizeof(kSuffix); /* includes NUL */

    const USHORT baseBytes = RegistryPath->Length;
    const USHORT allocBytes = (USHORT)(baseBytes + suffixBytes);

    PWCHAR path = (PWCHAR)ExAllocatePoolWithTag(PagedPool, allocBytes, AEROGPU_POOL_TAG);
    if (!path) {
        return;
    }

    RtlCopyMemory(path, RegistryPath->Buffer, baseBytes);
    RtlCopyMemory((PUCHAR)path + baseBytes, kSuffix, suffixBytes);

    ULONG maxDmaBytes = 0;

    RTL_QUERY_REGISTRY_TABLE table[2];
    RtlZeroMemory(table, sizeof(table));

    table[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[0].Name = L"MaxDmaBufferBytes";
    table[0].EntryContext = &maxDmaBytes;

    (void)RtlQueryRegistryValues(RTL_QUERY_REGISTRY_ABSOLUTE, path, table, NULL, NULL);

    ExFreePoolWithTag(path, AEROGPU_POOL_TAG);

    if (maxDmaBytes != 0) {
        if (maxDmaBytes < AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MIN) {
            maxDmaBytes = AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MIN;
        } else if (maxDmaBytes > AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MAX) {
            maxDmaBytes = AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MAX;
        }
        g_AeroGpuMaxDmaBufferBytes = maxDmaBytes;
    }

#if DBG
    if (g_AeroGpuMaxDmaBufferBytes != AEROGPU_KMD_MAX_DMA_BUFFER_BYTES) {
        AEROGPU_LOG("submit limits: MaxDmaBufferBytes=%lu (default=%lu)",
                    g_AeroGpuMaxDmaBufferBytes,
                    (ULONG)AEROGPU_KMD_MAX_DMA_BUFFER_BYTES);
    }
#endif
}

/* ---- DMA buffer private data plumbing ---------------------------------- */

static VOID AeroGpuFreeSubmissionMeta(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ AEROGPU_SUBMISSION_META* Meta);

static __forceinline ULONGLONG AeroGpuSubmissionMetaTotalBytes(_In_opt_ const AEROGPU_SUBMISSION_META* Meta)
{
    if (!Meta) {
        return 0;
    }

    /*
     * Meta is allocated as:
     *   FIELD_OFFSET(AEROGPU_SUBMISSION_META, Allocations) +
     *   (AllocationCount * sizeof(aerogpu_legacy_submission_desc_allocation))
     *
     * Track both the pool allocation and any associated allocation table memory.
     */
    SIZE_T allocBytes = 0;
    NTSTATUS st = RtlSizeTMult((SIZE_T)Meta->AllocationCount, sizeof(aerogpu_legacy_submission_desc_allocation), &allocBytes);
    if (!NT_SUCCESS(st)) {
        return 0xFFFFFFFFFFFFFFFFull;
    }

    SIZE_T metaBytes = 0;
    st = RtlSizeTAdd(FIELD_OFFSET(AEROGPU_SUBMISSION_META, Allocations), allocBytes, &metaBytes);
    if (!NT_SUCCESS(st)) {
        return 0xFFFFFFFFFFFFFFFFull;
    }

    ULONGLONG totalBytes = (ULONGLONG)metaBytes;
    const ULONGLONG tableBytes = (ULONGLONG)Meta->AllocTableSizeBytes;
    if (totalBytes > (0xFFFFFFFFFFFFFFFFull - tableBytes)) {
        return 0xFFFFFFFFFFFFFFFFull;
    }
    totalBytes += tableBytes;
    return totalBytes;
}

static __forceinline BOOLEAN AeroGpuMetaHandleAtCapacity(_Inout_ AEROGPU_ADAPTER* Adapter,
                                                         _Out_opt_ ULONG* PendingCountOut,
                                                         _Out_opt_ ULONGLONG* PendingBytesOut)
{
    if (PendingCountOut) {
        *PendingCountOut = 0;
    }
    if (PendingBytesOut) {
        *PendingBytesOut = 0;
    }
    if (!Adapter) {
        return TRUE;
    }

    ULONG count = 0;
    ULONGLONG bytes = 0;
    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);
    count = Adapter->PendingMetaHandleCount;
    bytes = Adapter->PendingMetaHandleBytes;
    KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

    if (PendingCountOut) {
        *PendingCountOut = count;
    }
    if (PendingBytesOut) {
        *PendingBytesOut = bytes;
    }
    return (count >= AEROGPU_PENDING_META_HANDLES_MAX_COUNT) || (bytes >= AEROGPU_PENDING_META_HANDLES_MAX_BYTES);
}

static NTSTATUS AeroGpuMetaHandleStore(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ AEROGPU_SUBMISSION_META* Meta, _Out_ ULONGLONG* HandleOut)
{
    *HandleOut = 0;

    const ULONGLONG metaBytes = AeroGpuSubmissionMetaTotalBytes(Meta);

    AEROGPU_META_HANDLE_ENTRY* entry =
        (AEROGPU_META_HANDLE_ENTRY*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*entry), AEROGPU_POOL_TAG);
    if (!entry) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(entry, sizeof(*entry));
    entry->Meta = Meta;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);

    const ULONG pendingCount = Adapter->PendingMetaHandleCount;
    const ULONGLONG pendingBytes = Adapter->PendingMetaHandleBytes;
    const BOOLEAN overCount = pendingCount >= AEROGPU_PENDING_META_HANDLES_MAX_COUNT;
    const BOOLEAN overBytes =
        (metaBytes > AEROGPU_PENDING_META_HANDLES_MAX_BYTES) ||
        (pendingBytes > (AEROGPU_PENDING_META_HANDLES_MAX_BYTES - metaBytes));
    if (overCount || overBytes) {
        KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);
        ExFreePoolWithTag(entry, AEROGPU_POOL_TAG);
#if DBG
        AEROGPU_LOG_RATELIMITED(g_PendingMetaHandleCapLogCount,
                                8,
                                "MetaHandleStore: pending meta handle cap hit (count=%lu/%lu bytes=%I64u/%I64u meta_bytes=%I64u)",
                                pendingCount,
                                (ULONG)AEROGPU_PENDING_META_HANDLES_MAX_COUNT,
                                pendingBytes,
                                (ULONGLONG)AEROGPU_PENDING_META_HANDLES_MAX_BYTES,
                                metaBytes);
#endif
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    /* 0 is reserved to mean "no meta". */
    ULONGLONG handle = ++Adapter->NextMetaHandle;
    if (handle == 0) {
        handle = ++Adapter->NextMetaHandle;
    }

    entry->Handle = handle;
    InsertTailList(&Adapter->PendingMetaHandles, &entry->ListEntry);
    Adapter->PendingMetaHandleCount += 1;
    Adapter->PendingMetaHandleBytes += metaBytes;

    KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

    *HandleOut = handle;
    return STATUS_SUCCESS;
}

static AEROGPU_SUBMISSION_META* AeroGpuMetaHandleTake(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG Handle)
{
    if (Handle == 0) {
        return NULL;
    }

    AEROGPU_META_HANDLE_ENTRY* found = NULL;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);

    for (PLIST_ENTRY it = Adapter->PendingMetaHandles.Flink; it != &Adapter->PendingMetaHandles; it = it->Flink) {
        AEROGPU_META_HANDLE_ENTRY* entry = CONTAINING_RECORD(it, AEROGPU_META_HANDLE_ENTRY, ListEntry);
        if (entry->Handle == Handle) {
            found = entry;
            RemoveEntryList(&entry->ListEntry);
            if (Adapter->PendingMetaHandleCount != 0) {
                Adapter->PendingMetaHandleCount -= 1;
            }
            const ULONGLONG bytes = AeroGpuSubmissionMetaTotalBytes(entry->Meta);
            if (Adapter->PendingMetaHandleBytes >= bytes) {
                Adapter->PendingMetaHandleBytes -= bytes;
            } else {
                Adapter->PendingMetaHandleBytes = 0;
            }
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

    if (!found) {
        return NULL;
    }

    AEROGPU_SUBMISSION_META* meta = found->Meta;
    ExFreePoolWithTag(found, AEROGPU_POOL_TAG);
    return meta;
}

static VOID AeroGpuMetaHandleFreeAll(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    for (;;) {
        AEROGPU_META_HANDLE_ENTRY* entry = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);
        if (!IsListEmpty(&Adapter->PendingMetaHandles)) {
            PLIST_ENTRY le = RemoveHeadList(&Adapter->PendingMetaHandles);
            entry = CONTAINING_RECORD(le, AEROGPU_META_HANDLE_ENTRY, ListEntry);
            if (Adapter->PendingMetaHandleCount != 0) {
                Adapter->PendingMetaHandleCount -= 1;
            }
            const ULONGLONG bytes = AeroGpuSubmissionMetaTotalBytes(entry->Meta);
            if (Adapter->PendingMetaHandleBytes >= bytes) {
                Adapter->PendingMetaHandleBytes -= bytes;
            } else {
                Adapter->PendingMetaHandleBytes = 0;
            }
        }
        KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);

        if (!entry) {
            break;
        }

        AeroGpuFreeSubmissionMeta(Adapter, entry->Meta);
        ExFreePoolWithTag(entry, AEROGPU_POOL_TAG);
    }

    /* Keep teardown idempotent and leave the adapter in a clean state. */
    {
        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->MetaHandleLock, &oldIrql);
        Adapter->PendingMetaHandleCount = 0;
        Adapter->PendingMetaHandleBytes = 0;
        InitializeListHead(&Adapter->PendingMetaHandles);
        KeReleaseSpinLock(&Adapter->MetaHandleLock, oldIrql);
    }
}

typedef enum _AEROGPU_INTERNAL_SUBMISSION_KIND {
    AEROGPU_INTERNAL_SUBMISSION_KIND_UNKNOWN = 0,
    AEROGPU_INTERNAL_SUBMISSION_KIND_RELEASE_SHARED_SURFACE = 1,
    AEROGPU_INTERNAL_SUBMISSION_KIND_SELFTEST = 2,
} AEROGPU_INTERNAL_SUBMISSION_KIND;
typedef struct _AEROGPU_PENDING_INTERNAL_SUBMISSION {
    LIST_ENTRY ListEntry;
    ULONG RingTailAfter;
    ULONG Kind; /* enum AEROGPU_INTERNAL_SUBMISSION_KIND */
    ULONGLONG ShareToken;
    PVOID CmdVa;
    SIZE_T CmdSizeBytes;
    PVOID DescVa;       /* legacy submission descriptor (optional) */
    SIZE_T DescSizeBytes;
} AEROGPU_PENDING_INTERNAL_SUBMISSION;

static __forceinline AEROGPU_PENDING_INTERNAL_SUBMISSION*
AeroGpuAllocPendingInternalSubmission(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return NULL;
    }

    AEROGPU_PENDING_INTERNAL_SUBMISSION* sub = (AEROGPU_PENDING_INTERNAL_SUBMISSION*)ExAllocateFromNPagedLookasideList(
        &Adapter->PendingInternalSubmissionLookaside);
    if (!sub) {
        return NULL;
    }

    RtlZeroMemory(sub, sizeof(*sub));
    return sub;
}

static __forceinline VOID AeroGpuFreePendingInternalSubmission(_Inout_ AEROGPU_ADAPTER* Adapter,
                                                               _In_opt_ AEROGPU_PENDING_INTERNAL_SUBMISSION* Sub)
{
    if (!Adapter || !Sub) {
        return;
    }

    ExFreeToNPagedLookasideList(&Adapter->PendingInternalSubmissionLookaside, Sub);
}

static VOID AeroGpuFreeSharedHandleTokens(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

    for (;;) {
        AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* node = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->SharedHandleTokenLock, &oldIrql);
        if (!IsListEmpty(&Adapter->SharedHandleTokens)) {
            PLIST_ENTRY entry = RemoveHeadList(&Adapter->SharedHandleTokens);
            node = CONTAINING_RECORD(entry, AEROGPU_SHARED_HANDLE_TOKEN_ENTRY, ListEntry);
            if (Adapter->SharedHandleTokenCount != 0) {
                Adapter->SharedHandleTokenCount--;
            }
        }
        KeReleaseSpinLock(&Adapter->SharedHandleTokenLock, oldIrql);

        if (!node) {
            break;
        }

        if (node->Object) {
            ObDereferenceObject(node->Object);
        }
        ExFreePoolWithTag(node, AEROGPU_POOL_TAG);
    }

    /* Keep teardown idempotent and leave the adapter in a clean state. */
    {
        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->SharedHandleTokenLock, &oldIrql);
        Adapter->SharedHandleTokenCount = 0;
        Adapter->NextSharedHandleToken = 0;
        InitializeListHead(&Adapter->SharedHandleTokens);
        KeReleaseSpinLock(&Adapter->SharedHandleTokenLock, oldIrql);
    }
}

/* ---- Helpers ------------------------------------------------------------ */

/*
 * Atomic helpers for shared 64-bit state.
 *
 * Defined below; forward-declared here so early helpers (e.g. completed fence
 * reads) can use the shared abstraction.
 */
static __forceinline ULONGLONG AeroGpuAtomicReadU64(_In_ const volatile ULONGLONG* Value);
static __forceinline VOID AeroGpuAtomicWriteU64(_Inout_ volatile ULONGLONG* Value, _In_ ULONGLONG NewValue);
static __forceinline ULONGLONG AeroGpuAtomicExchangeU64(_Inout_ volatile ULONGLONG* Value, _In_ ULONGLONG NewValue);
static __forceinline ULONGLONG AeroGpuAtomicCompareExchangeU64(_Inout_ volatile ULONGLONG* Value,
                                                               _In_ ULONGLONG NewValue,
                                                               _In_ ULONGLONG ExpectedValue);
static __forceinline ULONG AeroGpuAtomicReadU32(_In_ volatile ULONG* Value);

/*
 * Read a 64-bit MMIO value exposed as two 32-bit registers in LO/HI form.
 *
 * Use an HI/LO/HI pattern to avoid tearing if the device updates the value
 * concurrently.
 */
static ULONGLONG AeroGpuReadRegU64HiLoHi(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG LoOffset, _In_ ULONG HiOffset)
{
    ULONG hi = AeroGpuReadRegU32(Adapter, HiOffset);
    for (ULONG i = 0; i < 16; ++i) {
        const ULONG lo = AeroGpuReadRegU32(Adapter, LoOffset);
        const ULONG hi2 = AeroGpuReadRegU32(Adapter, HiOffset);
        if (hi == hi2) {
            return ((ULONGLONG)hi << 32) | (ULONGLONG)lo;
        }
        hi = hi2;
    }

    /* Best-effort: avoid an infinite loop if the device is misbehaving. */
    return ((ULONGLONG)hi << 32) | (ULONGLONG)AeroGpuReadRegU32(Adapter, LoOffset);
}

static ULONGLONG AeroGpuReadVolatileU64HiLoHi(_In_ const volatile ULONG* LoAddr)
{
    ULONG hi = LoAddr[1];
    for (ULONG i = 0; i < 16; ++i) {
        const ULONG lo = LoAddr[0];
        const ULONG hi2 = LoAddr[1];
        if (hi == hi2) {
            return ((ULONGLONG)hi << 32) | (ULONGLONG)lo;
        }
        hi = hi2;
    }

    /* Best-effort: avoid an infinite loop if the device is misbehaving. */
    return ((ULONGLONG)hi << 32) | (ULONGLONG)LoAddr[0];
}

static __forceinline BOOLEAN AeroGpuMmioSafeNow(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter || !Adapter->Bar0) {
        return FALSE;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
        DxgkDevicePowerStateD0) {
        return FALSE;
    }
    if (InterlockedCompareExchange((volatile LONG*)&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return FALSE;
    }
    return TRUE;
}

static ULONGLONG AeroGpuReadCompletedFence(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return 0;
    }

    const ULONGLONG cachedLastCompleted = AeroGpuAtomicReadU64(&Adapter->LastCompletedFence);

    /*
     * Avoid touching device-backed state (including the optional shared fence page) while the adapter
     * is leaving D0 / submissions are blocked.
     *
     * This prevents races with teardown paths (StopDevice) that can detach/free the fence page while
     * threads are still polling for completion (e.g. DxgkDdiLock CPU mapping paths).
     */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange((volatile LONG*)&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return cachedLastCompleted;
    }

    /*
     * If a shared fence page is configured, prefer reading it. This is always a
     * normal system-memory read (no MMIO), but still require the adapter to be
     * in a stable D0/submission-ready state to avoid racing teardown paths that
     * can detach/free the page.
     *
     * Clamp to the KMD's cached LastCompletedFence to avoid returning a value
     * that appears to go backwards (for example, if the device resets the fence
     * page while powered down/resuming).
     */
    if (Adapter->AbiKind == AEROGPU_ABI_KIND_V1 && Adapter->FencePageVa) {
        /*
         * The fence page can be detached/freed during teardown via AeroGpuRingCleanup. Hold RingLock
         * at <= DISPATCH_LEVEL to avoid racing cleanup. At DIRQL (ISR context) we cannot take the
         * lock; rely on the D0/AcceptingSubmissions gate above (StopDevice transitions block
         * submissions before freeing the page).
         */
        struct aerogpu_fence_page* fencePage = NULL;
        ULONGLONG fence = 0;

        if (KeGetCurrentIrql() <= DISPATCH_LEVEL) {
            KIRQL ringIrql;
            KeAcquireSpinLock(&((AEROGPU_ADAPTER*)Adapter)->RingLock, &ringIrql);
            fencePage = ((AEROGPU_ADAPTER*)Adapter)->FencePageVa;
            if (fencePage) {
                const volatile ULONG* parts = (const volatile ULONG*)&fencePage->completed_fence;
                fence = AeroGpuReadVolatileU64HiLoHi(parts);
            }
            KeReleaseSpinLock(&((AEROGPU_ADAPTER*)Adapter)->RingLock, ringIrql);
        } else {
            fencePage = ((AEROGPU_ADAPTER*)Adapter)->FencePageVa;
            if (fencePage) {
                const volatile ULONG* parts = (const volatile ULONG*)&fencePage->completed_fence;
                fence = AeroGpuReadVolatileU64HiLoHi(parts);
            }
        }

        if (fencePage) {
            if (fence < cachedLastCompleted) {
                fence = cachedLastCompleted;
            }
            return fence;
        }
    }

    if (!Adapter->Bar0) {
        return cachedLastCompleted;
    }

    /* Re-check the power/submission gate before touching MMIO (teardown races). */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange((volatile LONG*)&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return cachedLastCompleted;
    }

    if (Adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        return (ULONGLONG)AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_FENCE_COMPLETED);
    }

    return AeroGpuReadRegU64HiLoHi(Adapter, AEROGPU_MMIO_REG_COMPLETED_FENCE_LO, AEROGPU_MMIO_REG_COMPLETED_FENCE_HI);
}

static BOOLEAN AeroGpuTryReadErrorFence64(_In_ const AEROGPU_ADAPTER* Adapter, _Out_ ULONGLONG* FenceOut)
{
    if (FenceOut) {
        *FenceOut = 0;
    }
    if (!Adapter || !FenceOut || !Adapter->Bar0) {
        return FALSE;
    }

    /*
     * Avoid MMIO reads while the adapter is not in D0 or submissions are blocked
     * (resume/teardown windows). In these states the BAR mapping may still exist,
     * but the device can be inaccessible or MMIO state may be unstable.
     */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange((volatile LONG*)&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return FALSE;
    }

    /*
     * Error registers are part of the versioned (AGPU) ABI v1.3+ contract.
     */
    if (Adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        return FALSE;
    }
    if ((Adapter->DeviceFeatures & AEROGPU_FEATURE_ERROR_INFO) == 0) {
        return FALSE;
    }
    const ULONG abiMinor = (ULONG)(Adapter->DeviceAbiVersion & 0xFFFFu);
    if (abiMinor < 3) {
        return FALSE;
    }
    if (Adapter->Bar0Length < (AEROGPU_MMIO_REG_ERROR_COUNT + sizeof(ULONG))) {
        return FALSE;
    }

    const ULONGLONG fence =
        AeroGpuReadRegU64HiLoHi(Adapter, AEROGPU_MMIO_REG_ERROR_FENCE_LO, AEROGPU_MMIO_REG_ERROR_FENCE_HI);
    if (fence == 0) {
        return FALSE;
    }
    *FenceOut = fence;
    return TRUE;
}

/*
 * Atomic helpers for shared 64-bit state.
 *
 * Important: This driver is built for both x86 and x64. On x86, plain 64-bit
 * loads/stores are not atomic and can tear. Fence state is accessed across
 * multiple contexts (submit thread, ISR/DPC, dbgctl escapes), so all cross-thread
 * accesses must use Interlocked*64 operations (or be protected by a lock on all
 * paths).
 *
 * Interlocked*64 requires 8-byte alignment for its target address; fence fields
 * are declared with DECLSPEC_ALIGN(8) in AEROGPU_ADAPTER.
 */
static __forceinline ULONGLONG AeroGpuAtomicReadU64(_In_ const volatile ULONGLONG* Value)
{
#if DBG
    ASSERT(((ULONG_PTR)Value & 7u) == 0);
#endif
    return (ULONGLONG)InterlockedCompareExchange64((volatile LONGLONG*)Value, 0, 0);
}

static __forceinline VOID AeroGpuAtomicWriteU64(_Inout_ volatile ULONGLONG* Value, _In_ ULONGLONG NewValue)
{
#if DBG
    ASSERT(((ULONG_PTR)Value & 7u) == 0);
#endif
    InterlockedExchange64((volatile LONGLONG*)Value, (LONGLONG)NewValue);
}

static __forceinline ULONGLONG AeroGpuAtomicExchangeU64(_Inout_ volatile ULONGLONG* Value, _In_ ULONGLONG NewValue)
{
#if DBG
    ASSERT(((ULONG_PTR)Value & 7u) == 0);
#endif
    return (ULONGLONG)InterlockedExchange64((volatile LONGLONG*)Value, (LONGLONG)NewValue);
}

static __forceinline ULONGLONG AeroGpuAtomicCompareExchangeU64(_Inout_ volatile ULONGLONG* Value,
                                                               _In_ ULONGLONG NewValue,
                                                               _In_ ULONGLONG ExpectedValue)
{
#if DBG
    ASSERT(((ULONG_PTR)Value & 7u) == 0);
#endif
    return (ULONGLONG)InterlockedCompareExchange64((volatile LONGLONG*)Value, (LONGLONG)NewValue, (LONGLONG)ExpectedValue);
}

static __forceinline ULONG AeroGpuAtomicReadU32(_In_ volatile ULONG* Value)
{
    return (ULONG)InterlockedCompareExchange((volatile LONG*)Value, 0, 0);
}

/*
 * Extend Win7/WDDM 1.1 32-bit DMA fences into the AeroGPU v1 protocol's required
 * monotonic 64-bit fence domain.
 *
 * Must be called with Adapter->PendingLock held so submissions cannot race and
 * observe inconsistent epoch transitions.
 */
static __forceinline ULONGLONG AeroGpuV1ExtendFenceLocked(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG Fence32)
{
    if (!Adapter) {
        return (ULONGLONG)Fence32;
    }

    if (Fence32 < Adapter->V1LastFence32) {
        Adapter->V1FenceEpoch += 1;
    }
    Adapter->V1LastFence32 = Fence32;

    return (((ULONGLONG)Adapter->V1FenceEpoch) << 32) | (ULONGLONG)Fence32;
}

static const char* AeroGpuErrorCodeName(_In_ ULONG Code)
{
    switch (Code) {
        case AEROGPU_ERROR_NONE:
            return "NONE";
        case AEROGPU_ERROR_CMD_DECODE:
            return "CMD_DECODE";
        case AEROGPU_ERROR_OOB:
            return "OOB";
        case AEROGPU_ERROR_BACKEND:
            return "BACKEND";
        case AEROGPU_ERROR_INTERNAL:
            return "INTERNAL";
        default:
            break;
    }
    return "UNKNOWN";
}

static __forceinline BOOLEAN AeroGpuIsDeviceErrorLatched(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return FALSE;
    }
    return InterlockedCompareExchange((volatile LONG*)&Adapter->DeviceErrorLatched, 0, 0) != 0;
}

static VOID AeroGpuLogSubmission(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG Fence, _In_ ULONG Type, _In_ ULONG DmaSize)
{
    ULONG idx = Adapter->SubmissionLog.WriteIndex++ % AEROGPU_SUBMISSION_LOG_SIZE;
    Adapter->SubmissionLog.Entries[idx].Fence = Fence;
    Adapter->SubmissionLog.Entries[idx].Type = Type;
    Adapter->SubmissionLog.Entries[idx].DmaSize = DmaSize;
    Adapter->SubmissionLog.Entries[idx].Qpc = KeQueryPerformanceCounter(NULL);
}

static VOID AeroGpuTraceCreateAllocation(_Inout_ AEROGPU_ADAPTER* Adapter,
                                        _In_ ULONG CallSeq,
                                        _In_ ULONG AllocIndex,
                                        _In_ ULONG NumAllocations,
                                        _In_ ULONG CreateFlags,
                                        _In_ ULONG AllocationId,
                                        _In_ ULONGLONG ShareToken,
                                        _In_ ULONGLONG SizeBytes,
                                        _In_ ULONG FlagsIn,
                                        _In_ ULONG FlagsOut,
                                        _In_ ULONG PrivFlags,
                                        _In_ ULONG PitchBytes)
{
    if (!Adapter) {
        return;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->CreateAllocationTraceLock, &oldIrql);

    const ULONG seq = Adapter->CreateAllocationTrace.WriteIndex++;
    const ULONG slot = seq % AEROGPU_CREATEALLOCATION_TRACE_SIZE;
    AEROGPU_CREATEALLOCATION_TRACE_ENTRY* e = &Adapter->CreateAllocationTrace.Entries[slot];
    e->Seq = seq;
    e->CallSeq = CallSeq;
    e->AllocIndex = AllocIndex;
    e->NumAllocations = NumAllocations;
    e->CreateFlags = CreateFlags;
    e->AllocationId = AllocationId;
    e->ShareToken = ShareToken;
    e->SizeBytes = SizeBytes;
    e->FlagsIn = FlagsIn;
    e->FlagsOut = FlagsOut;
    e->PrivFlags = PrivFlags;
    e->PitchBytes = PitchBytes;

    KeReleaseSpinLock(&Adapter->CreateAllocationTraceLock, oldIrql);
}

#if defined(_WIN64)
#define AEROGPU_CONTIG_POOL_RETENTION_CAP_BYTES (16ull * 1024ull * 1024ull) /* 16 MiB */
#else
#define AEROGPU_CONTIG_POOL_RETENTION_CAP_BYTES (8ull * 1024ull * 1024ull) /* 8 MiB */
#endif
/* Bound the number of cached buffers per size class to avoid long free lists of tiny allocations. */
#define AEROGPU_CONTIG_POOL_MAX_ENTRIES_PER_CLASS 16u

static __forceinline BOOLEAN AeroGpuContigPoolClassForSize(_In_ SIZE_T Size, _Out_ UINT* ClassIndexOut, _Out_ SIZE_T* AllocSizeOut)
{
    if (!ClassIndexOut || !AllocSizeOut) {
        return FALSE;
    }
    *ClassIndexOut = 0;
    *AllocSizeOut = 0;

    if (Size == 0) {
        return FALSE;
    }

    /* Pool only up to a bounded size to avoid pinning too much contiguous memory. */
    if (Size > ((SIZE_T)AEROGPU_CONTIG_POOL_MAX_PAGES * (SIZE_T)PAGE_SIZE)) {
        return FALSE;
    }

    /*
     * Size classes are whole pages (1..AEROGPU_CONTIG_POOL_MAX_PAGES).
     * This avoids requesting more contiguous pages than the OS would allocate anyway.
     */
    if (Size > ((SIZE_T)-1) - ((SIZE_T)PAGE_SIZE - 1)) {
        return FALSE;
    }

    const SIZE_T pages = (Size + (SIZE_T)PAGE_SIZE - 1) / (SIZE_T)PAGE_SIZE;
    if (pages == 0 || pages > (SIZE_T)AEROGPU_CONTIG_POOL_MAX_PAGES) {
        return FALSE;
    }

    *ClassIndexOut = (UINT)(pages - 1);
    *AllocSizeOut = pages * (SIZE_T)PAGE_SIZE;
    return TRUE;
}

static __forceinline BOOLEAN AeroGpuRoundUpToPageSize(_In_ SIZE_T Size, _Out_ SIZE_T* RoundedOut)
{
    if (!RoundedOut) {
        return FALSE;
    }
    *RoundedOut = 0;

    if (Size == 0) {
        return FALSE;
    }

    if (Size > ((SIZE_T)-1) - ((SIZE_T)PAGE_SIZE - 1)) {
        return FALSE;
    }

    const SIZE_T rounded = (Size + (SIZE_T)PAGE_SIZE - 1) & ~((SIZE_T)PAGE_SIZE - 1);
    if (rounded == 0) {
        return FALSE;
    }
    *RoundedOut = rounded;
    return TRUE;
}

static VOID AeroGpuContigPoolInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

    KeInitializeSpinLock(&Adapter->ContigPool.Lock);
    for (UINT i = 0; i < AEROGPU_CONTIG_POOL_MAX_PAGES; ++i) {
        InitializeListHead(&Adapter->ContigPool.FreeLists[i]);
        Adapter->ContigPool.FreeCounts[i] = 0;
    }
    Adapter->ContigPool.BytesRetained = 0;
}

static VOID AeroGpuContigPoolPurge(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

#if DBG
    const LONGLONG hits = InterlockedCompareExchange64(&Adapter->ContigPool.Hits, 0, 0);
    const LONGLONG misses = InterlockedCompareExchange64(&Adapter->ContigPool.Misses, 0, 0);
    const LONGLONG freesToPool = InterlockedCompareExchange64(&Adapter->ContigPool.FreesToPool, 0, 0);
    const LONGLONG freesToOs = InterlockedCompareExchange64(&Adapter->ContigPool.FreesToOs, 0, 0);
    const LONGLONG osAllocs = InterlockedCompareExchange64(&Adapter->ContigPool.OsAllocs, 0, 0);
    const LONGLONG osAllocBytes = InterlockedCompareExchange64(&Adapter->ContigPool.OsAllocBytes, 0, 0);
    const LONGLONG osFrees = InterlockedCompareExchange64(&Adapter->ContigPool.OsFrees, 0, 0);
    const LONGLONG osFreeBytes = InterlockedCompareExchange64(&Adapter->ContigPool.OsFreeBytes, 0, 0);
    const LONGLONG hiWater = InterlockedCompareExchange64(&Adapter->ContigPool.HighWatermarkBytes, 0, 0);

    AEROGPU_LOG("ContigPool: hits=%I64d misses=%I64d retained=%Iu cap=%I64u frees_to_pool=%I64d frees_to_os=%I64d os_allocs=%I64d os_alloc_bytes=%I64d os_frees=%I64d os_free_bytes=%I64d hiwater=%I64d",
                hits,
                misses,
                Adapter->ContigPool.BytesRetained,
                (ULONGLONG)AEROGPU_CONTIG_POOL_RETENTION_CAP_BYTES,
                freesToPool,
                freesToOs,
                osAllocs,
                osAllocBytes,
                osFrees,
                osFreeBytes,
                hiWater);
#endif

    for (UINT i = 0; i < AEROGPU_CONTIG_POOL_MAX_PAGES; ++i) {
        const SIZE_T allocSize = (SIZE_T)(i + 1) * (SIZE_T)PAGE_SIZE;
        for (;;) {
            PVOID va = NULL;
            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&Adapter->ContigPool.Lock, &oldIrql);
                if (!IsListEmpty(&Adapter->ContigPool.FreeLists[i])) {
                    PLIST_ENTRY entry = RemoveHeadList(&Adapter->ContigPool.FreeLists[i]);
                    va = (PVOID)entry;
                    if (Adapter->ContigPool.BytesRetained >= allocSize) {
                        Adapter->ContigPool.BytesRetained -= allocSize;
                    } else {
                        Adapter->ContigPool.BytesRetained = 0;
                    }
                    if (Adapter->ContigPool.FreeCounts[i] != 0) {
                        Adapter->ContigPool.FreeCounts[i] -= 1;
                    }
                } else {
                    /* Be defensive: keep count consistent with list emptiness. */
                    Adapter->ContigPool.FreeCounts[i] = 0;
                }
                KeReleaseSpinLock(&Adapter->ContigPool.Lock, oldIrql);
            }
            if (!va) {
                break;
            }
            MmFreeContiguousMemorySpecifyCache(va, allocSize, MmNonCached);

#if DBG
            InterlockedIncrement64(&Adapter->ContigPool.OsFrees);
            InterlockedAdd64(&Adapter->ContigPool.OsFreeBytes, (LONGLONG)allocSize);
#endif
        }
    }
}

/*
 * Allocate a physically contiguous non-cached buffer without initializing it.
 *
 * This must only be used for buffers that are guaranteed to be fully overwritten
 * (at least the requested [0, Size) range) before the device can observe them
 * (for example DMA copy buffers populated via a single RtlCopyMemory of Size
 * bytes).
 *
 * Note: when allocations are eligible for pooling, the underlying allocation is
 * page-rounded. The allocator clears the page-tail slack bytes beyond Size so no
 * stale kernel data is left in memory outside the requested range.
 */
static PVOID AeroGpuAllocContiguousNoInit(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ SIZE_T Size, _Out_ PHYSICAL_ADDRESS* Pa)
{
    if (Size == 0) {
        return NULL;
    }

    /*
     * Guard against pathological callers requesting extremely large contiguous
     * allocations. Even failed attempts can be expensive and may fragment
     * contiguous memory on some guests.
     *
     * Note: this cap is also enforced explicitly for DMA buffer submissions in
     * AeroGpuDdiSubmitCommand (with a more specific error code), but applying a
     * global limit here also protects other contiguous allocation sites (legacy
     * descriptors, alloc tables).
     */
    const SIZE_T maxBytes = (SIZE_T)g_AeroGpuMaxDmaBufferBytes;
    if (maxBytes != 0 && Size > maxBytes) {
#if DBG
        static volatile LONG g_AllocContigTooLargeLogCount = 0;
        AEROGPU_LOG_RATELIMITED(g_AllocContigTooLargeLogCount,
                                8,
                                "AllocContiguous: request too large: size=%I64u max=%I64u",
                                (ULONGLONG)Size,
                                (ULONGLONG)maxBytes);
#endif
        return NULL;
    }

    PHYSICAL_ADDRESS low;
    PHYSICAL_ADDRESS high;
    PHYSICAL_ADDRESS boundary;

    if (!Adapter || !Pa || Size == 0) {
        return NULL;
    }

    Pa->QuadPart = 0;

    low.QuadPart = 0;
    boundary.QuadPart = 0;
    high.QuadPart = ~0ULL;

    UINT classIndex = 0;
    SIZE_T allocSize = 0;
    const BOOLEAN poolEligible = AeroGpuContigPoolClassForSize(Size, &classIndex, &allocSize);

    SIZE_T requestBytes = 0;
    if (poolEligible) {
        requestBytes = allocSize;
    } else {
        /*
         * MmAllocateContiguousMemorySpecifyCache ultimately deals in pages. Always round up so our
         * alloc/free sizes match and we can deterministically clear any tail slack bytes.
         */
        if (!AeroGpuRoundUpToPageSize(Size, &requestBytes)) {
            return NULL;
        }
    }

    PVOID va = NULL;
    BOOLEAN poolHit = FALSE;
    if (poolEligible) {
        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->ContigPool.Lock, &oldIrql);
        if (!IsListEmpty(&Adapter->ContigPool.FreeLists[classIndex])) {
            va = (PVOID)RemoveHeadList(&Adapter->ContigPool.FreeLists[classIndex]);
            poolHit = TRUE;
            if (Adapter->ContigPool.BytesRetained >= allocSize) {
                Adapter->ContigPool.BytesRetained -= allocSize;
            } else {
                Adapter->ContigPool.BytesRetained = 0;
            }
            if (Adapter->ContigPool.FreeCounts[classIndex] != 0) {
                Adapter->ContigPool.FreeCounts[classIndex] -= 1;
            }
#if DBG
            InterlockedIncrement64(&Adapter->ContigPool.Hits);
#endif
        } else {
            /* Be defensive: keep per-class count consistent with list emptiness. */
            Adapter->ContigPool.FreeCounts[classIndex] = 0;
#if DBG
            InterlockedIncrement64(&Adapter->ContigPool.Misses);
#endif
        }
        KeReleaseSpinLock(&Adapter->ContigPool.Lock, oldIrql);
    }

    if (poolEligible) {
        if (poolHit) {
            InterlockedIncrement64(&Adapter->PerfContigPoolHit);
            InterlockedAdd64(&Adapter->PerfContigPoolBytesSaved, (LONGLONG)allocSize);
        } else {
            InterlockedIncrement64(&Adapter->PerfContigPoolMiss);
        }
    }

    if (!va) {
        va = MmAllocateContiguousMemorySpecifyCache(requestBytes, low, high, boundary, MmNonCached);
        if (va) {
#if DBG
            InterlockedIncrement64(&Adapter->ContigPool.OsAllocs);
            InterlockedAdd64(&Adapter->ContigPool.OsAllocBytes, (LONGLONG)requestBytes);
#endif
        }
    }
    if (!va) {
        return NULL;
    }

    /*
     * Contiguous allocations are page-rounded. Ensure the tail slack (bytes beyond the requested
     * Size) is zeroed so no stale kernel data is left in memory that might be observed by the
     * device (for example if a host-side implementation were to DMA whole pages).
     *
     * This preserves the "no-init" contract for [0, Size) while making the page tail deterministic.
     */
    if (requestBytes > Size) {
        RtlZeroMemory((PUCHAR)va + Size, requestBytes - Size);
    }

    *Pa = MmGetPhysicalAddress(va);
    return va;
}

static PVOID AeroGpuAllocContiguous(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ SIZE_T Size, _Out_ PHYSICAL_ADDRESS* Pa)
{
    PVOID va = AeroGpuAllocContiguousNoInit(Adapter, Size, Pa);
    if (!va) {
        return NULL;
    }

    /*
     * Zero the full underlying allocation size (page-rounded when pooled) so any
     * slack bytes are not left holding stale kernel data.
     *
     * This is not the hot submit path (callers that need no-init use
     * AeroGpuAllocContiguousNoInit), so the extra page-tail zeroing is acceptable.
     */
    UINT classIndex = 0;
    SIZE_T allocSize = 0;
    const BOOLEAN eligible = AeroGpuContigPoolClassForSize(Size, &classIndex, &allocSize);
    SIZE_T zeroBytes = 0;
    if (eligible && allocSize) {
        zeroBytes = allocSize;
    } else {
        if (!AeroGpuRoundUpToPageSize(Size, &zeroBytes)) {
            /*
             * The allocation already succeeded, so failing to round here would be unexpected.
             * Fall back to best-effort initialization of the requested range.
             */
            zeroBytes = Size;
        }
    }
    RtlZeroMemory(va, zeroBytes);
    return va;
}

static VOID AeroGpuFreeContiguousNonCached(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ PVOID Va, _In_ SIZE_T Size)
{
    if (!Va) {
        return;
    }

    ASSERT(Size != 0);
    if (Size == 0) {
        return;
    }

    UINT classIndex = 0;
    SIZE_T allocSize = 0;
    const BOOLEAN eligible = AeroGpuContigPoolClassForSize(Size, &classIndex, &allocSize);
    SIZE_T freeBytes = 0;
    if (eligible && allocSize) {
        freeBytes = allocSize;
    } else {
        if (!AeroGpuRoundUpToPageSize(Size, &freeBytes)) {
            /* Best-effort teardown: fall back to the caller-provided size. */
            freeBytes = Size;
        }
    }

    if (!Adapter) {
        /*
         * Even though the pool is adapter-scoped, keep freeing correct when an adapter context
         * isn't available (e.g. best-effort cleanup during partial init/teardown paths).
         *
         * Note: allocations made by AeroGpuAllocContiguous* are page-rounded, so the size passed
         * to MmFreeContiguousMemorySpecifyCache must match the rounded allocation size.
         */
        MmFreeContiguousMemorySpecifyCache(Va, freeBytes, MmNonCached);
        return;
    }

    if (eligible) {
        BOOLEAN returned = FALSE;
        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&Adapter->ContigPool.Lock, &oldIrql);

            const SIZE_T cap = (SIZE_T)AEROGPU_CONTIG_POOL_RETENTION_CAP_BYTES;
            if (Adapter->ContigPool.FreeCounts[classIndex] < (ULONG)AEROGPU_CONTIG_POOL_MAX_ENTRIES_PER_CLASS &&
                Adapter->ContigPool.BytesRetained <= cap && (cap - Adapter->ContigPool.BytesRetained) >= allocSize) {
                InsertTailList(&Adapter->ContigPool.FreeLists[classIndex], (PLIST_ENTRY)Va);
                Adapter->ContigPool.FreeCounts[classIndex] += 1;
                Adapter->ContigPool.BytesRetained += allocSize;
                returned = TRUE;

#if DBG
                InterlockedIncrement64(&Adapter->ContigPool.FreesToPool);
                /*
                 * Update high watermark under the lock to keep it monotonic and avoid
                 * needing an additional atomic.
                 */
                const LONGLONG retained = (LONGLONG)Adapter->ContigPool.BytesRetained;
                if (retained > Adapter->ContigPool.HighWatermarkBytes) {
                    /*
                     * Keep the store atomic even on x86 so concurrent readers (which may use
                     * InterlockedCompareExchange64 without taking the pool lock) never observe
                     * a torn 64-bit value.
                     */
                    InterlockedExchange64(&Adapter->ContigPool.HighWatermarkBytes, retained);
                }
#endif
            }

            KeReleaseSpinLock(&Adapter->ContigPool.Lock, oldIrql);
        }

        if (returned) {
            return;
        }
    }

    MmFreeContiguousMemorySpecifyCache(Va, freeBytes, MmNonCached);
#if DBG
    InterlockedIncrement64(&Adapter->ContigPool.FreesToOs);
    InterlockedIncrement64(&Adapter->ContigPool.OsFrees);
    InterlockedAdd64(&Adapter->ContigPool.OsFreeBytes, (LONGLONG)freeBytes);
#endif
}

static VOID AeroGpuFreeSubmissionMeta(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ AEROGPU_SUBMISSION_META* Meta)
{
    if (!Meta) {
        return;
    }

    AeroGpuFreeContiguousNonCached(Adapter, Meta->AllocTableVa, (SIZE_T)Meta->AllocTableSizeBytes);
    ExFreePoolWithTag(Meta, AEROGPU_POOL_TAG);
}

static NTSTATUS AeroGpuAlignUpSizeTChecked(_In_ SIZE_T Value, _In_ SIZE_T Alignment, _Out_ SIZE_T* Out)
{
    if (!Out) {
        return STATUS_INVALID_PARAMETER;
    }
    *Out = 0;

    if (Alignment == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    const SIZE_T mask = Alignment - 1;
    if ((Alignment & mask) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    SIZE_T sum = 0;
    NTSTATUS st = RtlSizeTAdd(Value, mask, &sum);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }

    *Out = sum & ~mask;
    return STATUS_SUCCESS;
}

static __forceinline UINT AeroGpuAllocTableComputeHashCap(_In_ UINT Count)
{
    UINT cap = 16;
    const uint64_t target = (uint64_t)Count * 2ull;
    while ((uint64_t)cap < target && cap < (1u << 30)) {
        cap <<= 1;
    }
    return cap;
}

static NTSTATUS AeroGpuAllocTableScratchAllocBlock(_In_ UINT TmpEntriesCap,
                                                   _In_ UINT HashCap,
                                                   _Outptr_result_bytebuffer_(*BlockBytesOut) PVOID* BlockOut,
                                                   _Out_ SIZE_T* BlockBytesOut,
                                                   _Out_ struct aerogpu_alloc_entry** TmpEntriesOut,
                                                   _Out_ uint64_t** SeenSlotsOut)
{
    if (!BlockOut || !BlockBytesOut || !TmpEntriesOut || !SeenSlotsOut) {
        return STATUS_INVALID_PARAMETER;
    }

    *BlockOut = NULL;
    *BlockBytesOut = 0;
    *TmpEntriesOut = NULL;
    *SeenSlotsOut = NULL;

    if (TmpEntriesCap == 0 || HashCap == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * The hash table uses (cap - 1) masking, so cap must be power-of-two.
     * AeroGpuAllocTableComputeHashCap() guarantees this, but validate anyway.
     */
    if ((HashCap & (HashCap - 1)) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Allocate a single NonPagedPool block and carve it into the scratch arrays needed by
     * BuildAllocTable. This keeps allocation count and fragmentation down, and makes it easy
     * to cache.
     */
    SIZE_T off = 0;
    SIZE_T tmpOff = 0;
    SIZE_T seenSlotsOff = 0;

    SIZE_T tmpBytes = 0;
    SIZE_T seenSlotsBytes = 0;

    NTSTATUS st = AeroGpuAlignUpSizeTChecked(off, 8, &off);
    if (!NT_SUCCESS(st)) {
        return st;
    }
    tmpOff = off;
    st = RtlSizeTMult((SIZE_T)TmpEntriesCap, sizeof(struct aerogpu_alloc_entry), &tmpBytes);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }
    st = RtlSizeTAdd(off, tmpBytes, &off);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }

    st = AeroGpuAlignUpSizeTChecked(off, 8, &off);
    if (!NT_SUCCESS(st)) {
        return st;
    }
    seenSlotsOff = off;
    st = RtlSizeTMult((SIZE_T)HashCap, sizeof(uint64_t), &seenSlotsBytes);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }
    st = RtlSizeTAdd(off, seenSlotsBytes, &off);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }

    if (off == 0) {
        return STATUS_INTEGER_OVERFLOW;
    }

    PVOID block = ExAllocatePoolWithTag(NonPagedPool, off, AEROGPU_POOL_TAG);
    if (!block) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    *BlockOut = block;
    *BlockBytesOut = off;

    *TmpEntriesOut = (struct aerogpu_alloc_entry*)((PUCHAR)block + tmpOff);
    *SeenSlotsOut = (uint64_t*)((PUCHAR)block + seenSlotsOff);

    /* Initialize slot array (epoch==0 means empty). */
    if (seenSlotsBytes != 0) {
        RtlZeroMemory(*SeenSlotsOut, seenSlotsBytes);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuAllocTableScratchEnsureCapacityLocked(_Inout_ AEROGPU_ALLOC_TABLE_SCRATCH* Scratch,
                                                             _In_ UINT RequiredTmpEntriesCap,
                                                             _In_ UINT RequiredHashCap)
{
    if (!Scratch || RequiredTmpEntriesCap == 0 || RequiredHashCap == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Scratch->Block && Scratch->TmpEntriesCapacity >= RequiredTmpEntriesCap && Scratch->HashCapacity >= RequiredHashCap) {
#if DBG
        InterlockedIncrement(&Scratch->HitCount);
#endif
        return STATUS_SUCCESS;
    }

    UINT newTmpCap = Scratch->TmpEntriesCapacity;
    UINT newHashCap = Scratch->HashCapacity;
    if (newTmpCap < RequiredTmpEntriesCap) {
        newTmpCap = RequiredTmpEntriesCap;
    }
    if (newHashCap < RequiredHashCap) {
        newHashCap = RequiredHashCap;
    }

    PVOID newBlock = NULL;
    SIZE_T newBlockBytes = 0;
    struct aerogpu_alloc_entry* newTmpEntries = NULL;
    uint64_t* newSeenSlots = NULL;
    NTSTATUS st = AeroGpuAllocTableScratchAllocBlock(newTmpCap,
                                                     newHashCap,
                                                     &newBlock,
                                                     &newBlockBytes,
                                                     &newTmpEntries,
                                                     &newSeenSlots);
    if (!NT_SUCCESS(st)) {
        return st;
    }

    PVOID oldBlock = Scratch->Block;

    Scratch->Block = newBlock;
    Scratch->BlockBytes = newBlockBytes;
    Scratch->TmpEntriesCapacity = newTmpCap;
    Scratch->HashCapacity = newHashCap;
    Scratch->TmpEntries = newTmpEntries;
    Scratch->SeenSlots = newSeenSlots;
    Scratch->Epoch = 0;

#if DBG
    InterlockedIncrement(&Scratch->GrowCount);
    static volatile LONG g_BuildAllocTableScratchGrowLogCount = 0;
    AEROGPU_LOG_RATELIMITED(g_BuildAllocTableScratchGrowLogCount,
                            4,
                            "BuildAllocTable: scratch grow tmp_cap=%u hash_cap=%u bytes=%Iu",
                            newTmpCap,
                            newHashCap,
                            (SIZE_T)newBlockBytes);
#endif

    if (oldBlock) {
        ExFreePoolWithTag(oldBlock, AEROGPU_POOL_TAG);
    }
    return STATUS_SUCCESS;
}

static __forceinline uint32_t AeroGpuAllocTableEntryFlagsFromAllocationListEntry(_In_ const DXGK_ALLOCATIONLIST* Entry)
{
    /*
     * Win7/WDDM 1.1 supplies per-allocation access metadata for each submission in the allocation list.
     *
     * Propagate this into `aerogpu_alloc_entry.flags` so the host can reject attempts to write back
     * into guest memory that the runtime did not mark as writable for the current submission.
     *
     * Fail-open for compatibility: if we cannot determine write access reliably, leave READONLY
     * clear and log (DBG-only, rate-limited).
     */
    if (!Entry) {
        return 0;
    }

#if defined(AEROGPU_KMD_USE_WDK_DDI) && AEROGPU_KMD_USE_WDK_DDI
    /*
     * WDDM 1.1 contract: DXGK_ALLOCATIONLIST carries per-submit access flags.
     * Bit 0 of Flags.Value corresponds to WriteOperation.
     */
    const BOOLEAN written = ((Entry->Flags.Value & 0x1u) != 0) ? TRUE : FALSE;
    return written ? 0u : (uint32_t)AEROGPU_ALLOC_FLAG_READONLY;
#else
#if DBG
    static volatile LONG g_BuildAllocTableReadonlyFallbackLogCount = 0;
    AEROGPU_LOG_RATELIMITED(g_BuildAllocTableReadonlyFallbackLogCount,
                            8,
                            "%s",
                            "BuildAllocTable: allocation list access flags unavailable; not setting AEROGPU_ALLOC_FLAG_READONLY");
#endif
    return 0;
#endif
}

static NTSTATUS AeroGpuBuildAllocTableFillScratch(_In_reads_opt_(Count) const DXGK_ALLOCATIONLIST* List,
                                                  _In_ UINT Count,
                                                  _Inout_updates_(TmpEntriesCap) struct aerogpu_alloc_entry* TmpEntries,
                                                  _In_ UINT TmpEntriesCap,
                                                  _Inout_updates_(HashCap) uint64_t* SeenSlots,
                                                  _In_ uint16_t Epoch,
                                                  _In_ UINT HashCap,
                                                  _Out_ UINT* EntryCountOut)
{
    if (EntryCountOut) {
        *EntryCountOut = 0;
    }

    if (!List || Count == 0) {
        return STATUS_SUCCESS;
    }
    if (!TmpEntries || TmpEntriesCap == 0 || !SeenSlots || Epoch == 0 || !EntryCountOut) {
        return STATUS_INVALID_PARAMETER;
    }
    if (HashCap < 2 || (HashCap & (HashCap - 1)) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    UINT entryCount = 0;
    const UINT mask = HashCap - 1;

    for (UINT i = 0; i < Count; ++i) {
        AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)List[i].hAllocation;
        if (!alloc) {
            continue;
        }

        const uint32_t allocId = (uint32_t)alloc->AllocationId;
        if (allocId == 0) {
#if DBG
            static volatile LONG g_BuildAllocTableZeroAllocIdLogCount = 0;
            AEROGPU_LOG_RATELIMITED(
                g_BuildAllocTableZeroAllocIdLogCount, 8, "BuildAllocTable: AllocationList[%u] has alloc_id=0", i);
#endif
            continue;
        }

        const uint32_t entryFlags = AeroGpuAllocTableEntryFlagsFromAllocationListEntry(&List[i]);

        UINT slot = (allocId * 2654435761u) & mask;
        for (;;) {
            const uint64_t slotVal = SeenSlots[slot];
            const uint16_t slotEpoch = (uint16_t)(slotVal >> 48);
            if (slotEpoch != Epoch) {
                if (entryCount >= TmpEntriesCap) {
                    return STATUS_INTEGER_OVERFLOW;
                }
                if (entryCount > UINT16_MAX) {
                    return STATUS_INTEGER_OVERFLOW;
                }
                SeenSlots[slot] = ((uint64_t)Epoch << 48) | ((uint64_t)(uint16_t)entryCount << 32) | (uint64_t)allocId;

                TmpEntries[entryCount].alloc_id = allocId;
                TmpEntries[entryCount].flags = entryFlags;
                TmpEntries[entryCount].gpa = (uint64_t)List[i].PhysicalAddress.QuadPart;
                TmpEntries[entryCount].size_bytes = (uint64_t)alloc->SizeBytes;
                TmpEntries[entryCount].reserved0 = 0;

                entryCount += 1;
                break;
            }

            const uint32_t existing = (uint32_t)slotVal;
            if (existing == allocId) {
                const UINT entryIndex = (UINT)((slotVal >> 32) & 0xFFFFu);
                const uint64_t gpa = (uint64_t)List[i].PhysicalAddress.QuadPart;
                const uint64_t sizeBytes = (uint64_t)alloc->SizeBytes;
                if (entryIndex >= entryCount) {
                    return STATUS_INVALID_PARAMETER;
                }
                if (TmpEntries[entryIndex].gpa != gpa) {
#if DBG
                    static volatile LONG g_BuildAllocTableAllocIdCollisionLogCount = 0;
                    AEROGPU_LOG_RATELIMITED(
                        g_BuildAllocTableAllocIdCollisionLogCount,
                        8,
                        "BuildAllocTable: alloc_id collision: alloc_id=%lu first_entry=%u gpa0=0x%I64x size0=%I64u list_index=%u gpa1=0x%I64x size1=%I64u",
                        (ULONG)allocId,
                        (unsigned)entryIndex,
                        (ULONGLONG)TmpEntries[entryIndex].gpa,
                        (ULONGLONG)TmpEntries[entryIndex].size_bytes,
                        (unsigned)i,
                        (ULONGLONG)gpa,
                        (ULONGLONG)sizeBytes);
#endif
                    return STATUS_INVALID_PARAMETER;
                }

                /*
                 * Duplicate alloc_id for identical backing. Size may vary due to runtime
                 * alignment or different handle wrappers (CreateAllocation vs OpenAllocation).
                 * Use the maximum size to keep validation/bounds checks permissive.
                 */
                if (entryIndex < entryCount && sizeBytes > TmpEntries[entryIndex].size_bytes) {
                    TmpEntries[entryIndex].size_bytes = sizeBytes;
                }

                /* Merge submission-time access flags: READONLY only if all aliases are read-only. */
                if (entryIndex < entryCount) {
                    TmpEntries[entryIndex].flags &= entryFlags;
                }
                break;
            }

            slot = (slot + 1) & mask;
        }
    }

    *EntryCountOut = entryCount;
    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuBuildAllocTable(_Inout_ AEROGPU_ADAPTER* Adapter,
                                       _In_reads_opt_(Count) const DXGK_ALLOCATIONLIST* List,
                                       _In_ UINT Count,
                                       _Outptr_result_bytebuffer_(*OutSizeBytes) PVOID* OutVa,
                                       _Out_ PHYSICAL_ADDRESS* OutPa,
                                       _Out_ UINT* OutSizeBytes)
{
    if (!OutVa || !OutPa || !OutSizeBytes) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutVa = NULL;
    OutPa->QuadPart = 0;
    *OutSizeBytes = 0;

    if (Count > AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!Adapter) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!Count || !List) {
        return STATUS_SUCCESS;
    }

    NTSTATUS st = STATUS_SUCCESS;
    UINT entryCount = 0;

    /*
     * BuildAllocTable uses an adapter-owned shared scratch buffer. To reduce the
     * time that buffer is held under the mutex (and therefore reduce contention
     * between concurrent submissions), copy small tables onto the stack and
     * release the scratch lock early.
     *
     * Keep this conservative: kernel stack is limited, especially on x86.
     */
    enum { AEROGPU_ALLOC_TABLE_SCRATCH_STACK_COPY_MAX_ENTRIES = 64 };
    struct aerogpu_alloc_entry stackEntries[AEROGPU_ALLOC_TABLE_SCRATCH_STACK_COPY_MAX_ENTRIES];
    const struct aerogpu_alloc_entry* entriesToCopy = NULL;

    struct aerogpu_alloc_entry* tmpEntries = NULL;
    uint64_t* seenSlots = NULL;

    PVOID slowBlock = NULL;
    SIZE_T slowBlockBytes = 0;
    BOOLEAN usingCache = FALSE;
    BOOLEAN scratchLockHeld = FALSE;

    PVOID tableVa = NULL;
    PHYSICAL_ADDRESS tablePa;
    SIZE_T tableSizeBytes = 0;
    SIZE_T entriesBytes = 0;
    tablePa.QuadPart = 0;

    /*
     * LastKnownPa is consumed by the CPU mapping path (DxgkDdiLock) and may be
     * read/written concurrently. Guard it with CpuMapMutex to avoid torn 64-bit
     * writes on x86.
     *
     * Do this outside the scratch-cache lock so concurrent submissions can still
     * update their allocations even if they contend on the cached scratch buffer.
     */
    UINT nonZeroAllocIdCount = 0;
    for (UINT i = 0; i < Count; ++i) {
        AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)List[i].hAllocation;
        if (!alloc) {
#if DBG
            static volatile LONG g_BuildAllocTableNullHandleLogCount = 0;
            AEROGPU_LOG_RATELIMITED(g_BuildAllocTableNullHandleLogCount,
                                    8,
                                    "BuildAllocTable: AllocationList[%u] has null hAllocation",
                                    i);
#endif
            continue;
        }
        ExAcquireFastMutex(&alloc->CpuMapMutex);
        alloc->LastKnownPa.QuadPart = List[i].PhysicalAddress.QuadPart;
        ExReleaseFastMutex(&alloc->CpuMapMutex);
        if (alloc->AllocationId != 0) {
            nonZeroAllocIdCount += 1;
        }
    }

    /*
     * If no allocations in this submission have a non-zero alloc_id, omit the table entirely
     * (alloc_table_gpa/size will be 0). This avoids taking the scratch-cache lock
     * and touching large scratch arrays on submissions that never reference guest-backed memory.
     */
    if (nonZeroAllocIdCount == 0) {
        return STATUS_SUCCESS;
    }

    /*
     * Size scratch structures based on the number of non-zero alloc_id values rather than the
     * total allocation-list length. Many allocation list entries may have alloc_id == 0 (never
     * referenced via alloc_id in the command stream), and we only need scratch space for the
     * subset that can actually be inserted into the table.
     *
     * Round tmp-entry capacity up to the hash-table load target so the scratch cache grows in
     * larger steps (reducing realloc churn) while keeping memory bounded.
     */
    const UINT cap = AeroGpuAllocTableComputeHashCap(nonZeroAllocIdCount);
    UINT tmpEntriesCap = nonZeroAllocIdCount;
    const UINT targetTmpCap = cap / 2; /* hash cap is >= 2*N */
    if (tmpEntriesCap < targetTmpCap) {
        tmpEntriesCap = targetTmpCap;
    }

    const ULONG cpu = KeGetCurrentProcessorNumber();
    const UINT scratchShard = (UINT)(cpu % (ULONG)AEROGPU_ALLOC_TABLE_SCRATCH_SHARD_COUNT);
    AEROGPU_ALLOC_TABLE_SCRATCH* scratch = &Adapter->AllocTableScratch[scratchShard];

    /*
     * Use an adapter-owned sharded scratch block when possible; this avoids per-submit
     * NonPagedPool churn and reduces contention between concurrent submissions.
     *
     * We shard by current CPU to spread concurrent callers across independent scratch
     * buffers while keeping the implementation simple/deterministic.
     */
    ExAcquireFastMutex(&scratch->Mutex);
    const NTSTATUS scratchSt = AeroGpuAllocTableScratchEnsureCapacityLocked(scratch, tmpEntriesCap, cap);
    if (NT_SUCCESS(scratchSt)) {
        tmpEntries = scratch->TmpEntries;
        seenSlots = scratch->SeenSlots;
        usingCache = TRUE;
        scratchLockHeld = TRUE;
    } else {
#if DBG
        static volatile LONG g_BuildAllocTableScratchFallbackLogCount = 0;
        AEROGPU_LOG_RATELIMITED(g_BuildAllocTableScratchFallbackLogCount,
                                4,
                                "BuildAllocTable: scratch[%u] cache unavailable (Count=%u alloc_ids=%u cap=%u); falling back to per-call allocations",
                                scratchShard,
                                Count,
                                nonZeroAllocIdCount,
                                cap);
#endif
        ExReleaseFastMutex(&scratch->Mutex);
        scratchLockHeld = FALSE;

        if (scratchSt != STATUS_INSUFFICIENT_RESOURCES) {
            return scratchSt;
        }

        /* Allocation failure growing the cache. Fall back to one-off scratch allocations. */
        const NTSTATUS allocSt = AeroGpuAllocTableScratchAllocBlock(tmpEntriesCap,
                                                                    cap,
                                                                    &slowBlock,
                                                                    &slowBlockBytes,
                                                                    &tmpEntries,
                                                                    &seenSlots);
        if (!NT_SUCCESS(allocSt)) {
            return allocSt;
        }
    }

    uint16_t epoch = 1;
    if (usingCache) {
        epoch = (uint16_t)(scratch->Epoch + 1);
        scratch->Epoch = epoch;
        if (epoch == 0) {
            /* Epoch wrapped; clear and restart at 1. */
            SIZE_T slotsBytes = 0;
            if (!NT_SUCCESS(RtlSizeTMult((SIZE_T)scratch->HashCapacity, sizeof(uint64_t), &slotsBytes))) {
                st = STATUS_INTEGER_OVERFLOW;
                goto cleanup;
            }
            RtlZeroMemory(scratch->SeenSlots, slotsBytes);
            epoch = 1;
            scratch->Epoch = epoch;
        }
    }

    st = AeroGpuBuildAllocTableFillScratch(List, Count, tmpEntries, tmpEntriesCap, seenSlots, epoch, cap, &entryCount);
    if (!NT_SUCCESS(st)) {
        goto cleanup;
    }

    /*
     * If no allocations in this submission have a non-zero alloc_id, omit the table entirely
     * (alloc_table_gpa/size will be 0). This avoids doing extra work on submissions that never
     * reference guest-backed memory via alloc_id.
     */
    if (entryCount == 0) {
        st = STATUS_SUCCESS;
        goto cleanup;
    }

    entriesToCopy = tmpEntries;
    if (usingCache && entryCount <= AEROGPU_ALLOC_TABLE_SCRATCH_STACK_COPY_MAX_ENTRIES) {
        SIZE_T stackCopyBytes = 0;
        st = RtlSizeTMult((SIZE_T)entryCount, sizeof(stackEntries[0]), &stackCopyBytes);
        if (!NT_SUCCESS(st)) {
            st = STATUS_INTEGER_OVERFLOW;
            goto cleanup;
        }
        RtlCopyMemory(stackEntries, tmpEntries, stackCopyBytes);
        entriesToCopy = stackEntries;
        ExReleaseFastMutex(&scratch->Mutex);
        scratchLockHeld = FALSE;
    }

    st = RtlSizeTMult((SIZE_T)entryCount, sizeof(struct aerogpu_alloc_entry), &entriesBytes);
    if (!NT_SUCCESS(st)) {
        st = STATUS_INTEGER_OVERFLOW;
        goto cleanup;
    }

    st = RtlSizeTAdd(sizeof(struct aerogpu_alloc_table_header), entriesBytes, &tableSizeBytes);
    if (!NT_SUCCESS(st) || tableSizeBytes > UINT32_MAX) {
        st = STATUS_INTEGER_OVERFLOW;
        goto cleanup;
    }

    tableVa = AeroGpuAllocContiguousNoInit(Adapter, tableSizeBytes, &tablePa);
    if (!tableVa) {
        st = STATUS_INSUFFICIENT_RESOURCES;
        goto cleanup;
    }

    struct aerogpu_alloc_table_header* hdr = (struct aerogpu_alloc_table_header*)tableVa;
    hdr->magic = AEROGPU_ALLOC_TABLE_MAGIC;
    hdr->abi_version = AEROGPU_ABI_VERSION_U32;
    hdr->size_bytes = (uint32_t)tableSizeBytes;
    hdr->entry_count = (uint32_t)entryCount;
    hdr->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_alloc_entry);
    hdr->reserved0 = 0;

    if (entryCount) {
        struct aerogpu_alloc_entry* outEntries = (struct aerogpu_alloc_entry*)(hdr + 1);
        RtlCopyMemory(outEntries, entriesToCopy, entriesBytes);
    }

    /* dbgctl perf counters: record alloc-table build activity + READONLY propagation. */
    {
        UINT readonlyCount = 0;
        for (UINT i = 0; i < entryCount; ++i) {
            if ((entriesToCopy[i].flags & (uint32_t)AEROGPU_ALLOC_FLAG_READONLY) != 0) {
                readonlyCount += 1;
            }
        }
        InterlockedAdd64(&Adapter->PerfAllocTableEntries, (LONGLONG)entryCount);
        InterlockedAdd64(&Adapter->PerfAllocTableReadonlyEntries, (LONGLONG)readonlyCount);
        InterlockedIncrement64(&Adapter->PerfAllocTableCount);
    }

    *OutVa = tableVa;
    *OutPa = tablePa;
    *OutSizeBytes = (UINT)tableSizeBytes;
    tableVa = NULL;

cleanup:
    if (tableVa) {
        AeroGpuFreeContiguousNonCached(Adapter, tableVa, tableSizeBytes);
    }
    if (scratchLockHeld) {
        ExReleaseFastMutex(&scratch->Mutex);
    }
    if (slowBlock) {
        ExFreePoolWithTag(slowBlock, AEROGPU_POOL_TAG);
    }
    return st;
}

typedef struct _AEROGPU_SCANOUT_MMIO_SNAPSHOT {
    ULONG Enable;
    ULONG Width;
    ULONG Height;
    ULONG PitchBytes;
    ULONG Format; /* enum aerogpu_format */
    PHYSICAL_ADDRESS FbPa;
} AEROGPU_SCANOUT_MMIO_SNAPSHOT;

static BOOLEAN AeroGpuBytesPerPixelFromFormat(_In_ ULONG Format, _Out_ ULONG* OutBytesPerPixel)
{
    if (!OutBytesPerPixel) {
        return FALSE;
    }

    switch (Format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM_SRGB:
        *OutBytesPerPixel = 4;
        return TRUE;
    case AEROGPU_FORMAT_B5G6R5_UNORM:
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
        *OutBytesPerPixel = 2;
        return TRUE;
    default:
        *OutBytesPerPixel = 0;
        return FALSE;
    }
}

static BOOLEAN AeroGpuIsPlausibleScanoutSnapshot(_In_ const AEROGPU_SCANOUT_MMIO_SNAPSHOT* Snapshot)
{
    if (!Snapshot) {
        return FALSE;
    }
    if (Snapshot->Width == 0 || Snapshot->Height == 0 || Snapshot->PitchBytes == 0) {
        return FALSE;
    }

    if (Snapshot->Width > 16384u || Snapshot->Height > 16384u) {
        return FALSE;
    }

    ULONG bpp = 0;
    if (!AeroGpuBytesPerPixelFromFormat(Snapshot->Format, &bpp) || bpp == 0) {
        return FALSE;
    }

    if (Snapshot->Width > (0xFFFFFFFFu / bpp)) {
        return FALSE;
    }
    const ULONG rowBytes = Snapshot->Width * bpp;
    if (Snapshot->PitchBytes < rowBytes) {
        return FALSE;
    }

    return TRUE;
}

static BOOLEAN AeroGpuGetScanoutMmioSnapshot(_In_ const AEROGPU_ADAPTER* Adapter, _Out_ AEROGPU_SCANOUT_MMIO_SNAPSHOT* Out)
{
    if (!Adapter || !Adapter->Bar0 || !Out) {
        return FALSE;
    }

    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
        DxgkDevicePowerStateD0) {
        /* Avoid MMIO reads while the adapter is not in D0. */
        return FALSE;
    }

    RtlZeroMemory(Out, sizeof(*Out));
    Out->FbPa.QuadPart = 0;

    if ((Adapter->UsingNewAbi || Adapter->AbiKind == AEROGPU_ABI_KIND_V1) &&
        Adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
        Out->Enable = AeroGpuReadRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE);
        Out->Width = AeroGpuReadRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_WIDTH);
        Out->Height = AeroGpuReadRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_HEIGHT);
        Out->Format = AeroGpuReadRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FORMAT);
        if (Out->Format == AEROGPU_FORMAT_INVALID) {
            /*
             * Some boot/VBE paths may not initialize the scanout format register
             * even though the mode is a standard 32bpp X8R8G8B8-compatible
             * framebuffer. Default it so post-display-ownership handoff can still
             * infer a plausible mode/stride.
             */
            Out->Format = AEROGPU_FORMAT_B8G8R8X8_UNORM;
        }
        Out->PitchBytes = AeroGpuReadRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES);
        if (Out->PitchBytes == 0 && Out->Width != 0) {
            /*
             * Some boot/VBE paths may leave the pitch register unset. For a
             * standard linear framebuffer, default to tightly packed rows based
             * on the selected format.
             */
            ULONG bpp = 0;
            if (AeroGpuBytesPerPixelFromFormat(Out->Format, &bpp) && bpp != 0 && Out->Width <= (0xFFFFFFFFu / bpp)) {
                Out->PitchBytes = Out->Width * bpp;
            }
        }
        Out->FbPa.QuadPart =
            (LONGLONG)AeroGpuReadRegU64HiLoHi(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI);
        return TRUE;
    }

    if (Adapter->Bar0Length < (AEROGPU_LEGACY_REG_SCANOUT_FB_HI + sizeof(ULONG))) {
        return FALSE;
    }

    Out->Enable = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE);
    Out->Width = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_WIDTH);
    Out->Height = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_HEIGHT);
    Out->PitchBytes = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_PITCH);

    const ULONG legacyFormat = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FORMAT);
    if (legacyFormat == AEROGPU_LEGACY_SCANOUT_X8R8G8B8 || legacyFormat == 0) {
        /*
         * Legacy scanout format register is a bring-up-only enum. Some device
         * models may leave it at 0 during boot; default to our canonical 32bpp
         * scanout format so post-display ownership can still infer a plausible
         * mode/stride.
         */
        Out->Format = AEROGPU_FORMAT_B8G8R8X8_UNORM;
    } else {
        Out->Format = AEROGPU_FORMAT_INVALID;
    }

    if (Out->PitchBytes == 0 && Out->Width != 0) {
        ULONG bpp = 0;
        if (AeroGpuBytesPerPixelFromFormat(Out->Format, &bpp) && bpp != 0 && Out->Width <= (0xFFFFFFFFu / bpp)) {
            Out->PitchBytes = Out->Width * bpp;
        }
    }

    Out->FbPa.QuadPart =
        (LONGLONG)AeroGpuReadRegU64HiLoHi(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO, AEROGPU_LEGACY_REG_SCANOUT_FB_HI);
    return TRUE;
}

static D3DDDIFORMAT AeroGpuDdiColorFormatFromScanoutFormat(_In_ ULONG Format)
{
    switch (Format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB:
        return D3DDDIFMT_A8R8G8B8;
    case AEROGPU_FORMAT_B5G6R5_UNORM:
        return D3DDDIFMT_R5G6B5;
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
        return D3DDDIFMT_A1R5G5B5;
    default:
        return D3DDDIFMT_X8R8G8B8;
    }
}

static VOID AeroGpuProgramScanout(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ PHYSICAL_ADDRESS FbPa)
{
    if (!Adapter || !Adapter->Bar0) {
        return;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) != DxgkDevicePowerStateD0) {
        return;
    }

    /*
     * Guard against stale/invalid framebuffer addresses.
     *
     * During boot and during post-display-ownership transitions, dxgkrnl may call
     * StartDevice/AcquirePostDisplayOwnership before it has committed a VidPN and
     * before it has provided a valid PrimaryAddress via SetVidPnSourceAddress.
     *
     * Never enable scanout with FbPa == 0, otherwise the device may DMA from GPA 0
     * continuously (cursor/scanout) which can destabilize guests and makes
     * transitions flicker/black.
     */
    const ULONG enable = (Adapter->SourceVisible && !Adapter->PostDisplayOwnershipReleased && FbPa.QuadPart != 0) ? 1u : 0u;

    if (Adapter->UsingNewAbi || Adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        if (Adapter->Bar0Length < (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
            /* Defensive: avoid out-of-bounds MMIO on partial BAR0 mappings. */
            return;
        }
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_WIDTH, Adapter->CurrentWidth);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_HEIGHT, Adapter->CurrentHeight);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FORMAT, Adapter->CurrentFormat);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES, Adapter->CurrentPitch);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, FbPa.LowPart);
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI, (ULONG)(FbPa.QuadPart >> 32));
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, enable);

        if (!enable && Adapter->SupportsVblank && Adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
        return;
    }

    if (Adapter->Bar0Length < (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
        /* Defensive: avoid out-of-bounds MMIO on partial BAR0 mappings. */
        return;
    }
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO, FbPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_HI, (ULONG)(FbPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_PITCH, Adapter->CurrentPitch);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_WIDTH, Adapter->CurrentWidth);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_HEIGHT, Adapter->CurrentHeight);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_FORMAT, AEROGPU_LEGACY_SCANOUT_X8R8G8B8);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, enable);
    if (!enable && Adapter->SupportsVblank && Adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
        /* Be robust against stale vblank IRQ state on scanout disable. */
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
    }
}

static VOID AeroGpuSetScanoutEnable(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG Enable)
{
    if (!Adapter->Bar0) {
        return;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) != DxgkDevicePowerStateD0) {
        return;
    }

    if (Enable && (Adapter->CurrentScanoutFbPa.QuadPart == 0 || Adapter->PostDisplayOwnershipReleased)) {
        /*
         * Be conservative: never enable scanout unless we have a non-zero cached
         * framebuffer address. This prevents accidental DMA from GPA 0 when
         * dxgkrnl toggles visibility before SetVidPnSourceAddress runs.
         */
        Enable = 0;
    }

    if (Adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        if (Adapter->Bar0Length < (AEROGPU_MMIO_REG_SCANOUT0_ENABLE + sizeof(ULONG))) {
            return;
        }
        AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, Enable);
        if (!Enable) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            if (Adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
                AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
            }
        }
    } else {
        if (Adapter->Bar0Length < (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
            return;
        }
        AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, Enable);
        if (!Enable && Adapter->SupportsVblank && Adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            /* Be robust against stale vblank IRQ state on scanout disable. */
            AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
        }
    }
}

static __forceinline VOID AeroGpuLegacyRingUpdateHeadSeqLocked(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONG HeadIndex)
{
    if (!Adapter || Adapter->RingEntryCount == 0) {
        return;
    }

    const ULONG ringEntryCount = Adapter->RingEntryCount;

    ULONG oldIndex = Adapter->LegacyRingHeadIndex;
    if (oldIndex >= ringEntryCount) {
        /* Defensive: clamp corrupted cached index into range. */
        oldIndex %= ringEntryCount;
        Adapter->LegacyRingHeadIndex = oldIndex;
    }

    if (HeadIndex >= ringEntryCount) {
        /* Defensive: legacy head index is a masked register. */
        HeadIndex %= ringEntryCount;
    }

    if (HeadIndex == oldIndex) {
        return;
    }

    const ULONG delta = (HeadIndex > oldIndex) ? (HeadIndex - oldIndex) : (HeadIndex + ringEntryCount - oldIndex);
    Adapter->LegacyRingHeadSeq += delta;
    Adapter->LegacyRingHeadIndex = HeadIndex;
}

static NTSTATUS AeroGpuLegacyRingInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->RingEntryCount = AEROGPU_RING_ENTRY_COUNT_DEFAULT;
    Adapter->RingTail = 0;
    Adapter->LegacyRingHeadIndex = 0;
    Adapter->LegacyRingHeadSeq = 0;
    Adapter->LegacyRingTailSeq = 0;

    const SIZE_T ringBytes = Adapter->RingEntryCount * sizeof(aerogpu_legacy_ring_entry);
    Adapter->RingVa = AeroGpuAllocContiguous(Adapter, ringBytes, &Adapter->RingPa);
    if (!Adapter->RingVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    Adapter->RingSizeBytes = (ULONG)ringBytes;

    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, Adapter->RingPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, (ULONG)(Adapter->RingPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, Adapter->RingEntryCount);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);

    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1RingInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->RingEntryCount = AEROGPU_RING_ENTRY_COUNT_DEFAULT;
    Adapter->RingTail = 0;
    Adapter->LegacyRingHeadIndex = 0;
    Adapter->LegacyRingHeadSeq = 0;
    Adapter->LegacyRingTailSeq = 0;

    SIZE_T ringBytes = sizeof(struct aerogpu_ring_header) +
                       (SIZE_T)Adapter->RingEntryCount * sizeof(struct aerogpu_submit_desc);
    ringBytes = (ringBytes + PAGE_SIZE - 1) & ~(SIZE_T)(PAGE_SIZE - 1);

    Adapter->RingVa = AeroGpuAllocContiguous(Adapter, ringBytes, &Adapter->RingPa);
    if (!Adapter->RingVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    Adapter->RingSizeBytes = (ULONG)ringBytes;

    Adapter->RingHeader = (struct aerogpu_ring_header*)Adapter->RingVa;
    Adapter->RingHeader->magic = AEROGPU_RING_MAGIC;
    Adapter->RingHeader->abi_version = AEROGPU_ABI_VERSION_U32;
    Adapter->RingHeader->size_bytes = (uint32_t)ringBytes;
    Adapter->RingHeader->entry_count = (uint32_t)Adapter->RingEntryCount;
    Adapter->RingHeader->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
    Adapter->RingHeader->flags = 0;
    Adapter->RingHeader->head = 0;
    Adapter->RingHeader->tail = 0;

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_GPA_LO, Adapter->RingPa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_GPA_HI, (ULONG)(Adapter->RingPa.QuadPart >> 32));
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, Adapter->RingSizeBytes);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_RING_CONTROL, AEROGPU_RING_CONTROL_ENABLE);

    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1FencePageInit(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    Adapter->FencePageVa = NULL;
    Adapter->FencePagePa.QuadPart = 0;

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);

    if ((Adapter->DeviceFeatures & AEROGPU_FEATURE_FENCE_PAGE) == 0) {
        return STATUS_SUCCESS;
    }

    Adapter->FencePageVa = (struct aerogpu_fence_page*)AeroGpuAllocContiguous(Adapter, PAGE_SIZE, &Adapter->FencePagePa);
    if (!Adapter->FencePageVa) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    Adapter->FencePageVa->magic = AEROGPU_FENCE_PAGE_MAGIC;
    Adapter->FencePageVa->abi_version = AEROGPU_ABI_VERSION_U32;
    AeroGpuAtomicWriteU64((volatile ULONGLONG*)&Adapter->FencePageVa->completed_fence, 0);

    KeMemoryBarrier();

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, Adapter->FencePagePa.LowPart);
    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, (ULONG)(Adapter->FencePagePa.QuadPart >> 32));

    return STATUS_SUCCESS;
}

static VOID AeroGpuRingCleanup(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

    /*
     * Ring state can be observed concurrently by:
     *  - dbgctl escapes (under RingLock),
     *  - internal submission cleanup (under PendingLock), and
     *  - legacy ring head/tail polling (under RingLock).
     *
     * Detach pointers/metadata under the same lock ordering used elsewhere
     * (PendingLock -> RingLock), then free outside the locks to avoid holding
     * spin locks across potentially slow MmFreeContiguousMemory* calls.
     */
    PVOID ringVa = NULL;
    SIZE_T ringSizeBytes = 0;
    PVOID fencePageVa = NULL;

    KIRQL pendingIrql;
    KeAcquireSpinLock(&Adapter->PendingLock, &pendingIrql);

    KIRQL ringIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &ringIrql);

    ringVa = Adapter->RingVa;
    ringSizeBytes = (SIZE_T)Adapter->RingSizeBytes;

    Adapter->RingVa = NULL;
    Adapter->RingPa.QuadPart = 0;
    Adapter->RingSizeBytes = 0;
    Adapter->RingEntryCount = 0;
    Adapter->RingTail = 0;
    Adapter->LegacyRingHeadIndex = 0;
    Adapter->LegacyRingHeadSeq = 0;
    Adapter->LegacyRingTailSeq = 0;
    Adapter->RingHeader = NULL;

    fencePageVa = Adapter->FencePageVa;
    Adapter->FencePageVa = NULL;
    Adapter->FencePagePa.QuadPart = 0;

    KeReleaseSpinLock(&Adapter->RingLock, ringIrql);
    KeReleaseSpinLock(&Adapter->PendingLock, pendingIrql);

    AeroGpuFreeContiguousNonCached(Adapter, ringVa, ringSizeBytes);
    AeroGpuFreeContiguousNonCached(Adapter, fencePageVa, PAGE_SIZE);
}

static VOID AeroGpuUnmapBar0(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    /*
     * Detach Bar0 from the adapter before unmapping so any concurrent paths that
     * check `Adapter->Bar0` will observe NULL and avoid touching unmapped I/O
     * space.
     *
     * This is defensive against teardown races where an ISR/DPC or a late
     * DxgkDdi* callback might still run while StopDevice/StartDevice failure is
     * unmapping BAR0.
     */
    if (!Adapter) {
        return;
    }

    PUCHAR bar0 = (PUCHAR)InterlockedExchangePointer((PVOID volatile*)&Adapter->Bar0, NULL);
    const ULONG bar0Length = (ULONG)InterlockedExchange((volatile LONG*)&Adapter->Bar0Length, 0);
    if (!bar0 || bar0Length == 0) {
        return;
    }
    MmUnmapIoSpace(bar0, bar0Length);
}

static __forceinline BOOLEAN AeroGpuV1SubmitPathUsable(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter || !Adapter->Bar0 || !Adapter->RingVa || Adapter->RingEntryCount == 0) {
        return FALSE;
    }

    if (Adapter->Bar0Length < (AEROGPU_MMIO_REG_DOORBELL + sizeof(ULONG))) {
        return FALSE;
    }

    if (Adapter->RingSizeBytes < sizeof(struct aerogpu_ring_header)) {
        return FALSE;
    }

    const ULONG ringEntryCount = Adapter->RingEntryCount;
    if ((ringEntryCount & (ringEntryCount - 1)) != 0) {
        /* v1 ring requires a power-of-two entry count (see aerogpu_ring.h). */
        return FALSE;
    }

    const ULONGLONG minRingBytes =
        (ULONGLONG)sizeof(struct aerogpu_ring_header) +
        (ULONGLONG)ringEntryCount * (ULONGLONG)sizeof(struct aerogpu_submit_desc);
    if (minRingBytes > (ULONGLONG)Adapter->RingSizeBytes) {
        return FALSE;
    }

    const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)Adapter->RingVa;
    if (ringHeader->magic != AEROGPU_RING_MAGIC) {
        return FALSE;
    }
    if ((ringHeader->abi_version >> 16) != AEROGPU_ABI_MAJOR) {
        return FALSE;
    }
    if (ringHeader->entry_count != ringEntryCount) {
        return FALSE;
    }
    /* KMD expects the fixed descriptor stride used by the current ABI. */
    if (ringHeader->entry_stride_bytes != sizeof(struct aerogpu_submit_desc)) {
        return FALSE;
    }
    if ((ULONGLONG)ringHeader->size_bytes < minRingBytes) {
        return FALSE;
    }
    if (ringHeader->size_bytes > (uint32_t)Adapter->RingSizeBytes) {
        return FALSE;
    }

    /*
     * Sanity-check the current head/tail distance. The v1 ABI defines head/tail as
     * monotonically increasing counters (mod 2^32). The pending distance is
     * `tail - head` in unsigned arithmetic (wrap-around-safe).
     *
     * If the ring is corrupted (e.g. clobbered head/tail), the subtraction can
     * yield a very large number. Treat this as "ring unusable" to avoid any
     * out-of-bounds indexing in the submission path.
     */
    const uint32_t pending = (uint32_t)(ringHeader->tail - ringHeader->head);
    if (pending > ringEntryCount) {
        return FALSE;
    }

    return TRUE;
}

static __forceinline BOOLEAN AeroGpuLegacySubmitPathUsable(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter || !Adapter->Bar0 || !Adapter->RingVa || Adapter->RingEntryCount == 0) {
        return FALSE;
    }

    if (Adapter->Bar0Length < (AEROGPU_LEGACY_REG_RING_DOORBELL + sizeof(ULONG))) {
        return FALSE;
    }

    const ULONGLONG minRingBytes =
        (ULONGLONG)Adapter->RingEntryCount * (ULONGLONG)sizeof(aerogpu_legacy_ring_entry);
    if (minRingBytes > (ULONGLONG)Adapter->RingSizeBytes) {
        return FALSE;
    }

    return TRUE;
}

static NTSTATUS AeroGpuLegacyRingPushSubmit(_Inout_ AEROGPU_ADAPTER* Adapter,
                                            _In_ ULONG Fence,
                                            _In_ ULONG DescSize,
                                            _In_ PHYSICAL_ADDRESS DescPa)
{
    if (AeroGpuIsDeviceErrorLatched(Adapter)) {
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
        DxgkDevicePowerStateD0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (!AeroGpuLegacySubmitPathUsable(Adapter)) {
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_DEVICE_NOT_READY;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &oldIrql);

    /*
     * Re-check ring state under RingLock to avoid racing teardown (StopDevice ->
     * AeroGpuRingCleanup) between the initial check above and acquiring the lock.
     */
    if (!AeroGpuLegacySubmitPathUsable(Adapter)) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_DEVICE_NOT_READY;
    }

    /*
     * Re-check power/submission gating under RingLock: StopDevice may have flipped these after the
     * initial checks above but before we acquired the lock. Do not touch MMIO or ring memory if the
     * adapter is leaving D0.
     */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_DEVICE_NOT_READY;
    }

    ULONG head = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_RING_HEAD);
    AeroGpuLegacyRingUpdateHeadSeqLocked(Adapter, head);
    head = Adapter->LegacyRingHeadIndex;

    ULONG tail = Adapter->RingTail;
    if (tail >= Adapter->RingEntryCount) {
        /*
         * Defensive: RingTail is a masked index for the legacy ABI. If the cached value is
         * corrupted, resync it from the MMIO register to avoid out-of-bounds ring access.
         */
        tail = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_RING_TAIL);
        if (tail >= Adapter->RingEntryCount) {
            tail = 0;
        }
        Adapter->RingTail = tail;
        /*
         * Repair the monotonic tail sequence counter to match the observed masked indices.
         * Internal submission retirement relies on LegacyRingHeadSeq/LegacyRingTailSeq to be
         * consistent (no modulo arithmetic).
         */
        {
            const ULONG pending =
                (tail >= head) ? (tail - head) : (tail + Adapter->RingEntryCount - head);
            Adapter->LegacyRingTailSeq = Adapter->LegacyRingHeadSeq + pending;
        }
    }

    ULONG nextTail = (tail + 1) % Adapter->RingEntryCount;
    if (nextTail == head) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
    }

    aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)Adapter->RingVa;
    ring[tail].submit.type = AEROGPU_LEGACY_RING_ENTRY_SUBMIT;
    ring[tail].submit.flags = 0;
    ring[tail].submit.fence = Fence;
    ring[tail].submit.desc_size = DescSize;
    ring[tail].submit.desc_gpa = (uint64_t)DescPa.QuadPart;

    KeMemoryBarrier();
    /*
     * Publish the submitted fence before ringing the doorbell so the ISR can
     * associate any immediately-delivered IRQ_ERROR/IRQ_FENCE with a meaningful
     * LastSubmittedFence value.
     */
    AeroGpuAtomicWriteU64(&Adapter->LastSubmittedFence, (ULONGLONG)Fence);
    Adapter->RingTail = nextTail;
    Adapter->LegacyRingTailSeq += 1;
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_TAIL, Adapter->RingTail);
    AeroGpuWriteRegU32(Adapter, AEROGPU_LEGACY_REG_RING_DOORBELL, 1);

    KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
    return STATUS_SUCCESS;
}

static NTSTATUS AeroGpuV1RingPushSubmit(_Inout_ AEROGPU_ADAPTER* Adapter,
                                        _In_ uint32_t Flags,
                                        _In_ uint32_t ContextId,
                                        _In_ PHYSICAL_ADDRESS CmdPa,
                                        _In_ ULONG CmdSizeBytes,
                                        _In_ uint64_t AllocTableGpa,
                                        _In_ uint32_t AllocTableSizeBytes,
                                         _In_ ULONGLONG SignalFence,
                                         _Out_opt_ ULONG* RingTailAfterOut)
{
    if (AeroGpuIsDeviceErrorLatched(Adapter)) {
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
        DxgkDevicePowerStateD0) {
        return STATUS_DEVICE_NOT_READY;
    }
    if (InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return STATUS_DEVICE_NOT_READY;
    }

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->RingLock, &oldIrql);

    struct aerogpu_ring_header* ringHeader = (struct aerogpu_ring_header*)Adapter->RingVa;
    Adapter->RingHeader = ringHeader;

    /*
     * Validate ring state under RingLock to avoid racing teardown (StopDevice ->
     * AeroGpuRingCleanup) while we read ring header fields / touch the ring
     * buffer.
     */
    if (!AeroGpuV1SubmitPathUsable(Adapter)) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_DEVICE_NOT_READY;
    }

    /* Re-check power/submission gating under RingLock (StopDevice race). */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_DEVICE_NOT_READY;
    }

    const uint32_t head = ringHeader->head;
    const uint32_t tail = Adapter->RingTail;
    const uint32_t pending = tail - head;
    if (pending >= Adapter->RingEntryCount) {
        KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
        InterlockedIncrement64(&Adapter->PerfRingPushFailures);
        return STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
    }

    const uint32_t slot = tail & (Adapter->RingEntryCount - 1);
    struct aerogpu_submit_desc* desc =
        (struct aerogpu_submit_desc*)((PUCHAR)Adapter->RingVa + sizeof(struct aerogpu_ring_header) +
                                      ((SIZE_T)slot * sizeof(struct aerogpu_submit_desc)));

    RtlZeroMemory(desc, sizeof(*desc));
    desc->desc_size_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
    desc->flags = Flags;
    desc->context_id = ContextId;
    desc->engine_id = AEROGPU_ENGINE_0;
    desc->cmd_gpa = (uint64_t)CmdPa.QuadPart;
    desc->cmd_size_bytes = CmdSizeBytes;
    desc->alloc_table_gpa = AllocTableGpa;
    desc->alloc_table_size_bytes = AllocTableSizeBytes;
    desc->signal_fence = (uint64_t)SignalFence;

    KeMemoryBarrier();
    Adapter->RingTail = tail + 1;
    ringHeader->tail = Adapter->RingTail;
    KeMemoryBarrier();

    /*
     * Publish the submitted fence before ringing the doorbell so the ISR can
     * associate any immediately-delivered IRQ_ERROR/IRQ_FENCE with a meaningful
     * LastSubmittedFence value.
     */
    AeroGpuAtomicWriteU64(&Adapter->LastSubmittedFence, SignalFence);

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_DOORBELL, 1);

    if (RingTailAfterOut) {
        *RingTailAfterOut = Adapter->RingTail;
    }

    KeReleaseSpinLock(&Adapter->RingLock, oldIrql);
    return STATUS_SUCCESS;
}

static __forceinline VOID AeroGpuFreeInternalSubmission(_Inout_ AEROGPU_ADAPTER* Adapter,
                                                        _In_opt_ AEROGPU_PENDING_INTERNAL_SUBMISSION* Sub)
{
    if (!Adapter || !Sub) {
        return;
    }
    AeroGpuFreeContiguousNonCached(Adapter, Sub->CmdVa, Sub->CmdSizeBytes);
    AeroGpuFreeContiguousNonCached(Adapter, Sub->DescVa, Sub->DescSizeBytes);
    AeroGpuFreePendingInternalSubmission(Adapter, Sub);
}

static VOID AeroGpuFreeAllInternalSubmissions(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    for (;;) {
        AEROGPU_PENDING_INTERNAL_SUBMISSION* sub = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
        if (!IsListEmpty(&Adapter->PendingInternalSubmissions)) {
            PLIST_ENTRY entry = RemoveHeadList(&Adapter->PendingInternalSubmissions);
            sub = CONTAINING_RECORD(entry, AEROGPU_PENDING_INTERNAL_SUBMISSION, ListEntry);
        }
        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        if (!sub) {
            return;
        }

        AeroGpuFreeInternalSubmission(Adapter, sub);
    }
}

static VOID AeroGpuCleanupInternalSubmissions(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter) {
        return;
    }

    for (;;) {
        AEROGPU_PENDING_INTERNAL_SUBMISSION* sub = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);

        /*
         * Avoid touching ring state while the adapter is not in D0 or submissions are blocked
         * (resume/teardown windows). In these states:
         *  - Legacy devices can hang on MMIO reads, and
         *  - Ring memory can be in the process of being torn down.
         *
         * Internal submissions are drained during StopDevice/ResetFromTimeout or via the
         * SetPowerState(D0) "virtual reset" path.
         */
        if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange((volatile LONG*)&Adapter->DevicePowerState, 0, 0) !=
                DxgkDevicePowerStateD0 ||
            InterlockedCompareExchange((volatile LONG*)&Adapter->AcceptingSubmissions, 0, 0) == 0) {
            KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
            return;
        }

        ULONG head = 0;
        if (Adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            /*
             * For v1, ring head is in system memory (ring header). Still gate on the same
             * conditions as other MMIO/ring interactions so we don't race resume/teardown
             * windows where the ring may be reinitialised.
             */
            if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
                    DxgkDevicePowerStateD0 ||
                InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }

            if (!Adapter->RingVa || Adapter->RingEntryCount == 0 || Adapter->RingSizeBytes < sizeof(struct aerogpu_ring_header)) {
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }

            const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)Adapter->RingVa;
            const ULONG ringEntryCount = Adapter->RingEntryCount;
            const ULONGLONG minRingBytes =
                (ULONGLONG)sizeof(struct aerogpu_ring_header) +
                (ULONGLONG)ringEntryCount * (ULONGLONG)sizeof(struct aerogpu_submit_desc);
            if ((ringEntryCount & (ringEntryCount - 1)) != 0 ||
                minRingBytes > (ULONGLONG)Adapter->RingSizeBytes ||
                ringHeader->magic != AEROGPU_RING_MAGIC ||
                (ringHeader->abi_version >> 16) != AEROGPU_ABI_MAJOR ||
                ringHeader->entry_count != ringEntryCount ||
                ringHeader->entry_stride_bytes != sizeof(struct aerogpu_submit_desc) ||
                (ULONGLONG)ringHeader->size_bytes < minRingBytes ||
                ringHeader->size_bytes > (uint32_t)Adapter->RingSizeBytes) {
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }

            const uint32_t head32 = ringHeader->head;
            const uint32_t pending = (uint32_t)(ringHeader->tail - head32);
            if (pending > ringEntryCount) {
                /*
                 * Defensive: if head/tail are corrupted (e.g. device reset or guest memory clobber),
                 * avoid retiring internal submissions based on an invalid head value. Prematurely
                 * freeing internal submission buffers can lead to use-after-free when the device
                 * DMA-reads command buffers that are still referenced by the ring.
                 */
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }

            head = head32;
        } else {
            if (!Adapter->Bar0 || Adapter->RingEntryCount == 0) {
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }
            /*
             * Legacy ring head is device-owned (MMIO). Avoid MMIO reads unless the
             * adapter is in D0 and accepting submissions; DPCs can run during
             * resume/teardown windows where MMIO may be inaccessible.
             */
            if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
                    DxgkDevicePowerStateD0 ||
                InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0 ||
                Adapter->Bar0Length < (AEROGPU_LEGACY_REG_RING_HEAD + sizeof(ULONG))) {
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }
            KIRQL ringIrql;
            KeAcquireSpinLock(&Adapter->RingLock, &ringIrql);
            const ULONG headIndex = AeroGpuReadRegU32(Adapter, AEROGPU_LEGACY_REG_RING_HEAD);
            AeroGpuLegacyRingUpdateHeadSeqLocked(Adapter, headIndex);
            const ULONG pending = Adapter->LegacyRingTailSeq - Adapter->LegacyRingHeadSeq;
            if (pending > Adapter->RingEntryCount) {
                /*
                 * Defensive: legacy ring head/tail sequence tracking is expected to satisfy
                 * `tail_seq - head_seq <= RingEntryCount`. If this invariant is violated (e.g. due to
                 * device reset/tearing or corrupted cached indices), do not retire internal submissions
                 * based on the potentially-invalid head sequence. Prematurely freeing internal
                 * submission buffers can lead to use-after-free when the device later consumes stale
                 * descriptors.
                 */
                KeReleaseSpinLock(&Adapter->RingLock, ringIrql);
                KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
                return;
            }
            head = Adapter->LegacyRingHeadSeq;
            KeReleaseSpinLock(&Adapter->RingLock, ringIrql);
        }

        if (!IsListEmpty(&Adapter->PendingInternalSubmissions)) {
            PLIST_ENTRY entry = Adapter->PendingInternalSubmissions.Flink;
            AEROGPU_PENDING_INTERNAL_SUBMISSION* candidate =
                CONTAINING_RECORD(entry, AEROGPU_PENDING_INTERNAL_SUBMISSION, ListEntry);
            if ((LONG)(head - candidate->RingTailAfter) >= 0) {
                RemoveEntryList(&candidate->ListEntry);
                sub = candidate;
            }
        }
        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        if (!sub) {
            return;
        }

        AeroGpuFreeInternalSubmission(Adapter, sub);
    }
}

static __forceinline ULONGLONG AeroGpuSubmissionTotalBytes(_In_ const AEROGPU_SUBMISSION* Sub)
{
    if (!Sub) {
        return 0;
    }
    return (ULONGLONG)Sub->DmaCopySize + (ULONGLONG)Sub->AllocTableSizeBytes + (ULONGLONG)Sub->DescSize;
}

static VOID AeroGpuFreeSubmission(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ AEROGPU_SUBMISSION* Sub)
{
    if (!Adapter || !Sub) {
        return;
    }
    AeroGpuFreeContiguousNonCached(Adapter, Sub->AllocTableVa, (SIZE_T)Sub->AllocTableSizeBytes);
    AeroGpuFreeContiguousNonCached(Adapter, Sub->DmaCopyVa, Sub->DmaCopySize);
    AeroGpuFreeContiguousNonCached(Adapter, Sub->DescVa, Sub->DescSize);
    ExFreePoolWithTag(Sub, AEROGPU_POOL_TAG);
}

static BOOLEAN AeroGpuTryCopyFromSubmissionList(_In_ const LIST_ENTRY* ListHead,
                                                _In_ ULONGLONG Gpa,
                                                _In_ ULONG ReqBytes,
                                                _Out_writes_bytes_(ReqBytes) PUCHAR Out,
                                                _Inout_ ULONG* BytesToCopyInOut,
                                                _Inout_ NTSTATUS* OpStatusInOut)
{
    if (!ListHead || !Out || !BytesToCopyInOut || !OpStatusInOut) {
        return FALSE;
    }

    for (PLIST_ENTRY entry = ListHead->Flink; entry != ListHead; entry = entry->Flink) {
        const AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
        if (!sub) {
            continue;
        }

        struct range {
            ULONGLONG Base;
            ULONGLONG Size;
            const void* Va;
        } ranges[] = {
            {(ULONGLONG)sub->DmaCopyPa.QuadPart, (ULONGLONG)sub->DmaCopySize, sub->DmaCopyVa},
            {(ULONGLONG)sub->DescPa.QuadPart, (ULONGLONG)sub->DescSize, sub->DescVa},
            {(ULONGLONG)sub->AllocTablePa.QuadPart, (ULONGLONG)sub->AllocTableSizeBytes, sub->AllocTableVa},
        };

        for (UINT i = 0; i < (UINT)(sizeof(ranges) / sizeof(ranges[0])); ++i) {
            if (!ranges[i].Va || ranges[i].Size == 0) {
                continue;
            }
            const ULONGLONG base = ranges[i].Base;
            const ULONGLONG size = ranges[i].Size;
            if (Gpa < base) {
                continue;
            }
            const ULONGLONG offset = Gpa - base;
            if (offset >= size) {
                continue;
            }
            const ULONGLONG maxBytesU64 = size - offset;
            ULONG bytesToCopy = (maxBytesU64 < (ULONGLONG)ReqBytes) ? (ULONG)maxBytesU64 : ReqBytes;
            if (bytesToCopy != ReqBytes) {
                *OpStatusInOut = STATUS_PARTIAL_COPY;
            }
            RtlCopyMemory(Out, (const PUCHAR)ranges[i].Va + (SIZE_T)offset, bytesToCopy);
            *BytesToCopyInOut = bytesToCopy;
            return TRUE;
        }
    }

    return FALSE;
}

static VOID AeroGpuFreeAllPendingSubmissions(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);

    while (!IsListEmpty(&Adapter->PendingSubmissions)) {
        PLIST_ENTRY entry = RemoveHeadList(&Adapter->PendingSubmissions);
        AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);

        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        AeroGpuFreeSubmission(Adapter, sub);

        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
    }

    while (!IsListEmpty(&Adapter->RecentSubmissions)) {
        PLIST_ENTRY entry = RemoveHeadList(&Adapter->RecentSubmissions);
        AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
        const ULONGLONG bytes = AeroGpuSubmissionTotalBytes(sub);
        if (Adapter->RecentSubmissionCount != 0) {
            Adapter->RecentSubmissionCount -= 1;
        }
        if (Adapter->RecentSubmissionBytes >= bytes) {
            Adapter->RecentSubmissionBytes -= bytes;
        } else {
            Adapter->RecentSubmissionBytes = 0;
        }

        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        AeroGpuFreeSubmission(Adapter, sub);

        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
    }

    Adapter->RecentSubmissionCount = 0;
    Adapter->RecentSubmissionBytes = 0;

    KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);
}

static VOID AeroGpuRetireSubmissionsUpToFence(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG CompletedFence)
{
    for (;;) {
        AEROGPU_SUBMISSION* retired = NULL;
        LIST_ENTRY toFree;
        InitializeListHead(&toFree);

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);
        if (!IsListEmpty(&Adapter->PendingSubmissions)) {
            PLIST_ENTRY entry = Adapter->PendingSubmissions.Flink;
            AEROGPU_SUBMISSION* candidate = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
            if (candidate->Fence <= CompletedFence) {
                RemoveEntryList(entry);
                retired = candidate;
            }
        }

        if (retired) {
            const ULONGLONG bytes = AeroGpuSubmissionTotalBytes(retired);
            if (bytes == 0 || bytes > AEROGPU_DBGCTL_RECENT_SUBMISSIONS_MAX_BYTES) {
                InsertTailList(&toFree, &retired->ListEntry);
            } else {
                InsertTailList(&Adapter->RecentSubmissions, &retired->ListEntry);
                Adapter->RecentSubmissionCount += 1;
                Adapter->RecentSubmissionBytes += bytes;
            }

            while (Adapter->RecentSubmissionCount > AEROGPU_DBGCTL_RECENT_SUBMISSIONS_MAX_COUNT ||
                   Adapter->RecentSubmissionBytes > AEROGPU_DBGCTL_RECENT_SUBMISSIONS_MAX_BYTES) {
                PLIST_ENTRY e = RemoveHeadList(&Adapter->RecentSubmissions);
                AEROGPU_SUBMISSION* oldSub = CONTAINING_RECORD(e, AEROGPU_SUBMISSION, ListEntry);
                const ULONGLONG oldBytes = AeroGpuSubmissionTotalBytes(oldSub);
                if (Adapter->RecentSubmissionCount != 0) {
                    Adapter->RecentSubmissionCount -= 1;
                }
                if (Adapter->RecentSubmissionBytes >= oldBytes) {
                    Adapter->RecentSubmissionBytes -= oldBytes;
                } else {
                    Adapter->RecentSubmissionBytes = 0;
                }
                InsertTailList(&toFree, e);
            }
        }
        KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

        while (!IsListEmpty(&toFree)) {
            PLIST_ENTRY e = RemoveHeadList(&toFree);
            AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(e, AEROGPU_SUBMISSION, ListEntry);
            AeroGpuFreeSubmission(Adapter, sub);
        }

        if (!retired) {
            break;
        }
    }
}

static VOID AeroGpuAllocationUnmapCpu(_Inout_ AEROGPU_ALLOCATION* Alloc)
{
    if (!Alloc) {
        return;
    }

    if (Alloc->CpuMapUserVa && Alloc->CpuMapMdl) {
        MmUnmapLockedPages(Alloc->CpuMapUserVa, Alloc->CpuMapMdl);
    }

    if (Alloc->CpuMapMdl) {
        IoFreeMdl(Alloc->CpuMapMdl);
    }

    if (Alloc->CpuMapKernelVa && Alloc->CpuMapSize) {
        MmUnmapIoSpace(Alloc->CpuMapKernelVa, Alloc->CpuMapSize);
    }

    Alloc->CpuMapRefCount = 0;
    Alloc->CpuMapUserVa = NULL;
    Alloc->CpuMapKernelVa = NULL;
    Alloc->CpuMapMdl = NULL;
    Alloc->CpuMapSize = 0;
    Alloc->CpuMapPageOffset = 0;
    Alloc->CpuMapWritePending = FALSE;
}

static __forceinline BOOLEAN AeroGpuAllocationHasCpuMapResources(_In_ const AEROGPU_ALLOCATION* Alloc)
{
    if (!Alloc) {
        return FALSE;
    }

    /* Best-effort/unsafe inspection (can be called above PASSIVE_LEVEL). */
    if (Alloc->CpuMapMdl != NULL || Alloc->CpuMapKernelVa != NULL || Alloc->CpuMapUserVa != NULL) {
        return TRUE;
    }
    if (InterlockedCompareExchange((volatile LONG*)&Alloc->CpuMapRefCount, 0, 0) != 0) {
        return TRUE;
    }

    return FALSE;
}

static VOID AeroGpuAllocationDeferredFreeWorkItem(_In_ PVOID Context)
{
    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)Context;
    if (!alloc) {
        return;
    }

    ASSERT(KeGetCurrentIrql() == PASSIVE_LEVEL);

    ExAcquireFastMutex(&alloc->CpuMapMutex);
    AeroGpuAllocationUnmapCpu(alloc);
    ExReleaseFastMutex(&alloc->CpuMapMutex);

    ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
}

static VOID AeroGpuAllocationQueueDeferredFree(_Inout_ AEROGPU_ALLOCATION* Alloc)
{
    if (!Alloc) {
        return;
    }

    const KIRQL irql = KeGetCurrentIrql();
    if (irql > DISPATCH_LEVEL) {
        /*
         * Cannot queue a work item at IRQL > DISPATCH_LEVEL. Leak the allocation
         * rather than freeing it with CPU mapping resources still present.
         */
        AEROGPU_LOG("Allocation free: cannot defer free at IRQL=%lu (>DISPATCH), leaking allocation=%p alloc_id=%lu",
                    (ULONG)irql,
                    Alloc,
                    (ULONG)Alloc->AllocationId);
        return;
    }

    if (InterlockedCompareExchange(&Alloc->DeferredFreeQueued, 1, 0) != 0) {
        return;
    }

    ExInitializeWorkItem(&Alloc->DeferredFreeWorkItem, AeroGpuAllocationDeferredFreeWorkItem, Alloc);
    ExQueueWorkItem(&Alloc->DeferredFreeWorkItem, DelayedWorkQueue);
}

static ULONG AeroGpuShareTokenRefIncrementLocked(_Inout_ AEROGPU_ADAPTER* Adapter,
                                                 _In_ ULONGLONG ShareToken,
                                                 _Inout_ KIRQL* OldIrqlInOut,
                                                 _Outptr_result_maybenull_ AEROGPU_SHARE_TOKEN_REF** ToFreeOut)
{
    if (!Adapter || ShareToken == 0) {
        return 0;
    }
    if (!OldIrqlInOut) {
        return 0;
    }
    if (ToFreeOut) {
        *ToFreeOut = NULL;
    }

    /*
     * Assumes Adapter->AllocationsLock is held by the caller on entry and that it
     * should still be held on return.
     *
     * Note: this helper may temporarily release and re-acquire AllocationsLock
     * when inserting a new share-token tracking node. Callers must not rely on
     * uninterrupted lock ownership across this call.
     *
     * Avoid pool allocation/free while holding the spin lock (NonPagedPool is
     * legal at DISPATCH_LEVEL, but can increase hold time and contention).
     */
    for (PLIST_ENTRY it = Adapter->ShareTokenRefs.Flink; it != &Adapter->ShareTokenRefs; it = it->Flink) {
        AEROGPU_SHARE_TOKEN_REF* node = CONTAINING_RECORD(it, AEROGPU_SHARE_TOKEN_REF, ListEntry);
        if (node->ShareToken == ShareToken) {
            node->OpenCount += 1;
            return node->OpenCount;
        }
    }

    KeReleaseSpinLock(&Adapter->AllocationsLock, *OldIrqlInOut);

    AEROGPU_SHARE_TOKEN_REF* node =
        (AEROGPU_SHARE_TOKEN_REF*)ExAllocateFromNPagedLookasideList(&Adapter->ShareTokenRefLookaside);
    if (!node) {
        KeAcquireSpinLock(&Adapter->AllocationsLock, OldIrqlInOut);
        /*
         * Re-check under the lock: another thread may have inserted this token
         * while we were allocating. In that case, we can still bump the refcount
         * without needing to allocate a new node.
         */
        for (PLIST_ENTRY it = Adapter->ShareTokenRefs.Flink; it != &Adapter->ShareTokenRefs; it = it->Flink) {
            AEROGPU_SHARE_TOKEN_REF* existing = CONTAINING_RECORD(it, AEROGPU_SHARE_TOKEN_REF, ListEntry);
            if (existing->ShareToken == ShareToken) {
                existing->OpenCount += 1;
                return existing->OpenCount;
            }
        }
        return 0;
    }
    RtlZeroMemory(node, sizeof(*node));
    node->ShareToken = ShareToken;
    node->OpenCount = 1;

    KeAcquireSpinLock(&Adapter->AllocationsLock, OldIrqlInOut);

    /*
     * Re-check under the lock in case another thread inserted this token while we
     * were allocating.
     */
    for (PLIST_ENTRY it = Adapter->ShareTokenRefs.Flink; it != &Adapter->ShareTokenRefs; it = it->Flink) {
        AEROGPU_SHARE_TOKEN_REF* existing = CONTAINING_RECORD(it, AEROGPU_SHARE_TOKEN_REF, ListEntry);
        if (existing->ShareToken == ShareToken) {
            existing->OpenCount += 1;
            const ULONG openCount = existing->OpenCount;
            /*
             * Another thread inserted this token while we were allocating. Hand the
             * unused node back to the caller to free outside AllocationsLock.
             */
            if (ToFreeOut) {
                *ToFreeOut = node;
                return openCount;
            }

            /*
             * Avoid leaking the unused node if the caller is not collecting it.
             * Drop AllocationsLock to free outside the spin-locked region.
             */
            KeReleaseSpinLock(&Adapter->AllocationsLock, *OldIrqlInOut);
            ExFreeToNPagedLookasideList(&Adapter->ShareTokenRefLookaside, node);
            KeAcquireSpinLock(&Adapter->AllocationsLock, OldIrqlInOut);
            return openCount;
        }
    }

    InsertTailList(&Adapter->ShareTokenRefs, &node->ListEntry);
    return node->OpenCount;
}

static BOOLEAN AeroGpuShareTokenRefDecrement(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG ShareToken, _Out_ BOOLEAN* ShouldReleaseOut)
{
    if (ShouldReleaseOut) {
        *ShouldReleaseOut = FALSE;
    }

    if (!Adapter || ShareToken == 0) {
        return TRUE;
    }

    AEROGPU_SHARE_TOKEN_REF* toFree = NULL;
    ULONG newCount = 0;
    BOOLEAN found = FALSE;
    BOOLEAN shouldRelease = FALSE;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);

    for (PLIST_ENTRY it = Adapter->ShareTokenRefs.Flink; it != &Adapter->ShareTokenRefs; it = it->Flink) {
        AEROGPU_SHARE_TOKEN_REF* node = CONTAINING_RECORD(it, AEROGPU_SHARE_TOKEN_REF, ListEntry);
        if (node->ShareToken == ShareToken) {
            found = TRUE;
            if (node->OpenCount == 0) {
                newCount = 0;
            } else {
                node->OpenCount -= 1;
                newCount = node->OpenCount;
                if (node->OpenCount == 0) {
                    RemoveEntryList(&node->ListEntry);
                    toFree = node;
                    shouldRelease = TRUE;
                }
            }
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);

    if (!found) {
        AEROGPU_LOG("ShareTokenRef-- token=0x%I64x missing (already released?)", ShareToken);
        return FALSE;
    }

    if (shouldRelease) {
        AEROGPU_LOG("ShareTokenRef-- token=0x%I64x open_count=0 (final close)", ShareToken);
    } else {
        if (newCount == 0) {
            AEROGPU_LOG("ShareTokenRef-- token=0x%I64x underflow", ShareToken);
        } else {
            AEROGPU_LOG("ShareTokenRef-- token=0x%I64x open_count=%lu", ShareToken, newCount);
        }
    }

    if (toFree) {
        ExFreeToNPagedLookasideList(&Adapter->ShareTokenRefLookaside, toFree);
    }

    if (ShouldReleaseOut) {
        *ShouldReleaseOut = shouldRelease;
    }
    return TRUE;
}

static ULONGLONG AeroGpuGenerateShareToken(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    /*
     * 0 is reserved/invalid for share_token.
     *
     * Tokens are KMD-owned and monotonic within the adapter lifetime.
     */
    ULONGLONG token = (ULONGLONG)InterlockedIncrement64(&Adapter->NextShareToken);
    if (token == 0) {
        token = (ULONGLONG)InterlockedIncrement64(&Adapter->NextShareToken);
    }
    return token;
}

static VOID AeroGpuFreeAllShareTokenRefs(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    for (;;) {
        AEROGPU_SHARE_TOKEN_REF* node = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);
        if (!IsListEmpty(&Adapter->ShareTokenRefs)) {
            PLIST_ENTRY entry = RemoveHeadList(&Adapter->ShareTokenRefs);
            node = CONTAINING_RECORD(entry, AEROGPU_SHARE_TOKEN_REF, ListEntry);
        }
        KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);

        if (!node) {
            return;
        }

        ExFreeToNPagedLookasideList(&Adapter->ShareTokenRefLookaside, node);
    }
}

static VOID AeroGpuEmitReleaseSharedSurface(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ ULONGLONG ShareToken)
{
    if (!Adapter || ShareToken == 0) {
        return;
    }

    /*
     * Best-effort cleanup. Once the device has signaled IRQ_ERROR, avoid sending additional
     * commands to a potentially wedged device; the host side should clean up resources as part
     * of device reset/teardown.
     */
    if (AeroGpuIsDeviceErrorLatched(Adapter)) {
        return;
    }

    /*
     * This is a best-effort internal submission used to tell the host to release
     * a share_token mapping.
     *
     * Do not attempt to touch the ring/MMIO unless the adapter is powered (D0)
     * and accepting submissions; during sleep/disable transitions the ring may
     * be stopped and BAR state may be partially reset.
     */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return;
    }

    if (Adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        return;
    }

    {
        /*
         * AeroGpuV1SubmitPathUsable reads ring header fields; take RingLock so we don't race
         * AeroGpuRingCleanup during teardown.
         */
        KIRQL ringIrql;
        KeAcquireSpinLock(&Adapter->RingLock, &ringIrql);
        const BOOLEAN ringOk = AeroGpuV1SubmitPathUsable(Adapter);
        KeReleaseSpinLock(&Adapter->RingLock, ringIrql);
        if (!ringOk) {
            return;
        }
    }

    AEROGPU_PENDING_INTERNAL_SUBMISSION* internal = AeroGpuAllocPendingInternalSubmission(Adapter);
    if (!internal) {
#if DBG
        static volatile LONG g_ReleaseSharedSurfaceAllocFailLogs = 0;
        AEROGPU_LOG_RATELIMITED(g_ReleaseSharedSurfaceAllocFailLogs,
                                8,
                                "ReleaseSharedSurface: token=0x%I64x failed to allocate tracking node; skipping submit",
                                ShareToken);
#endif
        return;
    }

    const ULONG cmdSizeBytes =
        (ULONG)(sizeof(struct aerogpu_cmd_stream_header) + sizeof(struct aerogpu_cmd_release_shared_surface));
    PHYSICAL_ADDRESS cmdPa;
    cmdPa.QuadPart = 0;
    PVOID cmdVa = AeroGpuAllocContiguousNoInit(Adapter, cmdSizeBytes, &cmdPa);
    if (!cmdVa) {
        AeroGpuFreePendingInternalSubmission(Adapter, internal);
        return;
    }

    struct aerogpu_cmd_stream_header stream;
    RtlZeroMemory(&stream, sizeof(stream));
    stream.magic = AEROGPU_CMD_STREAM_MAGIC;
    stream.abi_version = AEROGPU_ABI_VERSION_U32;
    stream.size_bytes = (uint32_t)cmdSizeBytes;
    stream.flags = AEROGPU_CMD_STREAM_FLAG_NONE;
    stream.reserved0 = 0;
    stream.reserved1 = 0;

    struct aerogpu_cmd_release_shared_surface pkt;
    RtlZeroMemory(&pkt, sizeof(pkt));
    pkt.hdr.opcode = AEROGPU_CMD_RELEASE_SHARED_SURFACE;
    pkt.hdr.size_bytes = (uint32_t)sizeof(pkt);
    pkt.share_token = (uint64_t)ShareToken;
    pkt.reserved0 = 0;

    RtlCopyMemory(cmdVa, &stream, sizeof(stream));
    RtlCopyMemory((PUCHAR)cmdVa + sizeof(stream), &pkt, sizeof(pkt));

    ULONG ringTailAfter = 0;
    NTSTATUS st = STATUS_SUCCESS;
    {
        KIRQL pendingIrql;
        KeAcquireSpinLock(&Adapter->PendingLock, &pendingIrql);
        const ULONGLONG signalFence = AeroGpuAtomicReadU64(&Adapter->LastSubmittedFence);
        st = AeroGpuV1RingPushSubmit(Adapter,
                                     AEROGPU_SUBMIT_FLAG_NO_IRQ,
                                     0,
                                     cmdPa,
                                     cmdSizeBytes,
                                     0,
                                     0,
                                     signalFence,
                                     &ringTailAfter);

        if (NT_SUCCESS(st)) {
            internal->RingTailAfter = ringTailAfter;
            internal->Kind = AEROGPU_INTERNAL_SUBMISSION_KIND_RELEASE_SHARED_SURFACE;
            internal->ShareToken = ShareToken;
            internal->CmdVa = cmdVa;
            internal->CmdSizeBytes = cmdSizeBytes;
            InsertTailList(&Adapter->PendingInternalSubmissions, &internal->ListEntry);
        }
        KeReleaseSpinLock(&Adapter->PendingLock, pendingIrql);
    }
    if (!NT_SUCCESS(st)) {
        AeroGpuFreeContiguousNonCached(Adapter, cmdVa, cmdSizeBytes);
        AeroGpuFreePendingInternalSubmission(Adapter, internal);
        return;
    }

    /* Track internal submissions for dbgctl perf counters. */
    InterlockedIncrement64(&Adapter->PerfTotalSubmissions);
    InterlockedIncrement64(&Adapter->PerfTotalInternalSubmits);
}

static BOOLEAN AeroGpuTrackAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _Inout_ AEROGPU_ALLOCATION* Allocation)
{
    KIRQL oldIrql;
    AEROGPU_SHARE_TOKEN_REF* toFree = NULL;
    KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);
    /*
     * Increment share-token refs before making the allocation visible in
     * Adapter->Allocations. The increment helper may drop/re-acquire
     * AllocationsLock to allocate a tracking node.
     */
    const ULONG shareTokenCount = AeroGpuShareTokenRefIncrementLocked(Adapter, Allocation->ShareToken, &oldIrql, &toFree);
    const BOOLEAN ok = (Allocation->ShareToken == 0 || shareTokenCount != 0) ? TRUE : FALSE;
    if (ok) {
        InsertTailList(&Adapter->Allocations, &Allocation->ListEntry);
    }
    KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);

    if (toFree) {
        ExFreeToNPagedLookasideList(&Adapter->ShareTokenRefLookaside, toFree);
    }

    if (Allocation->ShareToken != 0) {
        if (shareTokenCount != 0) {
            AEROGPU_LOG("ShareTokenRef++ token=0x%I64x open_count=%lu", Allocation->ShareToken, shareTokenCount);
        } else {
            AEROGPU_LOG("ShareTokenRef++ token=0x%I64x failed (out of memory)", Allocation->ShareToken);
        }
    }
    return ok;
}

static BOOLEAN AeroGpuTryUntrackAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _In_ const AEROGPU_ALLOCATION* Allocation)
{
    BOOLEAN found = FALSE;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);

    for (PLIST_ENTRY entry = Adapter->Allocations.Flink; entry != &Adapter->Allocations; entry = entry->Flink) {
        const AEROGPU_ALLOCATION* candidate = CONTAINING_RECORD(entry, AEROGPU_ALLOCATION, ListEntry);
        if (candidate == Allocation) {
            RemoveEntryList(entry);
            found = TRUE;
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);
    return found;
}

static BOOLEAN AeroGpuUntrackAndFreeAllocation(_Inout_ AEROGPU_ADAPTER* Adapter, _In_opt_ HANDLE hAllocation)
{
    if (!hAllocation) {
        return FALSE;
    }

    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)hAllocation;
    if (!AeroGpuTryUntrackAllocation(Adapter, alloc)) {
        /*
         * Be tolerant of dxgkrnl calling CloseAllocation/DestroyAllocation in
         * different patterns. If the handle is already freed we should not
         * touch it again.
         */
        static LONG g_UntrackedAllocFreeWarned = 0;
        if (InterlockedExchange(&g_UntrackedAllocFreeWarned, 1) == 0) {
            AEROGPU_LOG("Allocation free: untracked handle=%p", hAllocation);
        }
        return FALSE;
    }

    const ULONGLONG shareToken = alloc->ShareToken;
    const KIRQL irql = KeGetCurrentIrql();

    if (irql == PASSIVE_LEVEL) {
        ExAcquireFastMutex(&alloc->CpuMapMutex);
        AeroGpuAllocationUnmapCpu(alloc);
        ExReleaseFastMutex(&alloc->CpuMapMutex);

        ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
    } else if (AeroGpuAllocationHasCpuMapResources(alloc)) {
        AEROGPU_LOG("Allocation free: deferring CPU unmap/free at IRQL=%lu allocation=%p alloc_id=%lu share_token=0x%I64x",
                    (ULONG)irql,
                    alloc,
                    (ULONG)alloc->AllocationId,
                    alloc->ShareToken);
        AeroGpuAllocationQueueDeferredFree(alloc);
    } else {
        ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
    }

    BOOLEAN shouldRelease = FALSE;
    if (shareToken != 0 && AeroGpuShareTokenRefDecrement(Adapter, shareToken, &shouldRelease) && shouldRelease) {
        AeroGpuEmitReleaseSharedSurface(Adapter, shareToken);
    }

    return TRUE;
}

static VOID AeroGpuFreeAllAllocations(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    const KIRQL irql = KeGetCurrentIrql();

    for (;;) {
        AEROGPU_ALLOCATION* alloc = NULL;

        KIRQL oldIrql;
        KeAcquireSpinLock(&Adapter->AllocationsLock, &oldIrql);
        if (!IsListEmpty(&Adapter->Allocations)) {
            PLIST_ENTRY entry = RemoveHeadList(&Adapter->Allocations);
            alloc = CONTAINING_RECORD(entry, AEROGPU_ALLOCATION, ListEntry);
        }
        KeReleaseSpinLock(&Adapter->AllocationsLock, oldIrql);

        if (!alloc) {
            return;
        }

        if (irql == PASSIVE_LEVEL) {
            ExAcquireFastMutex(&alloc->CpuMapMutex);
            AeroGpuAllocationUnmapCpu(alloc);
            ExReleaseFastMutex(&alloc->CpuMapMutex);
            ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
        } else if (AeroGpuAllocationHasCpuMapResources(alloc)) {
            AEROGPU_LOG("FreeAllAllocations: deferring CPU unmap/free at IRQL=%lu allocation=%p alloc_id=%lu share_token=0x%I64x",
                        (ULONG)irql,
                        alloc,
                        (ULONG)alloc->AllocationId,
                        alloc->ShareToken);
            AeroGpuAllocationQueueDeferredFree(alloc);
        } else {
            ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
        }
    }
}

static __forceinline BOOLEAN AeroGpuAllocTableContainsAllocId(_In_ const AEROGPU_SUBMISSION* Sub, _In_ ULONG AllocId)
{
    if (!Sub || !Sub->AllocTableVa || Sub->AllocTableSizeBytes < sizeof(struct aerogpu_alloc_table_header)) {
        return FALSE;
    }

    const struct aerogpu_alloc_table_header* hdr = (const struct aerogpu_alloc_table_header*)Sub->AllocTableVa;
    /*
     * Forward-compat: newer ABI minor versions may extend `aerogpu_alloc_entry` by increasing the
     * stride and appending fields. Only the entry prefix is required for alloc_id lookup.
     */
    if (hdr->magic != AEROGPU_ALLOC_TABLE_MAGIC || hdr->entry_stride_bytes < sizeof(struct aerogpu_alloc_entry)) {
        return FALSE;
    }

    const SIZE_T sizeBytes = hdr->size_bytes;
    if (sizeBytes > Sub->AllocTableSizeBytes || sizeBytes < sizeof(*hdr)) {
        return FALSE;
    }

    const SIZE_T entryStrideBytes = hdr->entry_stride_bytes;
    const SIZE_T maxEntries = (sizeBytes - sizeof(*hdr)) / entryStrideBytes;
    UINT count = hdr->entry_count;
    if ((SIZE_T)count > maxEntries) {
        count = (UINT)maxEntries;
    }

    const UINT8* entries = (const UINT8*)(hdr + 1);
    const uint32_t id = (uint32_t)AllocId;
    for (UINT i = 0; i < count; ++i) {
        const struct aerogpu_alloc_entry* entry =
            (const struct aerogpu_alloc_entry*)(entries + (SIZE_T)i * entryStrideBytes);
        if (entry->alloc_id == id) {
            return TRUE;
        }
    }

    return FALSE;
}

static BOOLEAN AeroGpuGetAllocationBusyFence(_Inout_ AEROGPU_ADAPTER* Adapter,
                                             _In_ const AEROGPU_ALLOCATION* Alloc,
                                             _Out_ ULONGLONG* BusyFenceOut)
{
    if (BusyFenceOut) {
        *BusyFenceOut = 0;
    }

    if (!Adapter || !Alloc || !BusyFenceOut) {
        return FALSE;
    }

    const ULONGLONG completedFence = AeroGpuReadCompletedFence(Adapter);
    ULONGLONG maxFence = 0;

    KIRQL oldIrql;
    KeAcquireSpinLock(&Adapter->PendingLock, &oldIrql);

    for (PLIST_ENTRY entry = Adapter->PendingSubmissions.Flink; entry != &Adapter->PendingSubmissions;
         entry = entry->Flink) {
        const AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
        if (sub->Fence <= completedFence) {
            continue;
        }

        if (!AeroGpuAllocTableContainsAllocId(sub, Alloc->AllocationId)) {
            continue;
        }

        if (sub->Fence > maxFence) {
            maxFence = sub->Fence;
        }
    }

    KeReleaseSpinLock(&Adapter->PendingLock, oldIrql);

    *BusyFenceOut = maxFence;
    return (maxFence != 0);
}

static NTSTATUS AeroGpuWaitForAllocationIdle(_Inout_ AEROGPU_ADAPTER* Adapter,
                                             _In_ const AEROGPU_ALLOCATION* Alloc,
                                             _In_ BOOLEAN DoNotWait)
{
    if (!Adapter || !Alloc) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (AeroGpuIsDeviceErrorLatched(Adapter)) {
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }

    /*
     * If the adapter is not in D0, avoid touching MMIO for fence polling.
     * The call sites for this helper are CPU-mapping paths (DxgkDdiLock) which
     * must not hang or fault when the device is powered down.
     */
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
            DxgkDevicePowerStateD0 ||
        InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
        return DoNotWait ? STATUS_GRAPHICS_GPU_BUSY : STATUS_DEVICE_NOT_READY;
    }

    for (;;) {
        ULONGLONG busyFence = 0;
        if (!AeroGpuGetAllocationBusyFence(Adapter, Alloc, &busyFence)) {
            return STATUS_SUCCESS;
        }

        if (DoNotWait) {
            /*
             * Win7 D3D10/11 runtimes translate this into DXGI_ERROR_WAS_STILL_DRAWING
             * for Map(D3D11_MAP_FLAG_DO_NOT_WAIT).
             */
            return STATUS_GRAPHICS_GPU_BUSY;
        }

        /*
         * Poll for the fence to complete. This is intentionally simple
         * (system-memory-only MVP, no paging) and keeps us from returning a CPU
         * VA while the emulator may still be writing the allocation.
         */
        while (AeroGpuReadCompletedFence(Adapter) < busyFence) {
            if (AeroGpuIsDeviceErrorLatched(Adapter)) {
                return STATUS_GRAPHICS_DEVICE_REMOVED;
            }

            /*
             * If the adapter is leaving D0 (sleep/hibernate, PnP disable, etc),
             * the completed fence value may stop advancing. Avoid hanging a
             * user-mode thread in a tight wait loop while the device is powered
             * down.
             */
            if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&Adapter->DevicePowerState, 0, 0) !=
                    DxgkDevicePowerStateD0 ||
                InterlockedCompareExchange(&Adapter->AcceptingSubmissions, 0, 0) == 0) {
                return STATUS_DEVICE_NOT_READY;
            }
            LARGE_INTEGER interval;
            interval.QuadPart = -10000; /* 1ms */
            KeDelayExecutionThread(KernelMode, FALSE, &interval);
        }
    }
}

/* ---- DxgkDdi* ----------------------------------------------------------- */

static ULONGLONG AeroGpuGetNonLocalMemorySizeBytes(_In_ const AEROGPU_ADAPTER* Adapter);

static NTSTATUS APIENTRY AeroGpuDdiAddDevice(_In_ PDEVICE_OBJECT PhysicalDeviceObject,
                                               _Outptr_ PVOID* MiniportDeviceContext)
{
    if (!MiniportDeviceContext) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_ADAPTER* adapter =
        (AEROGPU_ADAPTER*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*adapter), AEROGPU_POOL_TAG);
    if (!adapter) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(adapter, sizeof(*adapter));

    adapter->PhysicalDeviceObject = PhysicalDeviceObject;
    adapter->NonLocalMemorySizeBytes = AeroGpuGetNonLocalMemorySizeBytes(adapter);
    for (UINT i = 0; i < AEROGPU_ALLOC_TABLE_SCRATCH_SHARD_COUNT; ++i) {
        ExInitializeFastMutex(&adapter->AllocTableScratch[i].Mutex);
    }
    KeInitializeSpinLock(&adapter->RingLock);
    KeInitializeSpinLock(&adapter->IrqEnableLock);
    KeInitializeSpinLock(&adapter->PendingLock);
    InitializeListHead(&adapter->PendingSubmissions);
    InitializeListHead(&adapter->PendingInternalSubmissions);
    ExInitializeNPagedLookasideList(&adapter->PendingInternalSubmissionLookaside,
                                    NULL,
                                    NULL,
                                    0,
                                    sizeof(AEROGPU_PENDING_INTERNAL_SUBMISSION),
                                    AEROGPU_POOL_TAG,
                                    64);
    AeroGpuContigPoolInit(adapter);
    InitializeListHead(&adapter->RecentSubmissions);
    adapter->RecentSubmissionCount = 0;
    adapter->RecentSubmissionBytes = 0;
    KeInitializeSpinLock(&adapter->MetaHandleLock);
    InitializeListHead(&adapter->PendingMetaHandles);
    adapter->PendingMetaHandleCount = 0;
    adapter->PendingMetaHandleBytes = 0;
    adapter->NextMetaHandle = 0;
    KeInitializeSpinLock(&adapter->AllocationsLock);
    KeInitializeSpinLock(&adapter->CreateAllocationTraceLock);
    KeInitializeSpinLock(&adapter->CursorLock);
    InitializeListHead(&adapter->Allocations);
    InitializeListHead(&adapter->ShareTokenRefs);
    ExInitializeNPagedLookasideList(
        &adapter->ShareTokenRefLookaside, NULL, NULL, 0, sizeof(AEROGPU_SHARE_TOKEN_REF), AEROGPU_POOL_TAG, 128);

    KeInitializeSpinLock(&adapter->SharedHandleTokenLock);
    InitializeListHead(&adapter->SharedHandleTokens);
    adapter->NextSharedHandleToken = 0;
    adapter->SharedHandleTokenCount = 0;

    adapter->CurrentWidth = 1024;
    adapter->CurrentHeight = 768;
    adapter->CurrentPitch = 1024 * 4;
    adapter->CurrentFormat = AEROGPU_FORMAT_B8G8R8X8_UNORM;
    adapter->SourceVisible = TRUE;
    adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

    /*
     * Prefer the EDID's detailed timing descriptor as the default cached mode.
     *
     * The display stack may query standard allocation sizing before it has
     * committed a VidPN; defaulting to the EDID preferred mode avoids allocating
     * an obviously wrong primary surface (which can cause scanline/vblank sanity
     * checks to fail in real Win7 guests).
     */
    {
        AEROGPU_DISPLAY_MODE modes[16];
        const UINT modeCount = AeroGpuBuildModeList(modes, (UINT)(sizeof(modes) / sizeof(modes[0])));
        if (modeCount != 0) {
            adapter->CurrentWidth = modes[0].Width;
            adapter->CurrentHeight = modes[0].Height;

            ULONG pitch = 0;
            if (AeroGpuComputeDefaultPitchBytes(adapter->CurrentWidth, &pitch)) {
                adapter->CurrentPitch = pitch;
            } else if (adapter->CurrentWidth != 0 && adapter->CurrentWidth <= (0xFFFFFFFFu / 4u)) {
                adapter->CurrentPitch = adapter->CurrentWidth * 4u;
            }
        }
    }

    /*
     * Initialise so that the first InterlockedIncrement() yields
     * AEROGPU_WDDM_ALLOC_ID_KMD_MIN.
     */
    adapter->NextKmdAllocId = (LONG)AEROGPU_WDDM_ALLOC_ID_UMD_MAX;
    InterlockedExchange64(&adapter->NextShareToken, 0);

    *MiniportDeviceContext = adapter;
    AEROGPU_LOG0("AddDevice");
    return STATUS_SUCCESS;
}

static BOOLEAN AeroGpuExtractMemoryResource(_In_ const CM_PARTIAL_RESOURCE_DESCRIPTOR* desc,
                                            _Out_ PHYSICAL_ADDRESS* startOut,
                                            _Out_ ULONG* lengthOut)
{
    USHORT large;
    ULONGLONG lenBytes;

    if (startOut != NULL) {
        startOut->QuadPart = 0;
    }
    if (lengthOut != NULL) {
        *lengthOut = 0;
    }

    if (desc == NULL || startOut == NULL || lengthOut == NULL) {
        return FALSE;
    }

    lenBytes = 0;

    if (desc->Type == CmResourceTypeMemory) {
        *startOut = desc->u.Memory.Start;
        *lengthOut = desc->u.Memory.Length;
        return TRUE;
    }

    if (desc->Type == CmResourceTypeMemoryLarge) {
        large = desc->Flags & (CM_RESOURCE_MEMORY_LARGE_40 | CM_RESOURCE_MEMORY_LARGE_48 | CM_RESOURCE_MEMORY_LARGE_64);
        switch (large) {
            case CM_RESOURCE_MEMORY_LARGE_40:
                *startOut = desc->u.Memory40.Start;
                lenBytes = ((ULONGLONG)desc->u.Memory40.Length40) << 8;
                break;
            case CM_RESOURCE_MEMORY_LARGE_48:
                *startOut = desc->u.Memory48.Start;
                lenBytes = ((ULONGLONG)desc->u.Memory48.Length48) << 16;
                break;
            case CM_RESOURCE_MEMORY_LARGE_64:
                *startOut = desc->u.Memory64.Start;
                lenBytes = ((ULONGLONG)desc->u.Memory64.Length64) << 32;
                break;
            default:
                return FALSE;
        }

        if (lenBytes > 0xFFFFFFFFull) {
            return FALSE;
        }

        *lengthOut = (ULONG)lenBytes;
        return TRUE;
    }

    return FALSE;
}

static NTSTATUS APIENTRY AeroGpuDdiStartDevice(_In_ const PVOID MiniportDeviceContext,
                                               _In_ PDXGK_START_INFO DxgkStartInfo,
                                               _In_ PDXGKRNL_INTERFACE DxgkInterface,
                                               _Out_ PULONG NumberOfVideoPresentSources,
                                               _Out_ PULONG NumberOfChildren)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter || !DxgkStartInfo || !DxgkInterface || !NumberOfVideoPresentSources || !NumberOfChildren) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Clear any KMD-side latched "device error" state recorded from IRQ_ERROR. */
    InterlockedExchange(&adapter->DeviceErrorLatched, 0);
    /*
     * Ensure the next IRQ_ERROR can be surfaced to dxgkrnl even if the OS reuses
     * fence IDs across adapter restarts (TDR / PnP stop-start).
     */
    AeroGpuAtomicWriteU64(&adapter->LastNotifiedErrorFence, (ULONGLONG)(LONGLONG)-1);

    adapter->StartInfo = *DxgkStartInfo;
    adapter->DxgkInterface = *DxgkInterface;

    *NumberOfVideoPresentSources = 1;
    *NumberOfChildren = 1;

    PCM_RESOURCE_LIST resList = DxgkStartInfo->TranslatedResourceList;
    if (!resList || resList->Count < 1) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    adapter->Bar0 = NULL;
    adapter->Bar0Length = 0;

    /*
     * BAR0 discovery:
     *
     * Canonical AeroGPU exposes both:
     *   - BAR0: small MMIO register block ("AGPU" magic)
     *   - BAR1: large prefetchable VRAM aperture
     *
     * Windows does not guarantee resource ordering, so do not assume the first
     * translated memory resource is BAR0. Instead, probe each translated memory
     * resource for the expected MMIO magic at offset AEROGPU_MMIO_REG_MAGIC.
     *
     * We intentionally map only a tiny probe window for each candidate (enough
     * to read the ABI discovery registers) so we don't temporarily map a large
     * BAR1/VRAM aperture just to reject it.
     */
    enum {
        /* Includes MAGIC/ABI_VERSION/FEATURES_LO/FEATURES_HI. */
        AEROGPU_BAR0_PROBE_BYTES = (AEROGPU_MMIO_REG_FEATURES_HI + sizeof(ULONG)),
    };

    ULONG memResourceCount = 0;

    BOOLEAN haveFirstCandidate = FALSE;
    PHYSICAL_ADDRESS firstStart;
    ULONG firstLength = 0;

    BOOLEAN haveLegacyCandidate = FALSE;
    PHYSICAL_ADDRESS legacyStart;
    ULONG legacyLength = 0;

    BOOLEAN haveAgpuCandidate = FALSE;
    PHYSICAL_ADDRESS agpuStart;
    ULONG agpuLength = 0;

#if DBG
    ULONG probedCount = 0;

    ULONG firstMagic = 0;
    ULONG firstFullIndex = 0;
    ULONG firstPartialIndex = 0;
    ULONG firstMemOrdinal = 0;

    ULONG legacyMagic = 0;
    ULONG legacyFullIndex = 0;
    ULONG legacyPartialIndex = 0;
    ULONG legacyMemOrdinal = 0;

    ULONG agpuMagic = 0;
    ULONG agpuFullIndex = 0;
    ULONG agpuPartialIndex = 0;
    ULONG agpuMemOrdinal = 0;
#endif

    firstStart.QuadPart = 0;
    legacyStart.QuadPart = 0;
    agpuStart.QuadPart = 0;

    for (ULONG fi = 0; fi < resList->Count; ++fi) {
        PCM_FULL_RESOURCE_DESCRIPTOR full = &resList->List[fi];
        PCM_PARTIAL_RESOURCE_LIST partial = &full->PartialResourceList;
        for (ULONG pi = 0; pi < partial->Count; ++pi) {
            PCM_PARTIAL_RESOURCE_DESCRIPTOR desc = &partial->PartialDescriptors[pi];
            PHYSICAL_ADDRESS start;
            ULONG length;
            if (!AeroGpuExtractMemoryResource(desc, &start, &length)) {
                continue;
            }

#if DBG
            const ULONG memOrdinal = memResourceCount;
#endif
            memResourceCount++;

#if DBG
            BOOLEAN isFirstCandidate = FALSE;
#endif

            if (!haveFirstCandidate) {
                haveFirstCandidate = TRUE;
                firstStart = start;
                firstLength = length;
#if DBG
                isFirstCandidate = TRUE;
                firstFullIndex = fi;
                firstPartialIndex = pi;
                firstMemOrdinal = memOrdinal;
#endif
            }

            if (length < sizeof(ULONG)) {
#if DBG
                AEROGPU_LOG("StartDevice: BAR0 probe skip mem[%lu] full=%lu partial=%lu start=0x%I64x len=%lu (too small)",
                            memOrdinal,
                            fi,
                            pi,
                            (unsigned long long)start.QuadPart,
                            length);
#endif
                continue;
            }

            const SIZE_T probeBytes = (length < (ULONG)AEROGPU_BAR0_PROBE_BYTES) ? (SIZE_T)length : (SIZE_T)AEROGPU_BAR0_PROBE_BYTES;
            PUCHAR probeVa = (PUCHAR)MmMapIoSpace(start, probeBytes, MmNonCached);
            if (!probeVa) {
#if DBG
                AEROGPU_LOG("StartDevice: BAR0 probe map failed mem[%lu] full=%lu partial=%lu start=0x%I64x len=%lu probe=%Iu",
                            memOrdinal,
                            fi,
                            pi,
                            (unsigned long long)start.QuadPart,
                            length,
                            probeBytes);
#endif
                continue;
            }

            const ULONG magic = READ_REGISTER_ULONG((volatile ULONG*)(probeVa + AEROGPU_MMIO_REG_MAGIC));
#if DBG
            probedCount++;
            if (isFirstCandidate) {
                firstMagic = magic;
            }
#endif

            MmUnmapIoSpace(probeVa, probeBytes);
            probeVa = NULL;

            if (magic == AEROGPU_MMIO_MAGIC) {
                haveAgpuCandidate = TRUE;
                agpuStart = start;
                agpuLength = length;
#if DBG
                agpuMagic = magic;
                agpuFullIndex = fi;
                agpuPartialIndex = pi;
                agpuMemOrdinal = memOrdinal;
#endif
                goto Bar0ProbeDone;
            }

            if (!haveLegacyCandidate && magic == AEROGPU_LEGACY_MMIO_MAGIC) {
                haveLegacyCandidate = TRUE;
                legacyStart = start;
                legacyLength = length;
#if DBG
                legacyMagic = magic;
                legacyFullIndex = fi;
                legacyPartialIndex = pi;
                legacyMemOrdinal = memOrdinal;
#endif
            }
        }
    }

Bar0ProbeDone:
    /*
     * Selection order:
     *   1) New ABI ("AGPU") magic if found.
     *   2) Legacy ("ARGP") magic if found (helps older device models with BAR1).
     *   3) Fall back to the first memory resource only when it is unambiguous (single
     *      translated memory resource); otherwise fail.
     */
    PHYSICAL_ADDRESS selectedStart;
    ULONG selectedLength = 0;

    selectedStart.QuadPart = 0;

    if (haveAgpuCandidate) {
        selectedStart = agpuStart;
        selectedLength = agpuLength;
    } else if (haveLegacyCandidate) {
        selectedStart = legacyStart;
        selectedLength = legacyLength;
    } else if (haveFirstCandidate && memResourceCount == 1) {
        AEROGPU_LOG0("StartDevice: BAR0 magic not found; falling back to first memory resource");
        selectedStart = firstStart;
        selectedLength = firstLength;
    }

    if (selectedLength == 0) {
        AEROGPU_LOG("StartDevice: BAR0 could not be identified (no MMIO magic match across %lu memory resources)",
                    memResourceCount);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

#if DBG
    ULONG selectedMagic = 0;
    ULONG selectedFullIndex = 0;
    ULONG selectedPartialIndex = 0;
    ULONG selectedMemOrdinal = 0;

    if (haveAgpuCandidate) {
        selectedMagic = agpuMagic;
        selectedFullIndex = agpuFullIndex;
        selectedPartialIndex = agpuPartialIndex;
        selectedMemOrdinal = agpuMemOrdinal;
    } else if (haveLegacyCandidate) {
        selectedMagic = legacyMagic;
        selectedFullIndex = legacyFullIndex;
        selectedPartialIndex = legacyPartialIndex;
        selectedMemOrdinal = legacyMemOrdinal;
    } else if (haveFirstCandidate) {
        selectedMagic = firstMagic;
        selectedFullIndex = firstFullIndex;
        selectedPartialIndex = firstPartialIndex;
        selectedMemOrdinal = firstMemOrdinal;
    }

    AEROGPU_LOG("StartDevice: BAR0 probe inspected %lu memory resources (probed %lu); selected mem[%lu] full=%lu partial=%lu start=0x%I64x len=%lu magic=0x%08lx",
                memResourceCount,
                probedCount,
                selectedMemOrdinal,
                selectedFullIndex,
                selectedPartialIndex,
                (unsigned long long)selectedStart.QuadPart,
                selectedLength,
                selectedMagic);
#endif
    adapter->Bar0Length = selectedLength;
    adapter->Bar0 = (PUCHAR)MmMapIoSpace(selectedStart, adapter->Bar0Length, MmNonCached);

    if (!adapter->Bar0) {
        AEROGPU_LOG("StartDevice: MmMapIoSpace failed for BAR0 (len=%lu)", adapter->Bar0Length);
        adapter->Bar0Length = 0;
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    if (adapter->Bar0Length < sizeof(ULONG)) {
        AEROGPU_LOG("StartDevice: BAR0 too small (%lu bytes)", adapter->Bar0Length);
        AeroGpuUnmapBar0(adapter);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    /* StartDevice implies the adapter is entering D0. Keep submissions blocked until init completes. */
    InterlockedExchange(&adapter->DevicePowerState, (LONG)DxgkDevicePowerStateD0);
    InterlockedExchange(&adapter->AcceptingSubmissions, 0);

    /*
     * Reset fence bookkeeping on each (re)start so v1 ring submissions always begin from a
     * well-defined 64-bit fence extension epoch.
     */
    AeroGpuAtomicWriteU64(&adapter->LastSubmittedFence, 0);
    AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, 0);
    adapter->V1FenceEpoch = 0;
    adapter->V1LastFence32 = 0;

    const ULONG magic = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_MAGIC);
    ULONG abiVersion = 0;
    ULONGLONG features = 0;

    adapter->DeviceMmioMagic = magic;
    adapter->DeviceAbiVersion = 0;

    /*
     * ABI detection: treat the versioned "AGPU" MMIO magic as the new ABI, and
     * fall back to the legacy register map otherwise.
     *
     * This keeps older emulator device models working even if they don't report
     * the expected legacy magic value.
     */
    adapter->AbiKind = AEROGPU_ABI_KIND_LEGACY;
    adapter->UsingNewAbi = FALSE;
    if (magic == AEROGPU_MMIO_MAGIC) {
        if (adapter->Bar0Length < (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
            AEROGPU_LOG("StartDevice: BAR0 too small (%lu bytes) for AGPU ABI", adapter->Bar0Length);
            AeroGpuUnmapBar0(adapter);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        adapter->AbiKind = AEROGPU_ABI_KIND_V1;
        adapter->UsingNewAbi = TRUE;

        abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
        const ULONG abiMajor = abiVersion >> 16;
        if (abiMajor != AEROGPU_ABI_MAJOR) {
            AEROGPU_LOG("StartDevice: unsupported ABI major=%lu (abi=0x%08lx)", abiMajor, abiVersion);
            AeroGpuUnmapBar0(adapter);
            return STATUS_NOT_SUPPORTED;
        }

        features = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                   ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);

        if ((features & AEROGPU_FEATURE_VBLANK) != 0 &&
            adapter->Bar0Length < (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG))) {
            AEROGPU_LOG("StartDevice: BAR0 too small (%lu bytes) for vblank regs", adapter->Bar0Length);
            AeroGpuUnmapBar0(adapter);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        AEROGPU_LOG("StartDevice: ABI=v1 magic=0x%08lx (new) abi=0x%08lx features=0x%I64x",
                    magic,
                    abiVersion,
                    (unsigned long long)features);
    } else {
        if (adapter->Bar0Length < (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
            AEROGPU_LOG("StartDevice: BAR0 too small (%lu bytes) for legacy ABI", adapter->Bar0Length);
            AeroGpuUnmapBar0(adapter);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
        /*
         * Legacy devices do not guarantee FEATURES_LO/HI exist, but some bring-up
         * models expose them (mirroring `drivers/aerogpu/protocol/aerogpu_pci.h`) to
         * allow incremental migration of optional capabilities like vblank.
         *
         * Reuse the dbgctl "plausibility" guard: only accept the value if it
         * contains no unknown bits.
         */
        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_FEATURES_HI + sizeof(ULONG))) {
            const ULONGLONG maybeFeatures = (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                            ((ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
            const ULONGLONG unknownFeatures = maybeFeatures & ~(ULONGLONG)AEROGPU_KMD_LEGACY_PLAUSIBLE_FEATURES_MASK;
            if (unknownFeatures == 0) {
                features = maybeFeatures;
            } else {
                static LONG g_LegacyFeaturesImplausibleLogged = 0;
                if (InterlockedExchange(&g_LegacyFeaturesImplausibleLogged, 1) == 0) {
                    AEROGPU_LOG("StartDevice: legacy FEATURES has unknown bits 0x%I64x; ignoring (raw=0x%I64x)",
                                (unsigned long long)unknownFeatures,
                                (unsigned long long)maybeFeatures);
                }
                features = 0;
            }
        }
        if ((features & AEROGPU_FEATURE_VBLANK) != 0 &&
            adapter->Bar0Length < (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG))) {
            static LONG g_LegacyVblankRegsTooSmallLogged = 0;
            if (InterlockedExchange(&g_LegacyVblankRegsTooSmallLogged, 1) == 0) {
                AEROGPU_LOG("StartDevice: legacy BAR0 too small (%lu bytes) for vblank regs; disabling vblank feature",
                            adapter->Bar0Length);
            }
            features &= ~(ULONGLONG)AEROGPU_FEATURE_VBLANK;
        }
        if (magic != AEROGPU_LEGACY_MMIO_MAGIC) {
            AEROGPU_LOG("StartDevice: unknown MMIO magic=0x%08lx (expected 0x%08x); assuming legacy ABI",
                        magic,
                        AEROGPU_LEGACY_MMIO_MAGIC);
        }
        AEROGPU_LOG("StartDevice: ABI=legacy magic=0x%08lx version=0x%08lx features=0x%I64x",
                    magic,
                    abiVersion,
                    (unsigned long long)features);
    }

    adapter->DeviceAbiVersion = abiVersion;
    adapter->DeviceFeatures = features;
    adapter->SupportsVblank = (((features & AEROGPU_FEATURE_VBLANK) != 0) &&
                               (adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG))))
                                  ? TRUE
                                  : FALSE;
    adapter->VblankInterruptTypeValid = FALSE;
    adapter->VblankInterruptType = 0;

    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankSeq, 0);
    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankTimeNs, 0);
    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankInterruptTime100ns, 0);
    adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

    BOOLEAN interruptRegistered = FALSE;

    /*
     * Ensure a consistent initial IRQ state. dxgkrnl will enable/disable vsync
     * interrupts via DxgkDdiControlInterrupt.
     *
     * Some legacy device models also expose the versioned IRQ block. Reset it
     * to a known-disabled state so we don't inherit stale enable bits across
     * driver restarts.
     */
    if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
        adapter->IrqEnableMask = 0;
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
    }

    if (adapter->DxgkInterface.DxgkCbRegisterInterrupt) {
        NTSTATUS st = adapter->DxgkInterface.DxgkCbRegisterInterrupt(adapter->StartInfo.hDxgkHandle);
        if (!NT_SUCCESS(st)) {
            AEROGPU_LOG("StartDevice: DxgkCbRegisterInterrupt failed 0x%08lx", st);
        } else {
            interruptRegistered = TRUE;
        }
    }
    adapter->InterruptRegistered = interruptRegistered;

    NTSTATUS ringSt = STATUS_SUCCESS;
    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        ringSt = AeroGpuV1RingInit(adapter);
        if (NT_SUCCESS(ringSt)) {
            /*
             * Fence page is optional; if the device does not advertise
             * AEROGPU_FEATURE_FENCE_PAGE, fall back to polling COMPLETED_FENCE
             * via MMIO.
             */
            if (adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_FENCE_PAGE) {
                ringSt = AeroGpuV1FencePageInit(adapter);
            }
        }
        if (NT_SUCCESS(ringSt)) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                /*
                 * Only enable device IRQ generation when we have successfully
                 * registered an ISR with dxgkrnl. If RegisterInterrupt fails,
                 * leaving the device IRQ line asserted could trigger an
                 * unhandled interrupt storm.
                 */
                adapter->IrqEnableMask = interruptRegistered ? (AEROGPU_IRQ_FENCE | AEROGPU_IRQ_ERROR) : 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->IrqEnableMask);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
            }
        }
    } else {
        ringSt = AeroGpuLegacyRingInit(adapter);
        if (NT_SUCCESS(ringSt)) {
            /*
             * Some legacy device models expose the versioned IRQ block. Ensure
             * the mask starts from a known state so we don't inherit stale
             * enable bits across driver restarts.
             */
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                /*
                 * Fence IRQs for legacy devices are delivered via INT_STATUS/ACK,
                 * but ERROR/VBLANK use the versioned IRQ_STATUS/ENABLE/ACK block
                 * when present. Always enable ERROR delivery (when an ISR is
                 * registered) so the guest surfaces deterministic device-lost
                 * semantics instead of silently hanging.
                 */
                adapter->IrqEnableMask = interruptRegistered ? AEROGPU_IRQ_ERROR : 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->IrqEnableMask);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            }
        }
    }
    if (!NT_SUCCESS(ringSt)) {
        if (adapter->Bar0 && adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            /* Ensure the device won't touch freed ring memory on early-start failure. */
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);

            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                adapter->IrqEnableMask = 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
            }
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        } else if (adapter->Bar0) {
            /*
             * Legacy devices always expose INT_ACK for fences. Some legacy
             * device models also expose the versioned IRQ block; ack/disable
             * both so any level-triggered interrupt deasserts.
             */
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                adapter->IrqEnableMask = 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            }
        }

        /*
         * If `StartDevice` fails, dxgkrnl will not call StopDevice. Clean up
         * the registered interrupt handler explicitly to avoid leaving a stale
         * ISR callback installed.
         */
        if (interruptRegistered && adapter->DxgkInterface.DxgkCbDisableInterrupt) {
            adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
        }
        if (interruptRegistered && adapter->DxgkInterface.DxgkCbUnregisterInterrupt) {
            adapter->DxgkInterface.DxgkCbUnregisterInterrupt(adapter->StartInfo.hDxgkHandle);
        }
        adapter->InterruptRegistered = FALSE;

        AeroGpuRingCleanup(adapter);
        AeroGpuUnmapBar0(adapter);
        return ringSt;
    }

    if (interruptRegistered && adapter->DxgkInterface.DxgkCbEnableInterrupt) {
        adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    /*
     * Preserve any pre-existing scanout configuration (post-display ownership
     * handoff).
     *
     * On Win7, dxgkrnl can call DxgkDdiAcquirePostDisplayOwnership immediately
     * after StartDevice to map the existing framebuffer without doing a full
     * modeset. Do not clobber scanout state here; instead snapshot it and update
     * our cached mode/FbPa so AcquirePostDisplayOwnership can report consistent
     * values.
     *
     * Also proactively disable the hardware cursor so the device will not DMA
     * from a stale cursor GPA during transitions (cursor backing store is
     * driver-managed).
     */
    {
        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
        }

        AEROGPU_SCANOUT_MMIO_SNAPSHOT mmio;
        const BOOLEAN haveMmio = AeroGpuGetScanoutMmioSnapshot(adapter, &mmio);
        if (haveMmio && AeroGpuIsPlausibleScanoutSnapshot(&mmio)) {
            adapter->CurrentWidth = mmio.Width;
            adapter->CurrentHeight = mmio.Height;
            adapter->CurrentPitch = mmio.PitchBytes;
            adapter->CurrentFormat = mmio.Format;
            /*
             * Do not clobber the cached scanout FB GPA during a post-display ownership
             * transition if the device reports FbPa == 0.
             *
             * StopDevice/SetPowerState intentionally clear the MMIO FB address to
             * stop DMA, but we still need the cached value to restore scanout when
             * ownership is reacquired.
             */
            if (!adapter->PostDisplayOwnershipReleased || mmio.FbPa.QuadPart != 0) {
                adapter->CurrentScanoutFbPa = mmio.FbPa;
            }
            if (!adapter->PostDisplayOwnershipReleased) {
                adapter->SourceVisible = mmio.Enable ? TRUE : FALSE;
            }

            /* Never leave scanout enabled with an invalid framebuffer address. */
            if (mmio.Enable != 0 && mmio.FbPa.QuadPart == 0) {
                AeroGpuSetScanoutEnable(adapter, 0);
            }
        } else {
            /*
             * Scanout registers are not always initialized early in boot (or after a virtual
             * device reset). Avoid clobbering cached scanout state when we are in the middle of
             * a post-display ownership transition: AcquirePostDisplayOwnership may rely on the
             * cached FbPa/mode even if MMIO state is temporarily unavailable.
             */
            if (!adapter->PostDisplayOwnershipReleased) {
                PHYSICAL_ADDRESS zero;
                zero.QuadPart = 0;
                adapter->CurrentScanoutFbPa = zero;
            }

            /*
             * Be conservative: ensure scanout is disabled until dxgkrnl provides
             * a valid PrimaryAddress via SetVidPnSourceAddress.
             */
            AeroGpuSetScanoutEnable(adapter, 0);
        }
    }

    /*
     * Only allow submissions if BAR0 contains the required ring + doorbell registers.
     * Some bring-up/partial device models may expose enough MMIO for discovery/scanout
     * but not the DMA submission path.
     */
    BOOLEAN canSubmit = FALSE;
    {
        /*
         * AeroGpu*SubmitPathUsable reads ring header fields; take RingLock so we don't race
         * AeroGpuRingCleanup during teardown.
         */
        KIRQL ringIrql;
        KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            canSubmit = AeroGpuV1SubmitPathUsable(adapter);
        } else {
            canSubmit = AeroGpuLegacySubmitPathUsable(adapter);
        }
        KeReleaseSpinLock(&adapter->RingLock, ringIrql);
    }
    InterlockedExchange(&adapter->AcceptingSubmissions, canSubmit ? 1 : 0);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiStopDevice(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("StopDevice");
    InterlockedExchange(&adapter->AcceptingSubmissions, 0);
    const DXGK_DEVICE_POWER_STATE prevPowerState =
        (DXGK_DEVICE_POWER_STATE)InterlockedExchange(&adapter->DevicePowerState, (LONG)DxgkDevicePowerStateD3);
    /*
     * StopDevice can be called after the adapter has already been transitioned
     * to a non-D0 power state (e.g. after DxgkDdiSetPowerState(D3)).
     *
     * MMIO accesses while the device is powered down can hang; only touch MMIO
     * here if we believe the adapter was still in D0 at entry.
     */
    const BOOLEAN poweredOn = (prevPowerState == DxgkDevicePowerStateD0) ? TRUE : FALSE;

    if (adapter->Bar0 && poweredOn) {
        /*
         * Disable the hardware cursor early so the device will not DMA from freed
         * cursor memory during teardown.
         */
        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
        }

        /*
         * Stop scanout DMA during teardown.
         *
         * SetPowerState handles D0->Dx transitions, but StopDevice can be called as
         * part of a full PnP stop/start cycle and should not assume SetPowerState
         * has already quiesced scanout.
         */
        if (adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI, 0);
            }
        } else if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_HI, 0);
        }

        /* Stop device IRQ generation before unregistering the ISR. */
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);
            {
                KIRQL oldIrql;
                KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                adapter->IrqEnableMask = 0;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
                KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
            }
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        } else {
            /* Prevent the legacy device from touching freed ring memory. */
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
            /*
             * Legacy devices that expose the versioned IRQ_ENABLE block (mirroring
             * `aerogpu_pci.h`) may have vblank IRQs enabled. Disable + ack them before
             * unregistering the ISR to avoid leaving an INTx line asserted.
             */
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
                {
                    KIRQL oldIrql;
                    KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                    adapter->IrqEnableMask = 0;
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
                    KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                }
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            }
        }
    }

    if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbDisableInterrupt) {
        adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbUnregisterInterrupt) {
        adapter->DxgkInterface.DxgkCbUnregisterInterrupt(adapter->StartInfo.hDxgkHandle);
        adapter->InterruptRegistered = FALSE;
    }

    AeroGpuMetaHandleFreeAll(adapter);
    AeroGpuFreeAllPendingSubmissions(adapter);
    AeroGpuFreeAllInternalSubmissions(adapter);
    AeroGpuRingCleanup(adapter);
    {
        PVOID cursorVa = NULL;
        SIZE_T cursorSize = 0;
        KIRQL cursorIrql;
        KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
        cursorVa = adapter->CursorFbVa;
        cursorSize = adapter->CursorFbSizeBytes;
        adapter->CursorFbVa = NULL;
        adapter->CursorFbPa.QuadPart = 0;
        adapter->CursorFbSizeBytes = 0;
        adapter->CursorShapeValid = FALSE;
        adapter->CursorVisible = FALSE;
        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        AeroGpuFreeContiguousNonCached(adapter, cursorVa, cursorSize);
    }

    /* Release any pooled contiguous buffers retained by the submission hot path. */
    AeroGpuContigPoolPurge(adapter);

    if (adapter->Bar0) {
        AeroGpuUnmapBar0(adapter);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetPowerState(_In_ const HANDLE hAdapter,
                                                 _In_ DXGK_DEVICE_POWER_STATE DevicePowerState,
                                                 _In_ ULONG HwWakeupEnable)
{
    UNREFERENCED_PARAMETER(HwWakeupEnable);
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    const DXGK_DEVICE_POWER_STATE oldState =
        (DXGK_DEVICE_POWER_STATE)InterlockedExchange(&adapter->DevicePowerState, (LONG)DevicePowerState);

    if (DevicePowerState == DxgkDevicePowerStateD0) {
        /* Block submissions while restoring state. */
        InterlockedExchange(&adapter->AcceptingSubmissions, 0);

        if (!adapter->Bar0) {
            /* Early init / teardown: nothing to restore yet. */
            return STATUS_SUCCESS;
        }

        /*
         * Disable OS-level interrupt delivery while restoring device state so
         * we don't race ISR/DPC paths with partially-restored MMIO bookkeeping.
         *
         * StopDevice performs a full unregister; SetPowerState is a lighter
         * weight transition that keeps the ISR registered.
         */
        if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbDisableInterrupt) {
            adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
        }

        /*
         * Disable IRQs before resetting ring state to avoid racing ISR/DPC paths
         * with partially-restored bookkeeping.
         */
        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            KIRQL irqIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
            KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        }

        /*
         * If we are resuming from a non-D0 state, assume the virtual device may
         * have lost state. Do a best-effort "virtual reset":
         *   - treat all in-flight work as completed to avoid dxgkrnl stalls
         *   - reprogram ring/IRQ/fence-page MMIO state
         */
        if (oldState != DxgkDevicePowerStateD0) {
            LIST_ENTRY pendingToFree;
            InitializeListHead(&pendingToFree);
            LIST_ENTRY internalToFree;
            InitializeListHead(&internalToFree);

            ULONGLONG completedFence = 0;

            {
                KIRQL pendingIrql;
                KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);

                completedFence = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
                AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, completedFence);

                if (adapter->Bar0) {
                    KIRQL ringIrql;
                    KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

                    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                        /*
                         * Re-program the ring + optional fence page addresses in
                         * case the device reset them while powered down.
                         */
                        const BOOLEAN haveRingRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_RING_CONTROL + sizeof(ULONG));
                        const BOOLEAN haveFenceRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_FENCE_GPA_HI + sizeof(ULONG));

                        BOOLEAN haveRing = FALSE;
                        const ULONG ringEntryCount = adapter->RingEntryCount;
                        const BOOLEAN ringEntryCountPow2 =
                            (ringEntryCount != 0 && (ringEntryCount & (ringEntryCount - 1)) == 0) ? TRUE : FALSE;

                        if (adapter->RingVa && ringEntryCountPow2) {
                            const ULONGLONG minRingBytes =
                                (ULONGLONG)sizeof(struct aerogpu_ring_header) +
                                (ULONGLONG)ringEntryCount * (ULONGLONG)sizeof(struct aerogpu_submit_desc);
                            haveRing = (minRingBytes <= (ULONGLONG)adapter->RingSizeBytes) ? TRUE : FALSE;
                        }

                        if (haveRing && adapter->RingSizeBytes >= sizeof(struct aerogpu_ring_header)) {
                            /* Ring header lives at the start of the ring mapping. */
                            adapter->RingHeader = (struct aerogpu_ring_header*)adapter->RingVa;

                            /*
                             * Reinitialise the ring header static fields in case
                             * guest memory was clobbered while powered down.
                             */
                            adapter->RingHeader->magic = AEROGPU_RING_MAGIC;
                            adapter->RingHeader->abi_version = AEROGPU_ABI_VERSION_U32;
                            adapter->RingHeader->size_bytes = (uint32_t)adapter->RingSizeBytes;
                            adapter->RingHeader->entry_count = (uint32_t)adapter->RingEntryCount;
                            adapter->RingHeader->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
                            adapter->RingHeader->flags = 0;

                            const ULONG tail = adapter->RingTail;
                            adapter->RingHeader->head = tail;
                            adapter->RingHeader->tail = tail;
                            KeMemoryBarrier();
                        }

                        if (haveRingRegs) {
                            if (haveRing) {
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, adapter->RingPa.LowPart);
                                AeroGpuWriteRegU32(adapter,
                                                   AEROGPU_MMIO_REG_RING_GPA_HI,
                                                   (ULONG)(adapter->RingPa.QuadPart >> 32));
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, adapter->RingSizeBytes);
                            } else {
                                /* Ensure the device will not DMA from stale ring pointers. */
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, 0);
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, 0);
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, 0);
                            }

                            if (haveFenceRegs) {
                                if (adapter->FencePageVa &&
                                    (adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_FENCE_PAGE) != 0) {
                                    adapter->FencePageVa->magic = AEROGPU_FENCE_PAGE_MAGIC;
                                    adapter->FencePageVa->abi_version = AEROGPU_ABI_VERSION_U32;
                                    AeroGpuAtomicWriteU64((volatile ULONGLONG*)&adapter->FencePageVa->completed_fence, completedFence);
                                    KeMemoryBarrier();
                                    AeroGpuWriteRegU32(adapter,
                                                       AEROGPU_MMIO_REG_FENCE_GPA_LO,
                                                       adapter->FencePagePa.LowPart);
                                    AeroGpuWriteRegU32(adapter,
                                                       AEROGPU_MMIO_REG_FENCE_GPA_HI,
                                                       (ULONG)(adapter->FencePagePa.QuadPart >> 32));
                                } else {
                                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
                                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);
                                }
                            }

                            if (haveRing) {
                                AeroGpuWriteRegU32(adapter,
                                                   AEROGPU_MMIO_REG_RING_CONTROL,
                                                   AEROGPU_RING_CONTROL_ENABLE | AEROGPU_RING_CONTROL_RESET);
                            } else {
                                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, 0);
                            }
                        } else {
                            /*
                             * Defensive: BAR0 does not expose the v1 ring-control registers; we cannot
                             * safely reprogram/stop the ring here. Do not fall back to legacy ring
                             * registers (different ABI); leave submissions blocked instead.
                             */
                        }
                    } else {
                        BOOLEAN ringOk = FALSE;
                        if (adapter->RingVa && adapter->RingEntryCount != 0) {
                            const ULONGLONG minRingBytes =
                                (ULONGLONG)adapter->RingEntryCount * (ULONGLONG)sizeof(aerogpu_legacy_ring_entry);
                            ringOk = (minRingBytes <= (ULONGLONG)adapter->RingSizeBytes) ? TRUE : FALSE;
                        }

                        if (ringOk) {
                            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, adapter->RingPa.LowPart);
                            AeroGpuWriteRegU32(adapter,
                                               AEROGPU_LEGACY_REG_RING_BASE_HI,
                                               (ULONG)(adapter->RingPa.QuadPart >> 32));
                            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, adapter->RingEntryCount);
                        } else {
                            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, 0);
                            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, 0);
                            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, 0);
                        }

                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
                        adapter->RingTail = 0;
                        adapter->LegacyRingHeadIndex = 0;
                        adapter->LegacyRingHeadSeq = 0;
                        adapter->LegacyRingTailSeq = 0;
                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
                    }

                    KeReleaseSpinLock(&adapter->RingLock, ringIrql);
                }

                while (!IsListEmpty(&adapter->PendingSubmissions)) {
                    InsertTailList(&pendingToFree, RemoveHeadList(&adapter->PendingSubmissions));
                }
                while (!IsListEmpty(&adapter->RecentSubmissions)) {
                    InsertTailList(&pendingToFree, RemoveHeadList(&adapter->RecentSubmissions));
                }
                adapter->RecentSubmissionCount = 0;
                adapter->RecentSubmissionBytes = 0;
                while (!IsListEmpty(&adapter->PendingInternalSubmissions)) {
                    InsertTailList(&internalToFree, RemoveHeadList(&adapter->PendingInternalSubmissions));
                }

                KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
            }

            if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
                notify.DmaCompleted.SubmissionFenceId = (ULONG)completedFence;
                notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
            }

            if (adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
                adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
            }

            /*
             * Drop any per-submit metadata that was produced before the sleep transition but never
             * consumed by a subsequent SubmitCommand call (e.g. scheduler cancellation).
             */
            AeroGpuMetaHandleFreeAll(adapter);

            while (!IsListEmpty(&pendingToFree)) {
                PLIST_ENTRY entry = RemoveHeadList(&pendingToFree);
                AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
                AeroGpuFreeSubmission(adapter, sub);
            }
            while (!IsListEmpty(&internalToFree)) {
                PLIST_ENTRY entry = RemoveHeadList(&internalToFree);
                AEROGPU_PENDING_INTERNAL_SUBMISSION* sub =
                    CONTAINING_RECORD(entry, AEROGPU_PENDING_INTERNAL_SUBMISSION, ListEntry);
                AeroGpuFreeInternalSubmission(adapter, sub);
            }
        }

        /*
         * Some device models treat RING_CONTROL.RESET as a momentary edge, while others may latch
         * the bit until the driver clears it. Ensure we leave the v1 ring enabled after resume by
         * explicitly writing ENABLE once the virtual reset bookkeeping is complete.
         */
        if (oldState != DxgkDevicePowerStateD0 && adapter->AbiKind == AEROGPU_ABI_KIND_V1 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_RING_CONTROL + sizeof(ULONG))) {
            BOOLEAN ringOk = FALSE;
            {
                /*
                 * AeroGpuV1SubmitPathUsable reads ring header fields; take RingLock so we don't race
                 * AeroGpuRingCleanup during teardown.
                 */
                KIRQL ringIrql;
                KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
                ringOk = AeroGpuV1SubmitPathUsable(adapter);
                KeReleaseSpinLock(&adapter->RingLock, ringIrql);
            }
            if (ringOk) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, AEROGPU_RING_CONTROL_ENABLE);
            }
        }

        /* Reset vblank tracking so GetScanLine doesn't consume stale timestamps across resume. */
        InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankSeq, 0);
        InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankTimeNs, 0);
        InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankInterruptTime100ns, 0);
        adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

        /*
         * Re-apply scanout/cursor configuration after resume.
         *
         * If post-display ownership is currently released, keep scanout/cursor
         * disabled to avoid the device DMAing from guest memory while another
         * owner (VGA/basic/boot) is active.
         */
        if (!adapter->PostDisplayOwnershipReleased) {
            /* Re-apply scanout configuration (best-effort; modeset may arrive later). */
            AeroGpuProgramScanout(adapter, adapter->CurrentScanoutFbPa);

            /* Restore hardware cursor state (if supported). */
            if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
                adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
                BOOLEAN cursorShapeValid = FALSE;
                BOOLEAN cursorVisible = FALSE;
                LONG cursorX = 0;
                LONG cursorY = 0;
                ULONG cursorHotX = 0;
                ULONG cursorHotY = 0;
                ULONG cursorWidth = 0;
                ULONG cursorHeight = 0;
                ULONG cursorFormat = 0;
                ULONG cursorPitchBytes = 0;
                PVOID cursorVa = NULL;
                PHYSICAL_ADDRESS cursorPa;
                SIZE_T cursorSizeBytes = 0;
                cursorPa.QuadPart = 0;

                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    cursorShapeValid = adapter->CursorShapeValid;
                    cursorVisible = adapter->CursorVisible;
                    cursorX = adapter->CursorX;
                    cursorY = adapter->CursorY;
                    cursorHotX = adapter->CursorHotX;
                    cursorHotY = adapter->CursorHotY;
                    cursorWidth = adapter->CursorWidth;
                    cursorHeight = adapter->CursorHeight;
                    cursorFormat = adapter->CursorFormat;
                    cursorPitchBytes = adapter->CursorPitchBytes;
                    cursorVa = adapter->CursorFbVa;
                    cursorPa = adapter->CursorFbPa;
                    cursorSizeBytes = adapter->CursorFbSizeBytes;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }

                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
                if (cursorShapeValid && cursorVa && cursorSizeBytes != 0) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, (ULONG)cursorX);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, (ULONG)cursorY);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X, cursorHotX);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y, cursorHotY);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, cursorWidth);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, cursorHeight);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, cursorFormat);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, cursorPitchBytes);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, cursorPa.LowPart);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, (ULONG)(cursorPa.QuadPart >> 32));
                    KeMemoryBarrier();
                    AeroGpuWriteRegU32(adapter,
                                       AEROGPU_MMIO_REG_CURSOR_ENABLE,
                                       (cursorVisible && cursorShapeValid) ? 1u : 0u);
                } else {
                    /* Ensure the device does not DMA from a stale cursor GPA. */
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
                }
            }
        } else {
            AeroGpuSetScanoutEnable(adapter, 0);
            if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
                adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
            }
        }

        const BOOLEAN errorLatched = AeroGpuIsDeviceErrorLatched(adapter);

        /*
         * Re-enable interrupt delivery through dxgkrnl before unmasking device IRQ generation so
         * any immediately-pending (level-triggered) interrupt is routed to our ISR.
         */
        if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbEnableInterrupt) {
            adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
        }

        /* Restore IRQ enable mask (if supported). */
        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
            KIRQL irqIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
            ULONG enable = adapter->InterruptRegistered ? adapter->IrqEnableMask : 0;
            if (errorLatched) {
                /*
                 * If the device has asserted IRQ_ERROR, do not re-enable ERROR delivery across
                 * resume. Keeping vsync interrupts enabled (when requested by dxgkrnl) avoids
                 * hanging vblank wait paths.
                 */
                enable &= ~AEROGPU_IRQ_ERROR;
            } else if (adapter->InterruptRegistered) {
                /* Restore baseline delivery required for forward progress/diagnostics. */
                enable |= AEROGPU_IRQ_ERROR;
                if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                    enable |= AEROGPU_IRQ_FENCE;
                }
                adapter->IrqEnableMask = enable;
            }
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
            KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);

            /*
             * If we just resumed from a non-D0 state, clear any stale pending IRQ status bits that
             * may have latched while IRQ generation was masked.
             */
            if (!errorLatched && oldState != DxgkDevicePowerStateD0 &&
                adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
            }
        }

        BOOLEAN canSubmit = FALSE;
        {
            /*
             * AeroGpu*SubmitPathUsable reads ring header fields; take RingLock so we don't race
             * AeroGpuRingCleanup during teardown.
             */
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                canSubmit = AeroGpuV1SubmitPathUsable(adapter);
            } else {
                canSubmit = AeroGpuLegacySubmitPathUsable(adapter);
            }
            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }
        if (canSubmit) {
            InterlockedExchange(&adapter->AcceptingSubmissions, 1);
        }

        return STATUS_SUCCESS;
    }

    /* Transition away from D0: disable device IRQ generation and block submits. */
    InterlockedExchange(&adapter->AcceptingSubmissions, 0);

    if (!adapter->Bar0) {
        return STATUS_SUCCESS;
    }

    /* Disable OS-level interrupt delivery first to minimize ISR races during teardown. */
    if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbDisableInterrupt) {
        adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    /*
     * If we were already in a non-D0 state before this call, avoid touching MMIO.
     *
     * dxgkrnl can invoke SetPowerState repeatedly for the same power state during
     * PnP/hibernate transitions. MMIO accesses while powered down can hang, and
     * the device should already be quiesced from the initial D0->Dx transition.
     */
    if (oldState != DxgkDevicePowerStateD0) {
        return STATUS_SUCCESS;
    }

    /*
     * Stop scanout DMA while powered down.
     *
     * NOTE: AeroGpuSetScanoutEnable() is gated on DevicePowerState==D0, but this
     * callback updates DevicePowerState at entry. Disable scanout directly so we
     * still stop DMA on D0->DxgkDevicePowerStateD3 transitions.
     *
     * Scanout state will be restored in the D0 branch via AeroGpuProgramScanout
     * (using adapter->CurrentScanoutFbPa + cached mode state).
     */
    if (adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI, 0);
    } else {
        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_HI, 0);
    }
    if (adapter->SupportsVblank && adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
        /* Be robust against stale vblank IRQ state on scanout disable. */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
    }

    if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
        adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
        /*
         * Stop cursor DMA when leaving D0. The backing store lives in system memory
         * and may remain allocated across the power transition; reprogram state on
         * resume.
         */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
    }

    if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
    }

    /*
     * Stop ring execution while powered down.
     *
     * Take PendingLock -> RingLock to serialize against SubmitCommand paths
     * that hold PendingLock while pushing to the ring.
     */
    {
        KIRQL pendingIrql;
        KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);

        KIRQL ringIrql;
        KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);
        } else {
            /* Legacy ABI has no ring control bit; clear the ring programming instead. */
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
            adapter->RingTail = 0;
            adapter->LegacyRingHeadIndex = 0;
            adapter->LegacyRingHeadSeq = 0;
            adapter->LegacyRingTailSeq = 0;

            /* Legacy fence interrupts are acknowledged via INT_ACK. */
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
        }

        KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiStopDeviceAndReleasePostDisplayOwnership(
    _In_ const PVOID MiniportDeviceContext,
    _Inout_ DXGKARG_STOPDEVICEANDRELEASEPOSTDISPLAYOWNERSHIP* pStopDeviceAndReleasePostDisplayOwnership)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("StopDeviceAndReleasePostDisplayOwnership");

    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);

    /*
     * Report the current scanout mode + framebuffer so dxgkrnl can transition
     * cleanly to the next owner (boot/basic/VGA).
     *
     * Best-effort: if the device isn't mapped (early init/teardown), report no
     * framebuffer.
     */
    if (pStopDeviceAndReleasePostDisplayOwnership) {
        ULONG outWidth = adapter->CurrentWidth;
        ULONG outHeight = adapter->CurrentHeight;
        ULONG outPitch = adapter->CurrentPitch;
        ULONG outFormat = adapter->CurrentFormat;
        PHYSICAL_ADDRESS outFbPa = adapter->CurrentScanoutFbPa;

        if (poweredOn) {
            AEROGPU_SCANOUT_MMIO_SNAPSHOT mmio;
            if (AeroGpuGetScanoutMmioSnapshot(adapter, &mmio) && AeroGpuIsPlausibleScanoutSnapshot(&mmio)) {
                outWidth = mmio.Width;
                outHeight = mmio.Height;
                outPitch = mmio.PitchBytes;
                outFormat = mmio.Format;
                outFbPa = mmio.FbPa;

                adapter->CurrentWidth = mmio.Width;
                adapter->CurrentHeight = mmio.Height;
                adapter->CurrentPitch = mmio.PitchBytes;
                adapter->CurrentFormat = mmio.Format;
                /*
                 * If we are already in a released post-display-ownership state, avoid clobbering the
                 * cached scanout FB address with a zero value: StopDevice/SetPowerState clear the
                 * MMIO FB GPA registers to stop DMA, but we may still need the cached value to
                 * restore scanout when ownership is reacquired.
                 */
                if (!adapter->PostDisplayOwnershipReleased || mmio.FbPa.QuadPart != 0) {
                    adapter->CurrentScanoutFbPa = mmio.FbPa;
                }
            }
        } else if (!adapter->Bar0) {
            PHYSICAL_ADDRESS zero;
            zero.QuadPart = 0;
            adapter->CurrentScanoutFbPa = zero;
            outFbPa = zero;
        }

        DXGK_DISPLAY_INFORMATION* displayInfo = pStopDeviceAndReleasePostDisplayOwnership->pDisplayInfo;
        if (displayInfo) {
            RtlZeroMemory(displayInfo, sizeof(*displayInfo));
            displayInfo->Width = outWidth;
            displayInfo->Height = outHeight;
            displayInfo->Pitch = outPitch;
            displayInfo->ColorFormat = AeroGpuDdiColorFormatFromScanoutFormat(outFormat);
            displayInfo->PhysicalAddress = outFbPa;
            displayInfo->TargetId = AEROGPU_VIDPN_TARGET_ID;
        }

        DXGK_FRAMEBUFFER_INFORMATION* fbInfo = pStopDeviceAndReleasePostDisplayOwnership->pFrameBufferInfo;
        if (fbInfo) {
            RtlZeroMemory(fbInfo, sizeof(*fbInfo));
            if (outFbPa.QuadPart != 0) {
                fbInfo->FrameBufferBase = outFbPa;

                ULONGLONG len = 0;
                if (outPitch != 0 && outHeight != 0) {
                    len = (ULONGLONG)outPitch * (ULONGLONG)outHeight;
                }
                if (len > 0xFFFFFFFFull) {
                    len = 0xFFFFFFFFull;
                }
                fbInfo->FrameBufferLength = (ULONG)len;

                fbInfo->FrameBufferSegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
            }
        }
    }

    /*
     * dxgkrnl can request post-display ownership release during shutdown /
     * display transitions. Keep this path minimal and robust:
     *   - disable scanout so the device stops reading guest memory
     *   - disable vblank IRQ delivery
     *
     * Then, run the regular StopDevice teardown so BAR mappings, ring memory,
     * and interrupt handlers are released consistently.
     */
    if (adapter->Bar0) {
        const BOOLEAN poweredOn =
            ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) ==
             DxgkDevicePowerStateD0)
                ? TRUE
                : FALSE;

        /* Snapshot vblank enable state once per release cycle. */
        if (!adapter->PostDisplayOwnershipReleased) {
            adapter->PostDisplayVblankWasEnabled =
                (adapter->IrqEnableMask & AEROGPU_IRQ_SCANOUT_VBLANK) != 0 ? TRUE : FALSE;
        }
        adapter->PostDisplayOwnershipReleased = TRUE;

        /* Disable vblank IRQ generation. */
        if (poweredOn && adapter->SupportsVblank && adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);

            ULONG enable = adapter->IrqEnableMask;
            enable &= ~AEROGPU_IRQ_SCANOUT_VBLANK;
            if (AeroGpuIsDeviceErrorLatched(adapter)) {
                enable &= ~AEROGPU_IRQ_ERROR;
            }
            adapter->IrqEnableMask = enable;

            if (poweredOn) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                    enable &= ~AEROGPU_IRQ_ERROR;
                    adapter->IrqEnableMask = enable;
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                }

                /* Be robust against stale pending bits when disabling. */
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
            }

            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }

        /*
         * Disable the hardware cursor as part of the release path so the device
         * stops DMAing from system memory immediately (before the full StopDevice
         * teardown runs).
         */
        if (poweredOn && (adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
        }

        /* Disable scanout to stop the device from continuously touching guest memory. */
        if (poweredOn) {
            AeroGpuSetScanoutEnable(adapter, 0);
        }
    } else {
        adapter->PostDisplayOwnershipReleased = TRUE;
        adapter->PostDisplayVblankWasEnabled = FALSE;
    }

    return AeroGpuDdiStopDevice(MiniportDeviceContext);
}

static NTSTATUS APIENTRY AeroGpuDdiAcquirePostDisplayOwnership(
    _In_ const HANDLE hAdapter,
    _Inout_ DXGKARG_ACQUIREPOSTDISPLAYOWNERSHIP* pAcquirePostDisplayOwnership)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pAcquirePostDisplayOwnership) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("AcquirePostDisplayOwnership");

    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);

    /*
     * Best-effort snapshot of the currently-programmed scanout configuration.
     *
     * This is used by dxgkrnl to map the existing framebuffer during boot and
     * display-driver transitions (VGA/basic <-> WDDM). Keep it robust: if the
     * device is not mapped yet, or if the scanout registers are not plausible,
     * fall back to the cached mode and report no framebuffer address.
     */
    if (poweredOn) {
        /* Stop cursor DMA until the OS programs a new pointer shape. */
        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
        }

        AEROGPU_SCANOUT_MMIO_SNAPSHOT mmio;
        if (AeroGpuGetScanoutMmioSnapshot(adapter, &mmio) && AeroGpuIsPlausibleScanoutSnapshot(&mmio)) {
            adapter->CurrentWidth = mmio.Width;
            adapter->CurrentHeight = mmio.Height;
            adapter->CurrentPitch = mmio.PitchBytes;
            adapter->CurrentFormat = mmio.Format;
            /*
             * During reacquire after a post-display ownership release, StopDevice may have
             * cleared the scanout FB GPA in MMIO even though the next owner is still using
             * the same framebuffer. Preserve the cached FB GPA in that case so we can
             * restore scanout without requiring an immediate SetVidPnSourceAddress.
             */
            if (!adapter->PostDisplayOwnershipReleased || mmio.FbPa.QuadPart != 0) {
                adapter->CurrentScanoutFbPa = mmio.FbPa;
            }

            /*
             * Treat the hardware enable bit as authoritative during acquisition:
             * dxgkrnl has not yet called SetVidPnSourceVisibility in some paths.
             */
            if (!adapter->PostDisplayOwnershipReleased) {
                adapter->SourceVisible = mmio.Enable ? TRUE : FALSE;
            }

            /* Ensure we never enable scanout with FbPa == 0. */
            if (mmio.Enable != 0 && mmio.FbPa.QuadPart == 0) {
                AeroGpuSetScanoutEnable(adapter, 0);
            }
        } else {
            /*
             * Unknown scanout state.
             *
             * If we are reacquiring after a post-display ownership release, keep the cached
             * scanout FbPa/mode so we can restore scanout even if the MMIO state was reset.
             * Otherwise, report no framebuffer address.
             */
            if (!adapter->PostDisplayOwnershipReleased) {
                PHYSICAL_ADDRESS zero;
                zero.QuadPart = 0;
                adapter->CurrentScanoutFbPa = zero;
            }
        }
    } else if (!adapter->Bar0) {
        /* Device isn't mapped yet (early init / teardown). */
        PHYSICAL_ADDRESS zero;
        zero.QuadPart = 0;
        adapter->CurrentScanoutFbPa = zero;
    }

    /*
     * Report the current mode + framebuffer info back to dxgkrnl.
     *
     * The argument struct provides caller-allocated output structs.
     */
    {
        DXGK_DISPLAY_INFORMATION* displayInfo = pAcquirePostDisplayOwnership->pDisplayInfo;
        if (displayInfo) {
            RtlZeroMemory(displayInfo, sizeof(*displayInfo));
            displayInfo->Width = adapter->CurrentWidth;
            displayInfo->Height = adapter->CurrentHeight;
            displayInfo->Pitch = adapter->CurrentPitch;
            displayInfo->ColorFormat = AeroGpuDdiColorFormatFromScanoutFormat(adapter->CurrentFormat);
            displayInfo->PhysicalAddress = adapter->CurrentScanoutFbPa;
            displayInfo->TargetId = AEROGPU_VIDPN_TARGET_ID;
        }

        DXGK_FRAMEBUFFER_INFORMATION* fbInfo = pAcquirePostDisplayOwnership->pFrameBufferInfo;
        if (fbInfo) {
            RtlZeroMemory(fbInfo, sizeof(*fbInfo));
            if (adapter->CurrentScanoutFbPa.QuadPart != 0) {
                fbInfo->FrameBufferBase = adapter->CurrentScanoutFbPa;

                ULONGLONG len = 0;
                if (adapter->CurrentPitch != 0 && adapter->CurrentHeight != 0) {
                    len = (ULONGLONG)adapter->CurrentPitch * (ULONGLONG)adapter->CurrentHeight;
                }
                if (len > 0xFFFFFFFFull) {
                    len = 0xFFFFFFFFull;
                }
                fbInfo->FrameBufferLength = (ULONG)len;

                fbInfo->FrameBufferSegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
            }
        }
    }

    /*
     * Reacquire is expected to make the miniport responsible for programming
     * scanout again. This is best-effort: if the device isn't mapped yet (early
     * init) or is being torn down, just succeed.
     */
    if (!adapter->Bar0) {
        adapter->PostDisplayOwnershipReleased = FALSE;
        return STATUS_SUCCESS;
    }

    const BOOLEAN poweredOnNow =
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) ==
         DxgkDevicePowerStateD0)
            ? TRUE
            : FALSE;
    const BOOLEAN wasReleased = adapter->PostDisplayOwnershipReleased ? TRUE : FALSE;
    if (wasReleased) {
        /*
         * We are now reacquiring ownership; clear the release flag before
         * programming scanout so AeroGpuProgramScanout/AeroGpuSetScanoutEnable
         * can re-enable scanout.
         */
        adapter->PostDisplayOwnershipReleased = FALSE;
    }

    if (!poweredOnNow) {
        /*
         * Avoid touching MMIO while powered down.
         *
         * Still record that ownership has been reacquired so the next D0 resume
         * can restore scanout/cursor via DxgkDdiSetPowerState.
         */
        if (wasReleased && adapter->PostDisplayVblankWasEnabled && adapter->SupportsVblank &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            /*
             * Best-effort: restore the cached vblank enable mask without touching
             * MMIO so SetPowerState(D0) can reapply it.
             */
            if (!adapter->VblankInterruptTypeValid) {
                adapter->VblankInterruptType = DXGK_INTERRUPT_TYPE_CRTC_VSYNC;
                KeMemoryBarrier();
                adapter->VblankInterruptTypeValid = TRUE;
            }

            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
            adapter->IrqEnableMask |= AEROGPU_IRQ_SCANOUT_VBLANK;
            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }
        return STATUS_SUCCESS;
    }

    /* Re-program scanout registers using the last cached mode + FB address. */
    AeroGpuProgramScanout(adapter, adapter->CurrentScanoutFbPa);

    if (wasReleased) {
        /*
         * Restore vblank IRQ generation if it was enabled before the release.
         *
         * dxgkrnl typically re-enables via DxgkDdiControlInterrupt, but some
         * transition paths assume the miniport restores its prior state.
         */
        if (poweredOnNow && adapter->PostDisplayVblankWasEnabled && adapter->SupportsVblank &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            /*
             * Dxgkrnl normally tells us which interrupt type to use via
             * DxgkDdiControlInterrupt. If it skips that call during a
             * post-display-ownership transition, we still need a valid type so
             * the ISR can notify vblank delivery (Win7/WDDM 1.1 expects
             * DXGK_INTERRUPT_TYPE_CRTC_VSYNC).
             */
            if (!adapter->VblankInterruptTypeValid) {
                adapter->VblankInterruptType = DXGK_INTERRUPT_TYPE_CRTC_VSYNC;
                KeMemoryBarrier();
                adapter->VblankInterruptTypeValid = TRUE;
            }

            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);

            ULONG enable = adapter->IrqEnableMask;

            /* Clear any stale vblank status before enabling delivery. */
            if ((enable & AEROGPU_IRQ_SCANOUT_VBLANK) == 0) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
            }

            enable |= AEROGPU_IRQ_SCANOUT_VBLANK;
            if (AeroGpuIsDeviceErrorLatched(adapter)) {
                enable &= ~AEROGPU_IRQ_ERROR;
            }
            adapter->IrqEnableMask = enable;
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
            if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                enable &= ~AEROGPU_IRQ_ERROR;
                adapter->IrqEnableMask = enable;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
            }

            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRemoveDevice(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_LOG0("RemoveDevice");
    /* Free cached scratch buffers (BuildAllocTable). */
    {
        for (UINT shard = 0; shard < AEROGPU_ALLOC_TABLE_SCRATCH_SHARD_COUNT; ++shard) {
            AEROGPU_ALLOC_TABLE_SCRATCH* scratch = &adapter->AllocTableScratch[shard];
            PVOID block = NULL;
            SIZE_T blockBytes = 0;
            UINT tmpCap = 0;
            UINT hashCap = 0;
#if DBG
            LONG hitCount = 0;
            LONG growCount = 0;
#endif
            ExAcquireFastMutex(&scratch->Mutex);
            block = scratch->Block;
            blockBytes = scratch->BlockBytes;
            tmpCap = scratch->TmpEntriesCapacity;
            hashCap = scratch->HashCapacity;
#if DBG
            hitCount = scratch->HitCount;
            growCount = scratch->GrowCount;
#endif
            scratch->Block = NULL;
            scratch->BlockBytes = 0;
            scratch->TmpEntriesCapacity = 0;
            scratch->HashCapacity = 0;
            scratch->TmpEntries = NULL;
            scratch->SeenSlots = NULL;
            scratch->Epoch = 0;
            ExReleaseFastMutex(&scratch->Mutex);
#if DBG
            if (hitCount != 0 || growCount != 0 || blockBytes != 0) {
                AEROGPU_LOG("BuildAllocTable scratch[%u] stats: hits=%ld grows=%ld tmp_cap=%u hash_cap=%u bytes=%Iu",
                            shard,
                            hitCount,
                            growCount,
                            tmpCap,
                            hashCap,
                            blockBytes);
            }
#endif
            if (block) {
                ExFreePoolWithTag(block, AEROGPU_POOL_TAG);
            }
        }
    }
    {
        PVOID cursorVa = NULL;
        SIZE_T cursorSize = 0;
        KIRQL cursorIrql;
        KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
        cursorVa = adapter->CursorFbVa;
        cursorSize = adapter->CursorFbSizeBytes;
        adapter->CursorFbVa = NULL;
        adapter->CursorFbPa.QuadPart = 0;
        adapter->CursorFbSizeBytes = 0;
        adapter->CursorShapeValid = FALSE;
        adapter->CursorVisible = FALSE;
        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        AeroGpuFreeContiguousNonCached(adapter, cursorVa, cursorSize);
    }
    AeroGpuMetaHandleFreeAll(adapter);
    AeroGpuFreeAllPendingSubmissions(adapter);
    AeroGpuFreeAllAllocations(adapter);
    AeroGpuFreeAllShareTokenRefs(adapter);
    ExDeleteNPagedLookasideList(&adapter->ShareTokenRefLookaside);
    AeroGpuFreeAllInternalSubmissions(adapter);
    AeroGpuFreeSharedHandleTokens(adapter);
    AeroGpuContigPoolPurge(adapter);
    ExDeleteNPagedLookasideList(&adapter->PendingInternalSubmissionLookaside);
    ExFreePoolWithTag(adapter, AEROGPU_POOL_TAG);
    return STATUS_SUCCESS;
}

static VOID APIENTRY AeroGpuDdiUnload(VOID)
{
    AEROGPU_LOG0("Unload");
}

static __forceinline ULONG AeroGpuClampUlong(_In_ ULONG Value, _In_ ULONG Min, _In_ ULONG Max)
{
    if (Value < Min) {
        return Min;
    }
    if (Value > Max) {
        return Max;
    }
    return Value;
}

static BOOLEAN AeroGpuTryQueryRegistryDword(_In_ HANDLE Key, _In_ PCWSTR ValueNameW, _Out_ ULONG* ValueOut)
{
    if (!Key || !ValueNameW || !ValueOut) {
        return FALSE;
    }

    UNICODE_STRING valueName;
    RtlInitUnicodeString(&valueName, ValueNameW);

    UCHAR buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
    PKEY_VALUE_PARTIAL_INFORMATION info = (PKEY_VALUE_PARTIAL_INFORMATION)buf;
    RtlZeroMemory(buf, sizeof(buf));

    ULONG resultLen = 0;
    if (NT_SUCCESS(ZwQueryValueKey(Key, &valueName, KeyValuePartialInformation, info, sizeof(buf), &resultLen)) &&
        info->Type == REG_DWORD && info->DataLength >= sizeof(ULONG)) {
        *ValueOut = *(UNALIGNED const ULONG*)info->Data;
        return TRUE;
    }

    return FALSE;
}

static BOOLEAN AeroGpuTryReadRegistryDword(_In_ PDEVICE_OBJECT PhysicalDeviceObject,
                                           _In_ ULONG RootKeyType,
                                           _In_opt_ PCWSTR SubKeyNameW,
                                           _In_ PCWSTR ValueNameW,
                                           _Out_ ULONG* ValueOut)
{
    if (!PhysicalDeviceObject || !ValueNameW || !ValueOut) {
        return FALSE;
    }

    HANDLE rootKey = NULL;
    if (!NT_SUCCESS(IoOpenDeviceRegistryKey(PhysicalDeviceObject, RootKeyType, KEY_READ, &rootKey)) || rootKey == NULL) {
        return FALSE;
    }

    BOOLEAN ok = FALSE;

    if (SubKeyNameW && SubKeyNameW[0] != L'\0') {
        HANDLE subKey = NULL;
        UNICODE_STRING subKeyName;
        OBJECT_ATTRIBUTES oa;
        RtlInitUnicodeString(&subKeyName, SubKeyNameW);
        InitializeObjectAttributes(&oa, &subKeyName, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, rootKey, NULL);

        if (NT_SUCCESS(ZwOpenKey(&subKey, KEY_READ, &oa)) && subKey != NULL) {
            ok = AeroGpuTryQueryRegistryDword(subKey, ValueNameW, ValueOut);
            ZwClose(subKey);
        }
    } else {
        ok = AeroGpuTryQueryRegistryDword(rootKey, ValueNameW, ValueOut);
    }

    ZwClose(rootKey);
    return ok;
}

/* ---- dbgctl READ_GPA support --------------------------------------------- */

/*
 * READ_GPA is intentionally constrained:
 * - PASSIVE_LEVEL only (registry + mapping APIs)
 * - strict size caps
 * - physical range validation (RAM only)
 * - security gated (service key opt-in + privileged caller)
 *
 * This is a debugging escape; treat it as a sharp tool.
 */
#define AEROGPU_DBGCTL_READ_GPA_HARD_MAX_BYTES (64u * 1024u)

static BOOLEAN AeroGpuDbgctlReadGpaRegistryEnabled(_In_opt_ const AEROGPU_ADAPTER* Adapter)
{
    UNREFERENCED_PARAMETER(Adapter);
    return (g_AeroGpuEnableReadGpaEscape != 0) ? TRUE : FALSE;
}

static BOOLEAN AeroGpuDbgctlCallerIsAdminOrSeDebug(_In_ KPROCESSOR_MODE PreviousMode)
{
    /* Prefer an explicit group check for usability (SeDebugPrivilege is often disabled by default). */
    BOOLEAN isAdmin = FALSE;
    PACCESS_TOKEN token = PsReferencePrimaryToken(PsGetCurrentProcess());
    if (token) {
        isAdmin = SeTokenIsAdmin(token) ? TRUE : FALSE;
        PsDereferencePrimaryToken(token);
    }

    LUID debugLuid;
    debugLuid.LowPart = SE_DEBUG_PRIVILEGE;
    debugLuid.HighPart = 0;
    const BOOLEAN hasDebugPriv = SeSinglePrivilegeCheck(debugLuid, PreviousMode) ? TRUE : FALSE;

    return (isAdmin || hasDebugPriv) ? TRUE : FALSE;
}

static BOOLEAN AeroGpuDbgctlValidateGpaRangeIsRam(_In_ ULONGLONG Gpa, _In_ ULONG SizeBytes)
{
    if (SizeBytes == 0) {
        return TRUE;
    }

    const ULONGLONG end = Gpa + (ULONGLONG)SizeBytes;
    if (end < Gpa) {
        return FALSE;
    }

    PPHYSICAL_MEMORY_RANGE ranges = MmGetPhysicalMemoryRanges();
    if (!ranges) {
        return FALSE;
    }

    BOOLEAN ok = FALSE;
    for (PPHYSICAL_MEMORY_RANGE r = ranges; r->NumberOfBytes.QuadPart != 0; ++r) {
        const ULONGLONG base = (ULONGLONG)r->BaseAddress.QuadPart;
        const ULONGLONG len = (ULONGLONG)r->NumberOfBytes.QuadPart;
        const ULONGLONG limit = base + len;
        if (limit < base) {
            continue;
        }
        if (Gpa >= base && end <= limit) {
            ok = TRUE;
            break;
        }
    }

    ExFreePool(ranges);
    return ok;
}

static NTSTATUS AeroGpuDbgctlReadGpaBytes(_In_ ULONGLONG Gpa, _In_ ULONG SizeBytes, _Out_writes_bytes_(SizeBytes) UCHAR* Dst)
{
    if (!Dst) {
        return STATUS_INVALID_PARAMETER;
    }
    if (SizeBytes == 0) {
        return STATUS_SUCCESS;
    }

    const ULONGLONG pageMask = (ULONGLONG)PAGE_SIZE - 1ull;
    const ULONGLONG base = Gpa & ~pageMask;
    const ULONG offset = (ULONG)(Gpa - base);

    const ULONGLONG mapSize64 = (ULONGLONG)offset + (ULONGLONG)SizeBytes;
    if (mapSize64 < (ULONGLONG)offset) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONGLONG aligned64 = (mapSize64 + pageMask) & ~pageMask;
    if (aligned64 > 0xFFFFFFFFull) {
        return STATUS_INVALID_PARAMETER;
    }

    const SIZE_T mapSize = (SIZE_T)aligned64;
    PHYSICAL_ADDRESS pa;
    pa.QuadPart = (LONGLONG)base;

    PVOID map = MmMapIoSpace(pa, mapSize, MmCached);
    if (!map) {
        return STATUS_UNSUCCESSFUL;
    }

    NTSTATUS st = STATUS_SUCCESS;
    __try {
        RtlCopyMemory(Dst, (PUCHAR)map + offset, SizeBytes);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        st = STATUS_UNSUCCESSFUL;
    }

    MmUnmapIoSpace(map, mapSize);
    return st;
}

static ULONGLONG AeroGpuGetNonLocalMemorySizeBytes(_In_ const AEROGPU_ADAPTER* Adapter)
{
    ULONG sizeMb = AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_DEFAULT;

    /*
     * This value controls the WDDM segment budget reported via QueryAdapterInfo.
     * Read it once during bring-up (PASSIVE_LEVEL) and cache the final byte size
     * so later queries are consistent and do not touch the registry.
     *
     * The registry APIs require PASSIVE_LEVEL.
     */
    if (Adapter && Adapter->PhysicalDeviceObject && KeGetCurrentIrql() == PASSIVE_LEVEL) {
        ULONG regMb = 0;
        if (AeroGpuTryReadRegistryDword(Adapter->PhysicalDeviceObject,
                                        PLUGPLAY_REGKEY_DRIVER,
                                        L"Parameters",
                                        L"NonLocalMemorySizeMB",
                                        &regMb) ||
            AeroGpuTryReadRegistryDword(Adapter->PhysicalDeviceObject,
                                        PLUGPLAY_REGKEY_DEVICE,
                                        L"Parameters",
                                        L"NonLocalMemorySizeMB",
                                        &regMb)) {
            sizeMb = regMb;
        }
    }

    sizeMb = AeroGpuClampUlong(sizeMb, AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_MIN, AEROGPU_NON_LOCAL_MEMORY_SIZE_MB_MAX);
    return (ULONGLONG)sizeMb * 1024ull * 1024ull;
}

static NTSTATUS APIENTRY AeroGpuDdiQueryAdapterInfo(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGKARG_QUERYADAPTERINFO* pQueryAdapterInfo)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pQueryAdapterInfo || !pQueryAdapterInfo->pOutputData) {
        return STATUS_INVALID_PARAMETER;
    }

    switch (pQueryAdapterInfo->Type) {
    case DXGKQAITYPE_DRIVERCAPS: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_DRIVERCAPS)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        DXGK_DRIVERCAPS* caps = (DXGK_DRIVERCAPS*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(caps, sizeof(*caps));
        caps->WDDMVersion = DXGKDDI_WDDMv1_1;
        caps->HighestAcceptableAddress.QuadPart = ~0ULL;
        caps->MaxAllocationListSlotId = 0xFFFF;
        caps->MaxPatchLocationListSlotId = 0xFFFF;
        caps->DmaBufferPrivateDataSize = (ULONG)AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
        caps->SchedulingCaps.Value = 0;
        caps->SchedulingCaps.MultipleEngineAware = 0;
        caps->PreemptionCaps.GraphicsPreemptionGranularity = D3DKMDT_GRAPHICS_PREEMPTION_DMA_BUFFER_BOUNDARY;
        caps->PreemptionCaps.ComputePreemptionGranularity = D3DKMDT_COMPUTE_PREEMPTION_DMA_BUFFER_BOUNDARY;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_QUERYSEGMENT: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_QUERYSEGMENTOUT)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const ULONGLONG nonLocalBytes = adapter->NonLocalMemorySizeBytes;

        DXGK_QUERYSEGMENTOUT* out = (DXGK_QUERYSEGMENTOUT*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(out, sizeof(*out));

        out->NbSegments = 1;
        out->pSegmentDescriptor[0].BaseAddress.QuadPart = 0;
        out->pSegmentDescriptor[0].Size = nonLocalBytes;
        out->pSegmentDescriptor[0].Flags.Value = 0;
        out->pSegmentDescriptor[0].Flags.Aperture = 1;
        out->pSegmentDescriptor[0].Flags.CpuVisible = 1;
        out->pSegmentDescriptor[0].Flags.CacheCoherent = 1;
        out->pSegmentDescriptor[0].MemorySegmentGroup = DXGK_MEMORY_SEGMENT_GROUP_NON_LOCAL;

        out->PagingBufferPrivateDataSize = (ULONG)AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
        out->PagingBufferSegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        out->PagingBufferSize = 0;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_GETSEGMENTGROUPSIZE: {
        if (pQueryAdapterInfo->OutputDataSize < sizeof(DXGK_SEGMENTGROUPSIZE)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        DXGK_SEGMENTGROUPSIZE* sizes = (DXGK_SEGMENTGROUPSIZE*)pQueryAdapterInfo->pOutputData;
        RtlZeroMemory(sizes, sizeof(*sizes));
        sizes->LocalMemorySize = 0;
        sizes->NonLocalMemorySize = adapter->NonLocalMemorySizeBytes;
        return STATUS_SUCCESS;
    }

    case DXGKQAITYPE_UMDRIVERPRIVATE: {
        /*
         * User-mode discovery blob used by AeroGPU UMDs (D3D9Ex/D3D10+) to
         * identify the active device ABI (legacy "ARGP" vs new "AGPU"), ABI
         * version, and feature bits.
         *
         * Backwards compatibility:
         *   - Older guest tooling expected a single ULONG return value.
         *   - Preserve that when OutputDataSize == sizeof(ULONG).
         */
        if (pQueryAdapterInfo->OutputDataSize < sizeof(ULONG)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const BOOLEAN poweredOn =
            (adapter->Bar0 != NULL) &&
            ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);
        const BOOLEAN acceptingSubmissions =
            (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
        /*
         * Be defensive during resume/teardown windows: dxgkrnl can report the adapter
         * as D0 before we've fully restored ring/MMIO programming state. Gate MMIO
         * reads on the same "ready" signal used by submission paths.
         */
        const BOOLEAN mmioSafe = poweredOn && acceptingSubmissions;

        /*
         * v0 legacy query: return only the device ABI version.
         * - Legacy device: MMIO VERSION register (BAR0[0x0004]).
         * - New device: ABI_VERSION register (same offset).
         */
        if (pQueryAdapterInfo->OutputDataSize == sizeof(ULONG)) {
            /*
             * Avoid touching MMIO while powered down; return the last-known ABI
             * version discovered during StartDevice.
             */
            const ULONG abiVersion =
                mmioSafe ? AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION) : adapter->DeviceAbiVersion;
            *(ULONG*)pQueryAdapterInfo->pOutputData = (ULONG)abiVersion;
            return STATUS_SUCCESS;
        }

        if (pQueryAdapterInfo->OutputDataSize < sizeof(aerogpu_umd_private_v1)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_umd_private_v1* out = (aerogpu_umd_private_v1*)pQueryAdapterInfo->pOutputData;
        /*
         * The UMDRIVERPRIVATE blob is intentionally forward-compatible:
         * consumers may pass a larger buffer and ignore trailing bytes.
         *
         * Always clear the entire output buffer so we don't leak uninitialized
         * kernel memory if OutputDataSize > sizeof(aerogpu_umd_private_v1).
         */
        RtlZeroMemory(out, pQueryAdapterInfo->OutputDataSize);

        out->size_bytes = sizeof(*out);
        out->struct_version = AEROGPU_UMDPRIV_STRUCT_VERSION_V1;

        ULONG magic = 0;
        ULONG abiVersion = 0;
        ULONGLONG features = 0;
        ULONGLONG fencePageGpa = 0;

        if (mmioSafe) {
            magic = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_MAGIC);
            abiVersion = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_ABI_VERSION);
            if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_UMDPRIV_MMIO_REG_FEATURES_HI);
                features = ((ULONGLONG)hi << 32) | (ULONGLONG)lo;

                /*
                 * The UMD-private blob exposes a convenience flag indicating
                 * whether a shared fence page is configured/usable. Distinguish
                 * this from the raw feature bit (which only indicates support).
                 */
                if (features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) {
                    const ULONG fenceLo = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO);
                    const ULONG fenceHi = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI);
                    fencePageGpa = ((ULONGLONG)fenceHi << 32) | (ULONGLONG)fenceLo;
                }
            }
        } else {
            /* Return last-known discovery fields without touching MMIO while powered down. */
            magic = adapter->DeviceMmioMagic;
            abiVersion = adapter->DeviceAbiVersion;
            if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_NEW_AGPU) {
                features = adapter->DeviceFeatures;
                if ((features & AEROGPU_UMDPRIV_FEATURE_FENCE_PAGE) != 0 && adapter->FencePageVa != NULL &&
                    adapter->FencePagePa.QuadPart != 0) {
                    fencePageGpa = (ULONGLONG)adapter->FencePagePa.QuadPart;
                }
            }
        }

        out->device_mmio_magic = magic;
        out->device_abi_version_u32 = abiVersion;
        out->device_features = features;

        ULONG flags = 0;
        if (magic == AEROGPU_UMDPRIV_MMIO_MAGIC_LEGACY_ARGP) {
            flags |= AEROGPU_UMDPRIV_FLAG_IS_LEGACY;
        }
        if (features & AEROGPU_UMDPRIV_FEATURE_VBLANK) {
            flags |= AEROGPU_UMDPRIV_FLAG_HAS_VBLANK;
        }
        if (fencePageGpa != 0) {
            flags |= AEROGPU_UMDPRIV_FLAG_HAS_FENCE_PAGE;
        }
        out->flags = flags;
        return STATUS_SUCCESS;
    }

    default:
        return STATUS_NOT_SUPPORTED;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiQueryChildRelations(_In_ const HANDLE hAdapter,
                                                      _Inout_ DXGKARG_QUERYCHILDRELATIONS* pRelations)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pRelations || !pRelations->pChildRelations) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pRelations->ChildRelationsCount < 1) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    RtlZeroMemory(&pRelations->pChildRelations[0], sizeof(pRelations->pChildRelations[0]));
    pRelations->pChildRelations[0].ChildDeviceType = DXGK_CHILD_DEVICE_TYPE_MONITOR;
    /*
     * Virtual monitor is always connected; advertising HPD awareness helps Win7's
     * display stack avoid treating the output as hotpluggable/unknown.
     */
    pRelations->pChildRelations[0].ChildCapabilities.Type.VideoOutput.HpdAwareness = HpdAwarenessAlwaysConnected;
    pRelations->pChildRelations[0].ChildUid = AEROGPU_CHILD_UID;
    pRelations->pChildRelations[0].AcpiUid = 0;

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiQueryChildStatus(_In_ const HANDLE hAdapter,
                                                   _Inout_ DXGKARG_QUERYCHILDSTATUS* pChildStatus)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pChildStatus) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pChildStatus->ChildUid != AEROGPU_CHILD_UID) {
        return STATUS_INVALID_PARAMETER;
    }

    switch (pChildStatus->Type) {
    case StatusConnection:
        pChildStatus->HotPlug.Connected = TRUE;
        return STATUS_SUCCESS;
    default:
        return STATUS_SUCCESS;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiQueryDeviceDescriptor(_In_ const HANDLE hAdapter,
                                                         _Inout_ DXGKARG_QUERYDEVICE_DESCRIPTOR* pDescriptor)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pDescriptor || !pDescriptor->pDescriptorBuffer) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pDescriptor->ChildUid != AEROGPU_CHILD_UID) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pDescriptor->DescriptorOffset >= sizeof(g_AeroGpuEdid)) {
        return STATUS_INVALID_PARAMETER;
    }

    ULONG remaining = (ULONG)sizeof(g_AeroGpuEdid) - pDescriptor->DescriptorOffset;
    ULONG toCopy = pDescriptor->DescriptorLength;
    if (toCopy > remaining) {
        toCopy = remaining;
    }
    RtlCopyMemory(pDescriptor->pDescriptorBuffer, g_AeroGpuEdid + pDescriptor->DescriptorOffset, toCopy);
    pDescriptor->DescriptorLength = toCopy;
    return STATUS_SUCCESS;
}

static BOOLEAN AeroGpuIsSupportedVidPnModeDimensions(_In_ ULONG Width, _In_ ULONG Height)
{
    if (!AeroGpuModeWithinMax(Width, Height)) {
        return FALSE;
    }

    /*
     * Keep the supported-mode predicate consistent with our VidPN mode enumeration
     * (AeroGpuBuildModeList / EnumVidPnCofuncModality) so Windows does not offer
     * modes that we later reject.
     */
    AEROGPU_DISPLAY_MODE modes[16];
    const UINT count = AeroGpuBuildModeList(modes, (UINT)(sizeof(modes) / sizeof(modes[0])));

    if (AeroGpuModeListContains(modes, count, Width, Height)) {
        return TRUE;
    }

    /*
     * Allow minor rounding differences (for example 1366 vs 1368, or 768 vs 769)
     * that can arise from EDID standard timing quantisation.
     */
    for (UINT i = 0; i < count; ++i) {
        const ULONG mw = modes[i].Width;
        const ULONG mh = modes[i].Height;
        const ULONG diffW = (mw > Width) ? (mw - Width) : (Width - mw);
        const ULONG diffH = (mh > Height) ? (mh - Height) : (Height - mh);
        if (diffW <= 2u && diffH <= 2u) {
            return TRUE;
        }
    }

    return FALSE;
}

static __forceinline BOOLEAN AeroGpuVidPnModeDimsApproximatelyEqual(_In_ ULONG W0, _In_ ULONG H0, _In_ ULONG W1, _In_ ULONG H1)
{
    const ULONG diffW = (W0 > W1) ? (W0 - W1) : (W1 - W0);
    const ULONG diffH = (H0 > H1) ? (H0 - H1) : (H1 - H0);
    return (diffW <= 2u && diffH <= 2u) ? TRUE : FALSE;
}

static BOOLEAN AeroGpuIsSupportedVidPnPixelFormat(_In_ D3DDDIFORMAT Format)
{
    /*
     * MVP scanout formats:
     * - D3DDDIFMT_X8R8G8B8 (default desktop format on Win7)
     * - D3DDDIFMT_A8R8G8B8 (same memory layout; alpha ignored / treated as opaque for scanout)
     */
    switch (Format) {
    case D3DDDIFMT_X8R8G8B8:
    case D3DDDIFMT_A8R8G8B8:
        return TRUE;
    default:
        return FALSE;
    }
}

static BOOLEAN AeroGpuIsSupportedVidPnVSyncFrequency(_In_ ULONG Numerator, _In_ ULONG Denominator)
{
    /*
     * AeroGPU's MVP scanout uses a fixed ~60 Hz vblank cadence today.
     *
     * Win7's VidPN construction may describe modes with slightly different
     * refresh rates due to EDID-derived fractional values (e.g. 59.94 Hz
     * encoded as 60000/1001 or 59940/1000) or UI rounding (59 Hz).
     *
     * Be tolerant of minor encoding differences, but do not claim support for
     * arbitrary refresh rates since the emulator scanout cadence is not yet
     * mode-dependent.
     *
     * Treat 0/0 as uninitialized (allow) since some dxgkrnl helper paths may
     * leave frequency fields unset during intermediate VidPN construction.
     */
    if (Numerator == 0 && Denominator == 0) {
        return TRUE;
    }

    if (Numerator == 0 || Denominator == 0) {
        return FALSE;
    }

    /* Convert to milli-Hz for integer comparison. */
    const ULONGLONG num = (ULONGLONG)Numerator * 1000ull;
    const ULONGLONG den = (ULONGLONG)Denominator;
    const ULONGLONG mhz = num / den;

    /*
     * Accept ~60 Hz only (59-61 Hz inclusive) to match the MVP scanout/vblank
     * implementation.
     */
    if (mhz < 59000ull || mhz > 61000ull) {
        return FALSE;
    }
    return TRUE;
}

static NTSTATUS APIENTRY AeroGpuDdiIsSupportedVidPn(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_ISSUPPORTEDVIDPN* pIsSupportedVidPn)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!pIsSupportedVidPn) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Default to conservative rejection. */
    pIsSupportedVidPn->IsVidPnSupported = FALSE;

    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!pIsSupportedVidPn->hDesiredVidPn) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_VIDPN_INTERFACE vidpn;
    RtlZeroMemory(&vidpn, sizeof(vidpn));
    if (!adapter->DxgkInterface.DxgkCbQueryVidPnInterface) {
        return STATUS_SUCCESS;
    }

    NTSTATUS status =
        adapter->DxgkInterface.DxgkCbQueryVidPnInterface(adapter->StartInfo.hDxgkHandle, pIsSupportedVidPn->hDesiredVidPn, &vidpn);
    if (!NT_SUCCESS(status)) {
        return STATUS_SUCCESS;
    }

    if (!vidpn.pfnGetTopology || !vidpn.pfnGetTopologyInterface || !vidpn.pfnReleaseTopology || !vidpn.pfnGetSourceModeSet ||
        !vidpn.pfnGetSourceModeSetInterface || !vidpn.pfnReleaseSourceModeSet || !vidpn.pfnGetTargetModeSet ||
        !vidpn.pfnGetTargetModeSetInterface || !vidpn.pfnReleaseTargetModeSet) {
        return STATUS_SUCCESS;
    }

    BOOLEAN supported = TRUE;
    D3DKMDT_HVIDPNTOPOLOGY hTopology = 0;
    D3DKMDT_HVIDPNSOURCEMODESET hSourceModeSet = 0;
    D3DKMDT_HVIDPNTARGETMODESET hTargetModeSet = 0;
    BOOLEAN havePinnedSourceDims = FALSE;
    ULONG pinnedSourceW = 0;
    ULONG pinnedSourceH = 0;
    BOOLEAN havePinnedTargetDims = FALSE;
    ULONG pinnedTargetW = 0;
    ULONG pinnedTargetH = 0;

    AEROGPU_DISPLAY_MODE sourceDims[16];
    UINT sourceDimCount = 0;
    AEROGPU_DISPLAY_MODE targetDims[16];
    UINT targetDimCount = 0;

    status = vidpn.pfnGetTopology(pIsSupportedVidPn->hDesiredVidPn, &hTopology);
    if (!NT_SUCCESS(status) || !hTopology) {
        supported = FALSE;
        goto Cleanup;
    }

    DXGK_VIDPNTOPOLOGY_INTERFACE topo;
    RtlZeroMemory(&topo, sizeof(topo));
    status = vidpn.pfnGetTopologyInterface(pIsSupportedVidPn->hDesiredVidPn, hTopology, &topo);
    if (!NT_SUCCESS(status) || !topo.pfnGetNumPaths || !topo.pfnAcquireFirstPathInfo || !topo.pfnReleasePathInfo) {
        supported = FALSE;
        goto Cleanup;
    }

    UINT numPaths = 0;
    status = topo.pfnGetNumPaths(hTopology, &numPaths);
    if (!NT_SUCCESS(status)) {
        supported = FALSE;
        goto Cleanup;
    }
    if (numPaths != 1) {
        supported = FALSE;
        goto Cleanup;
    }

    const D3DKMDT_VIDPN_PRESENT_PATH* pathInfo = NULL;
    status = topo.pfnAcquireFirstPathInfo(hTopology, &pathInfo);
    if (!NT_SUCCESS(status) || !pathInfo) {
        supported = FALSE;
        goto Cleanup;
    }

    /* Strict 1 source -> 1 target topology. */
    if (pathInfo->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID || pathInfo->VidPnTargetId != AEROGPU_VIDPN_TARGET_ID) {
        supported = FALSE;
    }

    /* No rotation/scaling support (identity-only). */
    if (supported) {
        const D3DKMDT_VIDPN_PRESENT_PATH_ROTATION rot = pathInfo->ContentTransformation.Rotation;
        if (rot != D3DKMDT_VPPR_IDENTITY && rot != D3DKMDT_VPPR_UNINITIALIZED) {
            supported = FALSE;
        }
        const D3DKMDT_VIDPN_PRESENT_PATH_SCALING sc = pathInfo->ContentTransformation.Scaling;
        if (sc != D3DKMDT_VPPS_IDENTITY && sc != D3DKMDT_VPPS_UNINITIALIZED) {
            supported = FALSE;
        }
    }

    topo.pfnReleasePathInfo(hTopology, pathInfo);
    pathInfo = NULL;

    if (!supported) {
        goto Cleanup;
    }

    status = vidpn.pfnGetSourceModeSet(pIsSupportedVidPn->hDesiredVidPn, AEROGPU_VIDPN_SOURCE_ID, &hSourceModeSet);
    if (!NT_SUCCESS(status) || !hSourceModeSet) {
        supported = FALSE;
        goto Cleanup;
    }

    DXGK_VIDPNSOURCEMODESET_INTERFACE sms;
    RtlZeroMemory(&sms, sizeof(sms));
    status = vidpn.pfnGetSourceModeSetInterface(pIsSupportedVidPn->hDesiredVidPn, hSourceModeSet, &sms);
    if (!NT_SUCCESS(status) || !sms.pfnReleaseModeInfo) {
        supported = FALSE;
        goto Cleanup;
    }

    /* Validate the pinned source mode (format + dimensions), if present. */
    if (sms.pfnAcquirePinnedModeInfo) {
        const D3DKMDT_VIDPN_SOURCE_MODE* pinned = NULL;
        status = sms.pfnAcquirePinnedModeInfo(hSourceModeSet, &pinned);
        if (NT_SUCCESS(status) && pinned && pinned->Type == D3DKMDT_RMT_GRAPHICS) {
            pinnedSourceW = pinned->Format.Graphics.PrimSurfSize.cx;
            pinnedSourceH = pinned->Format.Graphics.PrimSurfSize.cy;
            const D3DDDIFORMAT fmt = pinned->Format.Graphics.PixelFormat;
            const LONG stride = pinned->Format.Graphics.Stride;

            if (stride < 0) {
                supported = FALSE;
            } else if (stride > 0 && pinnedSourceW != 0 && pinnedSourceW <= (0xFFFFFFFFu / 4u) &&
                       (ULONG)stride < pinnedSourceW * 4u) {
                supported = FALSE;
            } else if (!AeroGpuIsSupportedVidPnPixelFormat(fmt) ||
                       !AeroGpuIsSupportedVidPnModeDimensions(pinnedSourceW, pinnedSourceH)) {
                supported = FALSE;
            } else {
                havePinnedSourceDims = TRUE;
                AeroGpuModeListAddUnique(sourceDims,
                                         &sourceDimCount,
                                         (UINT)(sizeof(sourceDims) / sizeof(sourceDims[0])),
                                         pinnedSourceW,
                                         pinnedSourceH);
            }
        }
        if (pinned) {
            sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
        }
    }

    if (supported) {
        /*
         * Collect all supported source-mode dimensions.
         *
         * Be tolerant: during intermediate VidPN construction, dxgkrnl can
         * temporarily populate the source mode set with modes we will later
         * prune in EnumVidPnCofuncModality. We only require that at least one
         * supported mode exists (or a supported pinned mode), not that every
         * entry is supported.
         */
        if (sms.pfnAcquireFirstModeInfo && sms.pfnAcquireNextModeInfo) {
            const D3DKMDT_VIDPN_SOURCE_MODE* mode = NULL;
            status = sms.pfnAcquireFirstModeInfo(hSourceModeSet, &mode);
            if (mode == NULL) {
                /*
                 * Some VidPN construction paths can temporarily leave the mode
                 * set empty while still having a pinned mode selection. Accept
                 * the proposal iff we have a valid pinned mode.
                 */
                if (status != STATUS_SUCCESS && status != STATUS_GRAPHICS_NO_MORE_ELEMENTS && status != STATUS_NO_MORE_ENTRIES) {
                    supported = FALSE;
                } else if (!havePinnedSourceDims) {
                    supported = FALSE;
                }
            } else {
                if (status != STATUS_SUCCESS) {
                    /* Defensive: unexpected failure with a non-null mode pointer. */
                    supported = FALSE;
                    sms.pfnReleaseModeInfo(hSourceModeSet, mode);
                } else {
                    for (;;) {
                        if (mode->Type == D3DKMDT_RMT_GRAPHICS) {
                            const ULONG w = mode->Format.Graphics.PrimSurfSize.cx;
                            const ULONG h = mode->Format.Graphics.PrimSurfSize.cy;
                            const D3DDDIFORMAT fmt = mode->Format.Graphics.PixelFormat;
                            const LONG stride = mode->Format.Graphics.Stride;

                            if (stride >= 0 &&
                                (stride == 0 || (w != 0 && w <= (0xFFFFFFFFu / 4u) && (ULONG)stride >= (w * 4u))) &&
                                AeroGpuIsSupportedVidPnPixelFormat(fmt) && AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
                                AeroGpuModeListAddUnique(sourceDims,
                                                         &sourceDimCount,
                                                         (UINT)(sizeof(sourceDims) / sizeof(sourceDims[0])),
                                                         w,
                                                         h);
                            }
                        }

                        const D3DKMDT_VIDPN_SOURCE_MODE* next = NULL;
                        NTSTATUS stNext = sms.pfnAcquireNextModeInfo(hSourceModeSet, mode, &next);
                        sms.pfnReleaseModeInfo(hSourceModeSet, mode);
                        mode = next;

                        if (mode == NULL) {
                            /* End of enumeration. Some WDDM helpers return STATUS_GRAPHICS_NO_MORE_ELEMENTS here. */
                            if (stNext != STATUS_SUCCESS && stNext != STATUS_GRAPHICS_NO_MORE_ELEMENTS &&
                                stNext != STATUS_NO_MORE_ENTRIES) {
                                supported = FALSE;
                            }
                            break;
                        }

                        if (stNext != STATUS_SUCCESS) {
                            supported = FALSE;
                            sms.pfnReleaseModeInfo(hSourceModeSet, mode);
                            mode = NULL;
                            break;
                        }
                    }
                }
            }
        } else if (!havePinnedSourceDims) {
            supported = FALSE;
        }
    }

    if (!supported || sourceDimCount == 0) {
        supported = FALSE;
        goto Cleanup;
    }

    /* Validate target mode set (must be progressive and match supported dimensions). */
    status = vidpn.pfnGetTargetModeSet(pIsSupportedVidPn->hDesiredVidPn, AEROGPU_VIDPN_TARGET_ID, &hTargetModeSet);
    if (!NT_SUCCESS(status) || !hTargetModeSet) {
        supported = FALSE;
        goto Cleanup;
    }

    DXGK_VIDPNTARGETMODESET_INTERFACE tms;
    RtlZeroMemory(&tms, sizeof(tms));
    status = vidpn.pfnGetTargetModeSetInterface(pIsSupportedVidPn->hDesiredVidPn, hTargetModeSet, &tms);
    if (!NT_SUCCESS(status) || !tms.pfnReleaseModeInfo) {
        supported = FALSE;
        goto Cleanup;
    }

    if (tms.pfnAcquirePinnedModeInfo) {
        const D3DKMDT_VIDPN_TARGET_MODE* pinned = NULL;
        status = tms.pfnAcquirePinnedModeInfo(hTargetModeSet, &pinned);
        if (NT_SUCCESS(status) && pinned) {
            pinnedTargetW = pinned->VideoSignalInfo.ActiveSize.cx;
            pinnedTargetH = pinned->VideoSignalInfo.ActiveSize.cy;
            const D3DKMDT_VIDEO_SIGNAL_SCANLINE_ORDERING order = pinned->VideoSignalInfo.ScanLineOrdering;
            if (!AeroGpuIsSupportedVidPnModeDimensions(pinnedTargetW, pinnedTargetH) ||
                (order != D3DKMDT_VSSLO_PROGRESSIVE && order != D3DKMDT_VSSLO_UNINITIALIZED) ||
                !AeroGpuIsSupportedVidPnVSyncFrequency(pinned->VideoSignalInfo.VSyncFreq.Numerator,
                                                      pinned->VideoSignalInfo.VSyncFreq.Denominator)) {
                supported = FALSE;
            } else {
                havePinnedTargetDims = TRUE;
                AeroGpuModeListAddUnique(targetDims,
                                         &targetDimCount,
                                         (UINT)(sizeof(targetDims) / sizeof(targetDims[0])),
                                         pinnedTargetW,
                                         pinnedTargetH);
            }
        }
        if (pinned) {
            tms.pfnReleaseModeInfo(hTargetModeSet, pinned);
        }
    }

    if (supported) {
        if (tms.pfnAcquireFirstModeInfo && tms.pfnAcquireNextModeInfo) {
            const D3DKMDT_VIDPN_TARGET_MODE* mode = NULL;
            status = tms.pfnAcquireFirstModeInfo(hTargetModeSet, &mode);
            if (mode == NULL) {
                if (status != STATUS_SUCCESS && status != STATUS_GRAPHICS_NO_MORE_ELEMENTS && status != STATUS_NO_MORE_ENTRIES) {
                    supported = FALSE;
                } else if (!havePinnedTargetDims) {
                    supported = FALSE;
                }
            } else {
                if (status != STATUS_SUCCESS) {
                    supported = FALSE;
                    tms.pfnReleaseModeInfo(hTargetModeSet, mode);
                } else {
                    for (;;) {
                        const ULONG w = mode->VideoSignalInfo.ActiveSize.cx;
                        const ULONG h = mode->VideoSignalInfo.ActiveSize.cy;
                        const D3DKMDT_VIDEO_SIGNAL_SCANLINE_ORDERING order = mode->VideoSignalInfo.ScanLineOrdering;
                        if (AeroGpuIsSupportedVidPnModeDimensions(w, h) &&
                            (order == D3DKMDT_VSSLO_PROGRESSIVE || order == D3DKMDT_VSSLO_UNINITIALIZED) &&
                            AeroGpuIsSupportedVidPnVSyncFrequency(mode->VideoSignalInfo.VSyncFreq.Numerator,
                                                                 mode->VideoSignalInfo.VSyncFreq.Denominator)) {
                            AeroGpuModeListAddUnique(targetDims,
                                                     &targetDimCount,
                                                     (UINT)(sizeof(targetDims) / sizeof(targetDims[0])),
                                                     w,
                                                     h);
                        }

                        const D3DKMDT_VIDPN_TARGET_MODE* next = NULL;
                        NTSTATUS stNext = tms.pfnAcquireNextModeInfo(hTargetModeSet, mode, &next);
                        tms.pfnReleaseModeInfo(hTargetModeSet, mode);
                        mode = next;

                        if (mode == NULL) {
                            if (stNext != STATUS_SUCCESS && stNext != STATUS_GRAPHICS_NO_MORE_ELEMENTS &&
                                stNext != STATUS_NO_MORE_ENTRIES) {
                                supported = FALSE;
                            }
                            break;
                        }

                        if (stNext != STATUS_SUCCESS) {
                            supported = FALSE;
                            tms.pfnReleaseModeInfo(hTargetModeSet, mode);
                            mode = NULL;
                            break;
                        }
                    }
                }
            }
        } else if (!havePinnedTargetDims) {
            supported = FALSE;
        }
    }

    if (!supported || targetDimCount == 0) {
        supported = FALSE;
        goto Cleanup;
    }

    if (havePinnedSourceDims && havePinnedTargetDims) {
        if (!AeroGpuVidPnModeDimsApproximatelyEqual(pinnedSourceW, pinnedSourceH, pinnedTargetW, pinnedTargetH)) {
            supported = FALSE;
            goto Cleanup;
        }
    }

    /* Require at least one common mode between source and target sets. */
    {
        BOOLEAN haveCommon = FALSE;
        for (UINT i = 0; i < sourceDimCount && !haveCommon; ++i) {
            for (UINT j = 0; j < targetDimCount; ++j) {
                if (AeroGpuVidPnModeDimsApproximatelyEqual(sourceDims[i].Width, sourceDims[i].Height, targetDims[j].Width, targetDims[j].Height)) {
                    haveCommon = TRUE;
                    break;
                }
            }
        }
        if (!haveCommon) {
            supported = FALSE;
            goto Cleanup;
        }
    }

Cleanup:
    if (hSourceModeSet) {
        vidpn.pfnReleaseSourceModeSet(pIsSupportedVidPn->hDesiredVidPn, hSourceModeSet);
    }
    if (hTargetModeSet) {
        vidpn.pfnReleaseTargetModeSet(pIsSupportedVidPn->hDesiredVidPn, hTargetModeSet);
    }
    if (hTopology) {
        vidpn.pfnReleaseTopology(pIsSupportedVidPn->hDesiredVidPn, hTopology);
    }

    pIsSupportedVidPn->IsVidPnSupported = supported ? TRUE : FALSE;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiQueryVidPnHardwareCapability(_In_ const HANDLE hAdapter,
                                                                _Inout_ DXGKARG_QUERYVIDPNHARDWARECAPABILITY* pCapability)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pCapability) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Single-head MVP: only VidPn source 0 is valid.
     *
     * Win7 dxgkrnl should only query this once, but validate defensively.
     */
    if (pCapability->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * MVP: report minimal capabilities consistent with our current modesetting
     * path (no scaling, no rotation, no overlays).
     *
     * dxgkrnl treats a zeroed capability struct as "no optional features".
     */
    RtlZeroMemory(&pCapability->VidPnHardwareCapability, sizeof(pCapability->VidPnHardwareCapability));
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRecommendFunctionalVidPn(_In_ const HANDLE hAdapter,
                                                            _Inout_ DXGKARG_RECOMMENDFUNCTIONALVIDPN* pRecommend)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pRecommend) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!adapter->DxgkInterface.DxgkCbQueryVidPnInterface || !pRecommend->hFunctionalVidPn) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_VIDPN_INTERFACE vidpn;
    RtlZeroMemory(&vidpn, sizeof(vidpn));
    NTSTATUS status =
        adapter->DxgkInterface.DxgkCbQueryVidPnInterface(adapter->StartInfo.hDxgkHandle, pRecommend->hFunctionalVidPn, &vidpn);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (!vidpn.pfnCreateNewTopology || !vidpn.pfnGetTopologyInterface || !vidpn.pfnAssignTopology || !vidpn.pfnReleaseTopology) {
        return STATUS_NOT_SUPPORTED;
    }

    if (!vidpn.pfnCreateNewSourceModeSet || !vidpn.pfnAssignSourceModeSet || !vidpn.pfnGetSourceModeSetInterface ||
        !vidpn.pfnReleaseSourceModeSet || !vidpn.pfnCreateNewTargetModeSet || !vidpn.pfnAssignTargetModeSet ||
        !vidpn.pfnGetTargetModeSetInterface || !vidpn.pfnReleaseTargetModeSet) {
        return STATUS_NOT_SUPPORTED;
    }

    /* Build a conservative 32bpp @ 60Hz mode list up front. */
    AEROGPU_DISPLAY_MODE modes[16];
    UINT modeCount = AeroGpuBuildModeList(modes, (UINT)(sizeof(modes) / sizeof(modes[0])));
    if (modeCount == 0) {
        /*
         * This can only happen if a registry max-resolution cap filters out all
         * modes. Avoid returning an empty recommended VidPN.
         */
        return STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN;
    }

    const ULONG pinW = modes[0].Width;
    const ULONG pinH = modes[0].Height;

    D3DKMDT_HVIDPNTOPOLOGY hTopology = 0;
    D3DKMDT_HVIDPNSOURCEMODESET hSourceModeSet = 0;
    D3DKMDT_HVIDPNTARGETMODESET hTargetModeSet = 0;

    status = vidpn.pfnCreateNewTopology(pRecommend->hFunctionalVidPn, &hTopology);
    if (!NT_SUCCESS(status) || !hTopology) {
        return NT_SUCCESS(status) ? STATUS_INSUFFICIENT_RESOURCES : status;
    }

    DXGK_VIDPNTOPOLOGY_INTERFACE topo;
    RtlZeroMemory(&topo, sizeof(topo));
    status = vidpn.pfnGetTopologyInterface(pRecommend->hFunctionalVidPn, hTopology, &topo);
    if (!NT_SUCCESS(status) || !topo.pfnCreateNewPathInfo || !topo.pfnAddPath || !topo.pfnReleasePathInfo) {
        status = STATUS_NOT_SUPPORTED;
        goto Cleanup;
    }

    D3DKMDT_VIDPN_PRESENT_PATH* path = NULL;
    status = topo.pfnCreateNewPathInfo(hTopology, &path);
    if (!NT_SUCCESS(status) || !path) {
        status = NT_SUCCESS(status) ? STATUS_INSUFFICIENT_RESOURCES : status;
        goto Cleanup;
    }

    RtlZeroMemory(path, sizeof(*path));
    path->VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
    path->VidPnTargetId = AEROGPU_VIDPN_TARGET_ID;
    path->ContentTransformation.Rotation = D3DKMDT_VPPR_IDENTITY;
    path->ContentTransformation.Scaling = D3DKMDT_VPPS_IDENTITY;

    status = topo.pfnAddPath(hTopology, path);
    topo.pfnReleasePathInfo(hTopology, path);
    if (!NT_SUCCESS(status)) {
        goto Cleanup;
    }

    status = vidpn.pfnAssignTopology(pRecommend->hFunctionalVidPn, hTopology);
    if (!NT_SUCCESS(status)) {
        goto Cleanup;
    }

    status = vidpn.pfnCreateNewSourceModeSet(pRecommend->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, &hSourceModeSet);
    if (!NT_SUCCESS(status) || !hSourceModeSet) {
        status = NT_SUCCESS(status) ? STATUS_INSUFFICIENT_RESOURCES : status;
        goto Cleanup;
    }

    DXGK_VIDPNSOURCEMODESET_INTERFACE sms;
    RtlZeroMemory(&sms, sizeof(sms));
    status = vidpn.pfnGetSourceModeSetInterface(pRecommend->hFunctionalVidPn, hSourceModeSet, &sms);
    if (!NT_SUCCESS(status) || !sms.pfnCreateNewModeInfo || !sms.pfnAddMode || !sms.pfnReleaseModeInfo) {
        status = STATUS_NOT_SUPPORTED;
        goto Cleanup;
    }

    BOOLEAN addedAnySourceMode = FALSE;
    for (UINT i = 0; i < modeCount; ++i) {
        const ULONG w = modes[i].Width;
        const ULONG h = modes[i].Height;
        if (!AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
            continue;
        }

        {
            ULONG pitch = 0;
            if (!AeroGpuComputeDefaultPitchBytes(w, &pitch)) {
                pitch = w * 4u;
            }
            const LONG stride = (LONG)pitch;

            const D3DDDIFORMAT fmts[] = {D3DDDIFMT_X8R8G8B8, D3DDDIFMT_A8R8G8B8};
            for (UINT fi = 0; fi < (UINT)(sizeof(fmts) / sizeof(fmts[0])); ++fi) {
                if (!AeroGpuIsSupportedVidPnPixelFormat(fmts[fi])) {
                    continue;
                }

                D3DKMDT_VIDPN_SOURCE_MODE* modeInfo = NULL;
                NTSTATUS st2 = sms.pfnCreateNewModeInfo(hSourceModeSet, &modeInfo);
                if (!NT_SUCCESS(st2) || !modeInfo) {
                    continue;
                }

                RtlZeroMemory(modeInfo, sizeof(*modeInfo));
                modeInfo->Type = D3DKMDT_RMT_GRAPHICS;
                modeInfo->Format.Graphics.PrimSurfSize.cx = w;
                modeInfo->Format.Graphics.PrimSurfSize.cy = h;
                modeInfo->Format.Graphics.VisibleRegionSize.cx = w;
                modeInfo->Format.Graphics.VisibleRegionSize.cy = h;
                modeInfo->Format.Graphics.Stride = stride;
                modeInfo->Format.Graphics.PixelFormat = fmts[fi];

                st2 = sms.pfnAddMode(hSourceModeSet, modeInfo);
                if (NT_SUCCESS(st2) && sms.pfnPinMode && w == pinW && h == pinH && fmts[fi] == D3DDDIFMT_X8R8G8B8) {
                    (void)sms.pfnPinMode(hSourceModeSet, modeInfo);
                }
                if (NT_SUCCESS(st2)) {
                    addedAnySourceMode = TRUE;
                }

                sms.pfnReleaseModeInfo(hSourceModeSet, modeInfo);
            }
        }

    }

    if (!addedAnySourceMode) {
        status = STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN;
        goto Cleanup;
    }

    status = vidpn.pfnAssignSourceModeSet(pRecommend->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, hSourceModeSet);
    if (!NT_SUCCESS(status)) {
        goto Cleanup;
    }

    status = vidpn.pfnCreateNewTargetModeSet(pRecommend->hFunctionalVidPn, AEROGPU_VIDPN_TARGET_ID, &hTargetModeSet);
    if (!NT_SUCCESS(status) || !hTargetModeSet) {
        status = NT_SUCCESS(status) ? STATUS_INSUFFICIENT_RESOURCES : status;
        goto Cleanup;
    }

    DXGK_VIDPNTARGETMODESET_INTERFACE tms;
    RtlZeroMemory(&tms, sizeof(tms));
    status = vidpn.pfnGetTargetModeSetInterface(pRecommend->hFunctionalVidPn, hTargetModeSet, &tms);
    if (!NT_SUCCESS(status) || !tms.pfnCreateNewModeInfo || !tms.pfnAddMode || !tms.pfnReleaseModeInfo) {
        status = STATUS_NOT_SUPPORTED;
        goto Cleanup;
    }

    BOOLEAN addedAnyTargetMode = FALSE;
    for (UINT i = 0; i < modeCount; ++i) {
        const ULONG w = modes[i].Width;
        const ULONG h = modes[i].Height;
        if (!AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
            continue;
        }

        D3DKMDT_VIDPN_TARGET_MODE* modeInfo = NULL;
        NTSTATUS st2 = tms.pfnCreateNewModeInfo(hTargetModeSet, &modeInfo);
        if (!NT_SUCCESS(st2) || !modeInfo) {
            continue;
        }

        RtlZeroMemory(modeInfo, sizeof(*modeInfo));
        modeInfo->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
        modeInfo->VideoSignalInfo.ActiveSize.cx = w;
        modeInfo->VideoSignalInfo.ActiveSize.cy = h;
        modeInfo->VideoSignalInfo.TotalSize.cx = AeroGpuComputeTotalWidthForActiveWidth(w);
        modeInfo->VideoSignalInfo.TotalSize.cy = h + AeroGpuComputeVblankLineCountForActiveHeight(h);
        modeInfo->VideoSignalInfo.VSyncFreq.Numerator = 60;
        modeInfo->VideoSignalInfo.VSyncFreq.Denominator = 1;
        modeInfo->VideoSignalInfo.HSyncFreq.Numerator = 60 * modeInfo->VideoSignalInfo.TotalSize.cy;
        modeInfo->VideoSignalInfo.HSyncFreq.Denominator = 1;
        {
            ULONGLONG pixelRate =
                (ULONGLONG)60ull * (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cx * (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cy;
            if (pixelRate > (ULONGLONG)0xFFFFFFFFu) {
                pixelRate = 0;
            }
            modeInfo->VideoSignalInfo.PixelRate = (ULONG)pixelRate;
        }
        modeInfo->VideoSignalInfo.ScanLineOrdering = D3DKMDT_VSSLO_PROGRESSIVE;

        st2 = tms.pfnAddMode(hTargetModeSet, modeInfo);
        if (NT_SUCCESS(st2) && tms.pfnPinMode && w == pinW && h == pinH) {
            (void)tms.pfnPinMode(hTargetModeSet, modeInfo);
        }
        if (NT_SUCCESS(st2)) {
            addedAnyTargetMode = TRUE;
        }

        tms.pfnReleaseModeInfo(hTargetModeSet, modeInfo);
    }

    if (!addedAnyTargetMode) {
        status = STATUS_GRAPHICS_NO_RECOMMENDED_FUNCTIONAL_VIDPN;
        goto Cleanup;
    }

    status = vidpn.pfnAssignTargetModeSet(pRecommend->hFunctionalVidPn, AEROGPU_VIDPN_TARGET_ID, hTargetModeSet);

Cleanup:
    if (hSourceModeSet) {
        vidpn.pfnReleaseSourceModeSet(pRecommend->hFunctionalVidPn, hSourceModeSet);
    }
    if (hTargetModeSet) {
        vidpn.pfnReleaseTargetModeSet(pRecommend->hFunctionalVidPn, hTargetModeSet);
    }
    if (hTopology) {
        vidpn.pfnReleaseTopology(pRecommend->hFunctionalVidPn, hTopology);
    }
    return status;
}

static NTSTATUS APIENTRY AeroGpuDdiEnumVidPnCofuncModality(_In_ const HANDLE hAdapter,
                                                           _Inout_ DXGKARG_ENUMVIDPNCOFUNCMODALITY* pEnum)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pEnum) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!adapter->DxgkInterface.DxgkCbQueryVidPnInterface || !pEnum->hFunctionalVidPn) {
        /* Keep bring-up tolerant: accept even if we can't introspect/populate the VidPN. */
        return STATUS_SUCCESS;
    }

    DXGK_VIDPN_INTERFACE vidpn;
    RtlZeroMemory(&vidpn, sizeof(vidpn));

    NTSTATUS status =
        adapter->DxgkInterface.DxgkCbQueryVidPnInterface(adapter->StartInfo.hDxgkHandle, pEnum->hFunctionalVidPn, &vidpn);
    if (!NT_SUCCESS(status)) {
        return STATUS_SUCCESS;
    }

    /*
     * Validate topology: AeroGPU supports exactly one path (source 0 -> target 0)
     * with identity transforms.
     *
     * Note: keep this best-effort to avoid regressing bring-up flows if a header
     * variant omits topology callbacks.
     */
    if (vidpn.pfnGetTopology && vidpn.pfnGetTopologyInterface && vidpn.pfnReleaseTopology) {
        D3DKMDT_HVIDPNTOPOLOGY hTopology = 0;
        status = vidpn.pfnGetTopology(pEnum->hFunctionalVidPn, &hTopology);
        if (NT_SUCCESS(status) && hTopology) {
            DXGK_VIDPNTOPOLOGY_INTERFACE topo;
            RtlZeroMemory(&topo, sizeof(topo));
            status = vidpn.pfnGetTopologyInterface(pEnum->hFunctionalVidPn, hTopology, &topo);
            if (NT_SUCCESS(status) && topo.pfnGetNumPaths && topo.pfnAcquireFirstPathInfo && topo.pfnReleasePathInfo) {
                UINT numPaths = 0;
                status = topo.pfnGetNumPaths(hTopology, &numPaths);
                if (!NT_SUCCESS(status) || numPaths != 1) {
                    vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);
                    return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY;
                }

                const D3DKMDT_VIDPN_PRESENT_PATH* pathInfo = NULL;
                status = topo.pfnAcquireFirstPathInfo(hTopology, &pathInfo);
                if (!NT_SUCCESS(status) || !pathInfo) {
                    vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);
                    return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY;
                }

                BOOLEAN ok = TRUE;
                if (pathInfo->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID || pathInfo->VidPnTargetId != AEROGPU_VIDPN_TARGET_ID) {
                    ok = FALSE;
                }

                if (ok) {
                    const D3DKMDT_VIDPN_PRESENT_PATH_ROTATION rot = pathInfo->ContentTransformation.Rotation;
                    if (rot != D3DKMDT_VPPR_IDENTITY && rot != D3DKMDT_VPPR_UNINITIALIZED) {
                        topo.pfnReleasePathInfo(hTopology, pathInfo);
                        vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);
                        return STATUS_NOT_SUPPORTED;
                    }

                    const D3DKMDT_VIDPN_PRESENT_PATH_SCALING sc = pathInfo->ContentTransformation.Scaling;
                    if (sc != D3DKMDT_VPPS_IDENTITY && sc != D3DKMDT_VPPS_UNINITIALIZED) {
                        topo.pfnReleasePathInfo(hTopology, pathInfo);
                        vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);
                        return STATUS_NOT_SUPPORTED;
                    }
                }

                topo.pfnReleasePathInfo(hTopology, pathInfo);
                vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);

                if (!ok) {
                    return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY;
                }
            } else {
                vidpn.pfnReleaseTopology(pEnum->hFunctionalVidPn, hTopology);
            }
        }
    }

    if (!vidpn.pfnCreateNewSourceModeSet || !vidpn.pfnAssignSourceModeSet || !vidpn.pfnGetSourceModeSetInterface ||
        !vidpn.pfnReleaseSourceModeSet || !vidpn.pfnCreateNewTargetModeSet || !vidpn.pfnAssignTargetModeSet ||
        !vidpn.pfnGetTargetModeSetInterface || !vidpn.pfnReleaseTargetModeSet) {
        return STATUS_SUCCESS;
    }

    AEROGPU_DISPLAY_MODE modes[16];
    UINT modeCount = AeroGpuBuildModeList(modes, (UINT)(sizeof(modes) / sizeof(modes[0])));

    /* Preserve pinned source mode (if any), so dxgkrnl can keep its selection stable across enumeration calls. */
    {
        /* Also use this as the preferred pin target when we build a fresh mode set. */
        ULONG pinnedW = 0;
        ULONG pinnedH = 0;
        ULONG pinnedTargetW = 0;
        ULONG pinnedTargetH = 0;

        if (vidpn.pfnGetSourceModeSet && modeCount < (UINT)(sizeof(modes) / sizeof(modes[0]))) {
            D3DKMDT_HVIDPNSOURCEMODESET hExisting = 0;
            NTSTATUS st2 = vidpn.pfnGetSourceModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, &hExisting);
            if (NT_SUCCESS(st2) && hExisting) {
                DXGK_VIDPNSOURCEMODESET_INTERFACE smsExisting;
                RtlZeroMemory(&smsExisting, sizeof(smsExisting));
                st2 = vidpn.pfnGetSourceModeSetInterface(pEnum->hFunctionalVidPn, hExisting, &smsExisting);
                if (NT_SUCCESS(st2) && smsExisting.pfnAcquirePinnedModeInfo && smsExisting.pfnReleaseModeInfo) {
                    const D3DKMDT_VIDPN_SOURCE_MODE* pinned = NULL;
                    st2 = smsExisting.pfnAcquirePinnedModeInfo(hExisting, &pinned);
                    if (NT_SUCCESS(st2) && pinned && pinned->Type == D3DKMDT_RMT_GRAPHICS) {
                        pinnedW = pinned->Format.Graphics.PrimSurfSize.cx;
                        pinnedH = pinned->Format.Graphics.PrimSurfSize.cy;
                    }
                    if (pinned) {
                        smsExisting.pfnReleaseModeInfo(hExisting, pinned);
                    }
                }
                vidpn.pfnReleaseSourceModeSet(pEnum->hFunctionalVidPn, hExisting);
            }
        }

        if (vidpn.pfnGetTargetModeSet && modeCount < (UINT)(sizeof(modes) / sizeof(modes[0]))) {
            D3DKMDT_HVIDPNTARGETMODESET hExisting = 0;
            NTSTATUS st2 = vidpn.pfnGetTargetModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_TARGET_ID, &hExisting);
            if (NT_SUCCESS(st2) && hExisting) {
                DXGK_VIDPNTARGETMODESET_INTERFACE tmsExisting;
                RtlZeroMemory(&tmsExisting, sizeof(tmsExisting));
                st2 = vidpn.pfnGetTargetModeSetInterface(pEnum->hFunctionalVidPn, hExisting, &tmsExisting);
                if (NT_SUCCESS(st2) && tmsExisting.pfnAcquirePinnedModeInfo && tmsExisting.pfnReleaseModeInfo) {
                    const D3DKMDT_VIDPN_TARGET_MODE* pinned = NULL;
                    st2 = tmsExisting.pfnAcquirePinnedModeInfo(hExisting, &pinned);
                    if (NT_SUCCESS(st2) && pinned) {
                        pinnedTargetW = pinned->VideoSignalInfo.ActiveSize.cx;
                        pinnedTargetH = pinned->VideoSignalInfo.ActiveSize.cy;
                    }
                    if (pinned) {
                        tmsExisting.pfnReleaseModeInfo(hExisting, pinned);
                    }
                }
                vidpn.pfnReleaseTargetModeSet(pEnum->hFunctionalVidPn, hExisting);
            }
        }

        if (pinnedW != 0 && pinnedH != 0 && pinnedTargetW != 0 && pinnedTargetH != 0 &&
            !AeroGpuVidPnModeDimsApproximatelyEqual(pinnedW, pinnedH, pinnedTargetW, pinnedTargetH)) {
            return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY;
        }

        if ((pinnedW == 0 || pinnedH == 0) && pinnedTargetW != 0 && pinnedTargetH != 0) {
            pinnedW = pinnedTargetW;
            pinnedH = pinnedTargetH;
        }

        if (pinnedW != 0 && pinnedH != 0) {
            AeroGpuModeListAddUnique(modes, &modeCount, (UINT)(sizeof(modes) / sizeof(modes[0])), pinnedW, pinnedH);
        }

        /*
         * If we found a pinned mode, bubble it to the front of the list so our
         * newly-built mode sets preserve dxgkrnl's selection. This avoids
         * unnecessary mode churn during cofunctional modality enumeration.
         */
        if (pinnedW != 0 && pinnedH != 0) {
            for (UINT i = 0; i < modeCount; ++i) {
                if (modes[i].Width == pinnedW && modes[i].Height == pinnedH) {
                    if (i != 0) {
                        const AEROGPU_DISPLAY_MODE tmp = modes[0];
                        modes[0] = modes[i];
                        modes[i] = tmp;
                    }
                    break;
                }
            }
        }
    }

    if (modeCount == 0) {
        /* No supported modes; keep bring-up tolerant by leaving the VidPN unchanged. */
        return STATUS_SUCCESS;
    }

    /*
     * Create new source + target mode sets and only assign them if we successfully
     * add at least one mode to each. This avoids clearing previously valid mode
     * sets if adding modes fails for any reason.
     */
    D3DKMDT_HVIDPNSOURCEMODESET hSourceModeSet = 0;
    D3DKMDT_HVIDPNTARGETMODESET hTargetModeSet = 0;
    BOOLEAN haveSourceModes = FALSE;
    BOOLEAN haveTargetModes = FALSE;

    status = vidpn.pfnCreateNewSourceModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, &hSourceModeSet);
    if (NT_SUCCESS(status) && hSourceModeSet) {
        DXGK_VIDPNSOURCEMODESET_INTERFACE sms;
        RtlZeroMemory(&sms, sizeof(sms));
        status = vidpn.pfnGetSourceModeSetInterface(pEnum->hFunctionalVidPn, hSourceModeSet, &sms);
        if (NT_SUCCESS(status) && sms.pfnCreateNewModeInfo && sms.pfnAddMode && sms.pfnReleaseModeInfo) {
            const ULONG pinW = modes[0].Width;
            const ULONG pinH = modes[0].Height;
            for (UINT i = 0; i < modeCount; ++i) {
                const ULONG w = modes[i].Width;
                const ULONG h = modes[i].Height;
                if (!AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
                    continue;
                }

                ULONG pitch = 0;
                if (!AeroGpuComputeDefaultPitchBytes(w, &pitch)) {
                    pitch = w * 4u;
                }
                const LONG stride = (LONG)pitch;

                const D3DDDIFORMAT fmts[] = {D3DDDIFMT_X8R8G8B8, D3DDDIFMT_A8R8G8B8};
                for (UINT fi = 0; fi < (UINT)(sizeof(fmts) / sizeof(fmts[0])); ++fi) {
                    if (!AeroGpuIsSupportedVidPnPixelFormat(fmts[fi])) {
                        continue;
                    }

                    D3DKMDT_VIDPN_SOURCE_MODE* modeInfo = NULL;
                    NTSTATUS st2 = sms.pfnCreateNewModeInfo(hSourceModeSet, &modeInfo);
                    if (!NT_SUCCESS(st2) || !modeInfo) {
                        continue;
                    }

                    RtlZeroMemory(modeInfo, sizeof(*modeInfo));
                    modeInfo->Type = D3DKMDT_RMT_GRAPHICS;
                    modeInfo->Format.Graphics.PrimSurfSize.cx = w;
                    modeInfo->Format.Graphics.PrimSurfSize.cy = h;
                    modeInfo->Format.Graphics.VisibleRegionSize.cx = w;
                    modeInfo->Format.Graphics.VisibleRegionSize.cy = h;
                    modeInfo->Format.Graphics.Stride = stride;
                    modeInfo->Format.Graphics.PixelFormat = fmts[fi];

                    st2 = sms.pfnAddMode(hSourceModeSet, modeInfo);
                    if (NT_SUCCESS(st2) && sms.pfnPinMode && w == pinW && h == pinH && fmts[fi] == D3DDDIFMT_X8R8G8B8) {
                        (void)sms.pfnPinMode(hSourceModeSet, modeInfo);
                    }
                    if (NT_SUCCESS(st2)) {
                        haveSourceModes = TRUE;
                    }

                    sms.pfnReleaseModeInfo(hSourceModeSet, modeInfo);
                }
            }
        }
    }

    status = vidpn.pfnCreateNewTargetModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_TARGET_ID, &hTargetModeSet);
    if (NT_SUCCESS(status) && hTargetModeSet) {
        DXGK_VIDPNTARGETMODESET_INTERFACE tms;
        RtlZeroMemory(&tms, sizeof(tms));
        status = vidpn.pfnGetTargetModeSetInterface(pEnum->hFunctionalVidPn, hTargetModeSet, &tms);
        if (NT_SUCCESS(status) && tms.pfnCreateNewModeInfo && tms.pfnAddMode && tms.pfnReleaseModeInfo) {
            const ULONG pinW = modes[0].Width;
            const ULONG pinH = modes[0].Height;
            for (UINT i = 0; i < modeCount; ++i) {
                const ULONG w = modes[i].Width;
                const ULONG h = modes[i].Height;
                if (!AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
                    continue;
                }

                D3DKMDT_VIDPN_TARGET_MODE* modeInfo = NULL;
                NTSTATUS st2 = tms.pfnCreateNewModeInfo(hTargetModeSet, &modeInfo);
                if (!NT_SUCCESS(st2) || !modeInfo) {
                    continue;
                }

                RtlZeroMemory(modeInfo, sizeof(*modeInfo));
                modeInfo->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
                modeInfo->VideoSignalInfo.ActiveSize.cx = w;
                modeInfo->VideoSignalInfo.ActiveSize.cy = h;
                modeInfo->VideoSignalInfo.TotalSize.cx = AeroGpuComputeTotalWidthForActiveWidth(w);
                modeInfo->VideoSignalInfo.TotalSize.cy = h + AeroGpuComputeVblankLineCountForActiveHeight(h);
                modeInfo->VideoSignalInfo.VSyncFreq.Numerator = 60;
                modeInfo->VideoSignalInfo.VSyncFreq.Denominator = 1;
                modeInfo->VideoSignalInfo.HSyncFreq.Numerator = 60 * modeInfo->VideoSignalInfo.TotalSize.cy;
                modeInfo->VideoSignalInfo.HSyncFreq.Denominator = 1;
                {
                    ULONGLONG pixelRate =
                        (ULONGLONG)60ull * (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cx *
                        (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cy;
                    if (pixelRate > (ULONGLONG)0xFFFFFFFFu) {
                        pixelRate = 0;
                    }
                    modeInfo->VideoSignalInfo.PixelRate = (ULONG)pixelRate;
                }
                modeInfo->VideoSignalInfo.ScanLineOrdering = D3DKMDT_VSSLO_PROGRESSIVE;

                st2 = tms.pfnAddMode(hTargetModeSet, modeInfo);
                if (NT_SUCCESS(st2) && tms.pfnPinMode && w == pinW && h == pinH) {
                    (void)tms.pfnPinMode(hTargetModeSet, modeInfo);
                }
                if (NT_SUCCESS(st2)) {
                    haveTargetModes = TRUE;
                }

                tms.pfnReleaseModeInfo(hTargetModeSet, modeInfo);
            }
        }
    }

    if (haveSourceModes && haveTargetModes) {
        (void)vidpn.pfnAssignSourceModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, hSourceModeSet);
        (void)vidpn.pfnAssignTargetModeSet(pEnum->hFunctionalVidPn, AEROGPU_VIDPN_TARGET_ID, hTargetModeSet);
    }

    if (hSourceModeSet) {
        vidpn.pfnReleaseSourceModeSet(pEnum->hFunctionalVidPn, hSourceModeSet);
    }
    if (hTargetModeSet) {
        vidpn.pfnReleaseTargetModeSet(pEnum->hFunctionalVidPn, hTargetModeSet);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCommitVidPn(_In_ const HANDLE hAdapter, _In_ const DXGKARG_COMMITVIDPN* pCommitVidPn)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCommitVidPn) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!adapter->DxgkInterface.DxgkCbQueryVidPnInterface || !pCommitVidPn->hFunctionalVidPn) {
        /* Keep legacy behavior: accept the commit even if we can't introspect it. */
        return STATUS_SUCCESS;
    }

    DXGK_VIDPN_INTERFACE vidpn;
    RtlZeroMemory(&vidpn, sizeof(vidpn));
    NTSTATUS status =
        adapter->DxgkInterface.DxgkCbQueryVidPnInterface(adapter->StartInfo.hDxgkHandle, pCommitVidPn->hFunctionalVidPn, &vidpn);
    if (!NT_SUCCESS(status)) {
        return STATUS_SUCCESS;
    }

    if (!vidpn.pfnGetSourceModeSet || !vidpn.pfnGetSourceModeSetInterface || !vidpn.pfnReleaseSourceModeSet) {
        return STATUS_SUCCESS;
    }

    D3DKMDT_HVIDPNSOURCEMODESET hSourceModeSet = 0;
    status = vidpn.pfnGetSourceModeSet(pCommitVidPn->hFunctionalVidPn, AEROGPU_VIDPN_SOURCE_ID, &hSourceModeSet);
    if (!NT_SUCCESS(status)) {
        return STATUS_SUCCESS;
    }

    DXGK_VIDPNSOURCEMODESET_INTERFACE sms;
    RtlZeroMemory(&sms, sizeof(sms));
    status = vidpn.pfnGetSourceModeSetInterface(pCommitVidPn->hFunctionalVidPn, hSourceModeSet, &sms);
    if (!NT_SUCCESS(status)) {
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_SUCCESS;
    }

    if (!sms.pfnAcquirePinnedModeInfo || !sms.pfnReleaseModeInfo) {
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_SUCCESS;
    }

    const D3DKMDT_VIDPN_SOURCE_MODE* pinned = NULL;
    status = sms.pfnAcquirePinnedModeInfo(hSourceModeSet, &pinned);
    if (!NT_SUCCESS(status)) {
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_SUCCESS;
    }

    if (!pinned) {
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_SUCCESS;
    }

    if (pinned->Type != D3DKMDT_RMT_GRAPHICS) {
        sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_SUCCESS;
    }

    const ULONG width = pinned->Format.Graphics.PrimSurfSize.cx;
    const ULONG height = pinned->Format.Graphics.PrimSurfSize.cy;
    const D3DDDIFORMAT fmt = pinned->Format.Graphics.PixelFormat;

    if (!AeroGpuIsSupportedVidPnPixelFormat(fmt)) {
        sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_NOT_SUPPORTED;
    }

    if (width == 0 || height == 0 || width > 16384u || height > 16384u || width > (0xFFFFFFFFu / 4u)) {
        sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_INVALID_PARAMETER;
    }

    if (!AeroGpuIsSupportedVidPnModeDimensions(width, height)) {
        /*
         * Enforce the same supported-mode predicate used by our VidPN mode-set
         * enumeration. This prevents Win7 from committing an arbitrary
         * resolution even if it falls within the max caps.
         */
        sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
        vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
        return STATUS_NOT_SUPPORTED;
    }

    adapter->CurrentWidth = width;
    adapter->CurrentHeight = height;

    {
        ULONG pitch = 0;
        const LONG stride = pinned->Format.Graphics.Stride;
        if (stride > 0) {
            pitch = (ULONG)stride;
            const ULONG rowBytes = width * 4u;
            if (pitch < rowBytes) {
                pitch = rowBytes;
            }
        } else if (!AeroGpuComputeDefaultPitchBytes(width, &pitch)) {
            pitch = width * 4u;
        }

        adapter->CurrentPitch = pitch;
    }
    switch (fmt) {
    case D3DDDIFMT_A8R8G8B8:
        adapter->CurrentFormat = AEROGPU_FORMAT_B8G8R8A8_UNORM;
        break;
    case D3DDDIFMT_X8R8G8B8:
    default:
        adapter->CurrentFormat = AEROGPU_FORMAT_B8G8R8X8_UNORM;
        break;
    }

    sms.pfnReleaseModeInfo(hSourceModeSet, pinned);
    vidpn.pfnReleaseSourceModeSet(pCommitVidPn->hFunctionalVidPn, hSourceModeSet);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetVidPnSourceAddress(_In_ const HANDLE hAdapter,
                                                        _Inout_ const DXGKARG_SETVIDPNSOURCEADDRESS* pSetAddress)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pSetAddress) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pSetAddress->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    ULONG pitch = pSetAddress->PrimaryPitch;
    if (pitch == 0) {
        ULONG computed = 0;
        if (AeroGpuComputeDefaultPitchBytes(adapter->CurrentWidth, &computed)) {
            pitch = computed;
        }
    }

    if (adapter->CurrentWidth != 0 && adapter->CurrentWidth <= (0xFFFFFFFFu / 4u)) {
        const ULONG rowBytes = adapter->CurrentWidth * 4u;
        if (pitch != 0 && pitch < rowBytes) {
            pitch = rowBytes;
        }
    }

    adapter->CurrentPitch = pitch;

    PHYSICAL_ADDRESS fb;
    fb.QuadPart = pSetAddress->PrimaryAddress.QuadPart;
    adapter->CurrentScanoutFbPa = fb;
    AeroGpuProgramScanout(adapter, fb);

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetVidPnSourceVisibility(_In_ const HANDLE hAdapter,
                                                           _In_ const DXGKARG_SETVIDPNSOURCEVISIBILITY* pVisibility)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pVisibility) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pVisibility->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    adapter->SourceVisible = pVisibility->Visible ? TRUE : FALSE;
    AeroGpuSetScanoutEnable(adapter, adapter->SourceVisible ? 1u : 0u);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiGetScanLine(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_GETSCANLINE* pGetScanLine)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pGetScanLine) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pGetScanLine->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONG height = adapter->CurrentHeight ? adapter->CurrentHeight : 1u;
    const ULONG vblankLines = AeroGpuComputeVblankLineCountForActiveHeight(height);

    const ULONG totalLines = height + vblankLines;

    const ULONGLONG now100ns = KeQueryInterruptTime();

    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);
    const BOOLEAN acceptingSubmissions =
        (InterlockedCompareExchange((volatile LONG*)&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
    const BOOLEAN mmioSafe = poweredOn && acceptingSubmissions;

    ULONGLONG periodNs =
        adapter->VblankPeriodNs ? (ULONGLONG)adapter->VblankPeriodNs : (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
    if (periodNs == 0) {
        periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
    }

    ULONGLONG posNs = 0;
    BOOLEAN usedCache = FALSE;

    /*
     * Fast path: use the cached vblank anchor maintained by the vblank IRQ ISR.
     *
     * This avoids multiple MMIO reads per call for D3D9-era apps that poll
     * GetRasterStatus at very high frequency.
     */
    {
        /*
         * Avoid touching MMIO on the fast path: D3D9-era apps may poll
         * GetRasterStatus thousands of times per second, so even a single MMIO
         * read here can be a measurable regression.
         *
         * Use the cached IRQ mask as our "interrupts enabled" gate instead. If
         * the device/driver lose interrupt state (for example across a reset),
         * the cached vblank anchor will go stale and we'll fall back to the
         * MMIO-based or synthetic cadence paths.
         */
        const ULONG irqEnableMask = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->IrqEnableMask);
        const BOOLEAN vblankIrqEnabled = (irqEnableMask & AEROGPU_IRQ_SCANOUT_VBLANK) != 0;

        const ULONGLONG lastVblank100ns = AeroGpuAtomicReadU64(&adapter->LastVblankInterruptTime100ns);
        const LONG vblankIrqCount = InterlockedCompareExchange(&adapter->IrqIsrVblankCount, 0, 0);

        if (poweredOn && adapter->SupportsVblank && vblankIrqEnabled && lastVblank100ns != 0 && vblankIrqCount != 0) {
            ULONGLONG delta100ns = (now100ns >= lastVblank100ns) ? (now100ns - lastVblank100ns) : 0;

            /*
             * Treat the cached anchor as stale if we haven't observed a vblank IRQ
             * for "too long". This avoids reporting scanline position based on a
             * frozen cadence when scanout/vblank is no longer ticking.
             *
             * Use a threshold based on the nominal vblank period, with a small
             * absolute minimum to tolerate jitter.
             */
            ULONGLONG period100ns = (periodNs + 99ull) / 100ull;
            ULONGLONG staleThreshold100ns = period100ns * 4ull;
            const ULONGLONG minThreshold100ns = 500000ull; /* 50ms */
            if (staleThreshold100ns < minThreshold100ns) {
                staleThreshold100ns = minThreshold100ns;
            }

            if (delta100ns <= staleThreshold100ns) {
                const ULONGLONG deltaNs = delta100ns * 100ull;
                posNs = (periodNs != 0) ? (deltaNs % periodNs) : 0;
                usedCache = TRUE;
            }
        }
    }

    BOOLEAN haveVblankRegs = FALSE;
    if (!usedCache) {
        if (mmioSafe && adapter->SupportsVblank &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG))) {
            const ULONG mmioPeriod = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
            if (mmioPeriod != 0) {
                adapter->VblankPeriodNs = mmioPeriod;
                periodNs = (ULONGLONG)mmioPeriod;
            } else if (periodNs == 0) {
                periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
            }
            haveVblankRegs = TRUE;
        } else if (periodNs == 0) {
            periodNs = (ULONGLONG)AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
        }
    }

    if (usedCache) {
#if DBG
        InterlockedIncrement64(&adapter->PerfGetScanLineCacheHits);
#endif
    } else if (haveVblankRegs) {
#if DBG
        InterlockedIncrement64(&adapter->PerfGetScanLineMmioPolls);
#endif
        ULONGLONG seq = AeroGpuReadRegU64HiLoHi(adapter,
                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
        ULONGLONG timeNs = AeroGpuReadRegU64HiLoHi(adapter,
                                                   AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                   AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
        {
            const ULONGLONG seq2 = AeroGpuReadRegU64HiLoHi(adapter,
                                                           AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                           AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
            if (seq2 != seq) {
                seq = seq2;
                timeNs = AeroGpuReadRegU64HiLoHi(adapter,
                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                 AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
            }
        }

        const ULONGLONG cachedSeq = AeroGpuAtomicReadU64(&adapter->LastVblankSeq);
        const ULONGLONG cachedTimeNs = AeroGpuAtomicReadU64(&adapter->LastVblankTimeNs);
        ULONGLONG lastVblank100ns = AeroGpuAtomicReadU64(&adapter->LastVblankInterruptTime100ns);
        if (seq != cachedSeq) {
            /*
             * Update our guest-time estimate of when the most recent vblank occurred.
             *
             * Prefer advancing by the device's monotonic VBLANK_TIME_NS delta (mapped to
             * 100ns units) to avoid phase drift if the nominal period changes.
             * Fall back to `deltaSeq * period` if timestamps are not usable.
             */
            ULONGLONG newLastVblank100ns = now100ns;

            if (lastVblank100ns != 0 && cachedSeq != 0) {
                ULONGLONG advance100ns = 0;

                if (cachedTimeNs != 0 && timeNs != 0 && timeNs >= cachedTimeNs) {
                    const ULONGLONG deltaDeviceNs = timeNs - cachedTimeNs;
                    advance100ns = deltaDeviceNs / 100ull;
                } else {
                    const ULONGLONG deltaSeq = seq - cachedSeq;
                    if (deltaSeq != 0) {
                        if (deltaSeq > (~0ull / periodNs)) {
                            advance100ns = ~0ull;
                        } else {
                            const ULONGLONG advanceNs = deltaSeq * periodNs;
                            advance100ns = advanceNs / 100ull;
                        }
                    }
                }

                ULONGLONG predicted = lastVblank100ns;
                if (advance100ns == ~0ull || predicted > (~0ull - advance100ns)) {
                    predicted = ~0ull;
                } else {
                    predicted += advance100ns;
                }

                if (predicted <= now100ns) {
                    newLastVblank100ns = predicted;
                }
            }

            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankTimeNs, timeNs);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, newLastVblank100ns);
            lastVblank100ns = newLastVblank100ns;
        }

        if (lastVblank100ns == 0) {
            /* First observation: anchor the cadence to "now". */
            AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
            AeroGpuAtomicWriteU64(&adapter->LastVblankTimeNs, timeNs);
            AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);
            lastVblank100ns = now100ns;
        }

        ULONGLONG delta100ns = (now100ns >= lastVblank100ns) ? (now100ns - lastVblank100ns) : 0;
        ULONGLONG deltaNs = delta100ns * 100ull;
        posNs = (periodNs != 0) ? (deltaNs % periodNs) : 0;
    } else {
        /*
         * Fallback path for devices without vblank timing registers: simulate a
         * fixed cadence from KeQueryInterruptTime(). This keeps D3D9-era apps
         * that poll raster status from busy-waiting forever.
         */
        const ULONGLONG nowNs = now100ns * 100ull;
        posNs = (periodNs != 0) ? (nowNs % periodNs) : 0;
    }

    ULONGLONG line = 0;
    if (periodNs != 0 && totalLines != 0) {
        ULONGLONG tline = (posNs * (ULONGLONG)totalLines) / periodNs;
        if (tline >= (ULONGLONG)totalLines) {
            tline = (ULONGLONG)totalLines - 1;
        }

        line = tline + (ULONGLONG)height;
        if (line >= (ULONGLONG)totalLines) {
            line -= (ULONGLONG)totalLines;
        }
    }

    pGetScanLine->InVerticalBlank = (line >= (ULONGLONG)height) ? TRUE : FALSE;
    pGetScanLine->ScanLine = (ULONG)line;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiUpdateActiveVidPnPresentPath(_In_ const HANDLE hAdapter,
                                                                 _Inout_ DXGKARG_UPDATEACTIVEVIDPNPRESENTPATH* pUpdate)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pUpdate) {
        return STATUS_INVALID_PARAMETER;
    }

    D3DKMDT_VIDPN_PRESENT_PATH* path = &pUpdate->VidPnPresentPathInfo;

    if (path->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID || path->VidPnTargetId != AEROGPU_VIDPN_TARGET_ID) {
        return STATUS_GRAPHICS_INVALID_VIDPN_TOPOLOGY;
    }

    const D3DKMDT_VIDPN_PRESENT_PATH_ROTATION rot = path->ContentTransformation.Rotation;
    if (rot != D3DKMDT_VPPR_IDENTITY && rot != D3DKMDT_VPPR_UNINITIALIZED) {
        return STATUS_NOT_SUPPORTED;
    }

    const D3DKMDT_VIDPN_PRESENT_PATH_SCALING sc = path->ContentTransformation.Scaling;
    if (sc != D3DKMDT_VPPS_IDENTITY && sc != D3DKMDT_VPPS_UNINITIALIZED) {
        return STATUS_NOT_SUPPORTED;
    }

    /*
     * Be explicit: treat uninitialized transforms as identity in the active path.
     * This helps dxgkrnl keep the committed VidPN stable across mode changes.
     */
    if (path->ContentTransformation.Rotation == D3DKMDT_VPPR_UNINITIALIZED) {
        path->ContentTransformation.Rotation = D3DKMDT_VPPR_IDENTITY;
    }
    if (path->ContentTransformation.Scaling == D3DKMDT_VPPS_UNINITIALIZED) {
        path->ContentTransformation.Scaling = D3DKMDT_VPPS_IDENTITY;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRecommendMonitorModes(_In_ const HANDLE hAdapter,
                                                         _Inout_ DXGKARG_RECOMMENDMONITORMODES* pRecommend)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pRecommend) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pRecommend->ChildUid != AEROGPU_CHILD_UID) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!pRecommend->hMonitorSourceModeSet || !pRecommend->pMonitorSourceModeSetInterface) {
        return STATUS_INVALID_PARAMETER;
    }

    const DXGK_MONITORSOURCEMODESET_INTERFACE* msi = pRecommend->pMonitorSourceModeSetInterface;
    if (!msi->pfnCreateNewModeInfo || !msi->pfnAddMode || !msi->pfnReleaseModeInfo) {
        return STATUS_NOT_SUPPORTED;
    }

    AEROGPU_DISPLAY_MODE modes[16];
    UINT modeCount = AeroGpuBuildModeList(modes, (UINT)(sizeof(modes) / sizeof(modes[0])));
    ULONG pinW = 0;
    ULONG pinH = 0;
    for (UINT i = 0; i < modeCount; ++i) {
        const ULONG w = modes[i].Width;
        const ULONG h = modes[i].Height;
        if (AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
            pinW = w;
            pinH = h;
            break;
        }
    }
    BOOLEAN pinned = FALSE;

    /* Avoid failing on duplicates if dxgkrnl already populated the set from EDID. */
    AEROGPU_DISPLAY_MODE existing[32];
    UINT existingCount = 0;
    RtlZeroMemory(existing, sizeof(existing));

    if (msi->pfnAcquireFirstModeInfo && msi->pfnAcquireNextModeInfo) {
        const D3DKMDT_MONITOR_SOURCE_MODE* cur = NULL;
        NTSTATUS st = msi->pfnAcquireFirstModeInfo(pRecommend->hMonitorSourceModeSet, &cur);
        while (NT_SUCCESS(st) && cur) {
            if (!pinned && msi->pfnPinMode && pinW != 0 && pinH != 0) {
                const ULONG cw = cur->VideoSignalInfo.ActiveSize.cx;
                const ULONG ch = cur->VideoSignalInfo.ActiveSize.cy;
                const D3DKMDT_VIDEO_SIGNAL_SCANLINE_ORDERING order = cur->VideoSignalInfo.ScanLineOrdering;
                if (AeroGpuModeWithinMax(cw, ch) &&
                    (order == D3DKMDT_VSSLO_PROGRESSIVE || order == D3DKMDT_VSSLO_UNINITIALIZED) &&
                    AeroGpuIsSupportedVidPnVSyncFrequency(cur->VideoSignalInfo.VSyncFreq.Numerator, cur->VideoSignalInfo.VSyncFreq.Denominator)) {
                    const ULONG diffW = (cw > pinW) ? (cw - pinW) : (pinW - cw);
                    const ULONG diffH = (ch > pinH) ? (ch - pinH) : (pinH - ch);
                    if (diffW <= 2u && diffH <= 2u) {
                        /* Pin the preferred mode if it already exists in the mode set (common when dxgkrnl parsed EDID). */
                        (void)msi->pfnPinMode(pRecommend->hMonitorSourceModeSet, (D3DKMDT_MONITOR_SOURCE_MODE*)cur);
                        pinned = TRUE;
                    }
                }
            }

            {
                const ULONG cw = cur->VideoSignalInfo.ActiveSize.cx;
                const ULONG ch = cur->VideoSignalInfo.ActiveSize.cy;
                const D3DKMDT_VIDEO_SIGNAL_SCANLINE_ORDERING order = cur->VideoSignalInfo.ScanLineOrdering;
                if (AeroGpuIsSupportedVidPnModeDimensions(cw, ch) &&
                    (order == D3DKMDT_VSSLO_PROGRESSIVE || order == D3DKMDT_VSSLO_UNINITIALIZED) &&
                    AeroGpuIsSupportedVidPnVSyncFrequency(cur->VideoSignalInfo.VSyncFreq.Numerator, cur->VideoSignalInfo.VSyncFreq.Denominator)) {
                    AeroGpuModeListAddUnique(existing, &existingCount, (UINT)(sizeof(existing) / sizeof(existing[0])), cw, ch);
                }
            }

            const D3DKMDT_MONITOR_SOURCE_MODE* next = NULL;
            st = msi->pfnAcquireNextModeInfo(pRecommend->hMonitorSourceModeSet, cur, &next);
            msi->pfnReleaseModeInfo(pRecommend->hMonitorSourceModeSet, cur);
            cur = next;
        }
    }

    for (UINT i = 0; i < modeCount; ++i) {
        const ULONG w = modes[i].Width;
        const ULONG h = modes[i].Height;
        if (!AeroGpuIsSupportedVidPnModeDimensions(w, h)) {
            continue;
        }

        if (AeroGpuModeListContainsApprox(existing, existingCount, w, h, 2u)) {
            continue;
        }

        D3DKMDT_MONITOR_SOURCE_MODE* modeInfo = NULL;
        NTSTATUS st2 = msi->pfnCreateNewModeInfo(pRecommend->hMonitorSourceModeSet, &modeInfo);
        if (!NT_SUCCESS(st2) || !modeInfo) {
            return NT_SUCCESS(st2) ? STATUS_INSUFFICIENT_RESOURCES : st2;
        }

        RtlZeroMemory(modeInfo, sizeof(*modeInfo));
        modeInfo->VideoSignalInfo.VideoStandard = D3DKMDT_VSS_OTHER;
        modeInfo->VideoSignalInfo.ActiveSize.cx = w;
        modeInfo->VideoSignalInfo.ActiveSize.cy = h;
        modeInfo->VideoSignalInfo.TotalSize.cx = AeroGpuComputeTotalWidthForActiveWidth(w);
        modeInfo->VideoSignalInfo.TotalSize.cy = h + AeroGpuComputeVblankLineCountForActiveHeight(h);
        modeInfo->VideoSignalInfo.VSyncFreq.Numerator = 60;
        modeInfo->VideoSignalInfo.VSyncFreq.Denominator = 1;
        modeInfo->VideoSignalInfo.HSyncFreq.Numerator = 60 * modeInfo->VideoSignalInfo.TotalSize.cy;
        modeInfo->VideoSignalInfo.HSyncFreq.Denominator = 1;
        {
            ULONGLONG pixelRate =
                (ULONGLONG)60ull * (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cx * (ULONGLONG)modeInfo->VideoSignalInfo.TotalSize.cy;
            if (pixelRate > (ULONGLONG)0xFFFFFFFFu) {
                pixelRate = 0;
            }
            modeInfo->VideoSignalInfo.PixelRate = (ULONG)pixelRate;
        }
        modeInfo->VideoSignalInfo.ScanLineOrdering = D3DKMDT_VSSLO_PROGRESSIVE;

        st2 = msi->pfnAddMode(pRecommend->hMonitorSourceModeSet, modeInfo);
        if (NT_SUCCESS(st2) && !pinned && msi->pfnPinMode && w == pinW && h == pinH) {
            (void)msi->pfnPinMode(pRecommend->hMonitorSourceModeSet, modeInfo);
            pinned = TRUE;
        }
        msi->pfnReleaseModeInfo(pRecommend->hMonitorSourceModeSet, modeInfo);

        if (!NT_SUCCESS(st2)) {
            /* Treat duplicates/ordering issues as non-fatal. */
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiGetStandardAllocationDriverData(_In_ const HANDLE hAdapter,
                                                                   _Inout_ DXGKARG_GETSTANDARDALLOCATIONDRIVERDATA* pData)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pData || !pData->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_ALLOCATIONINFO* info = pData->pAllocationInfo;
    RtlZeroMemory(info, sizeof(*info));

    switch (pData->StandardAllocationType) {
    case StandardAllocationTypePrimary: {
        ULONG pitch = adapter->CurrentPitch;
        ULONG height = adapter->CurrentHeight;
        if (pitch == 0 && AeroGpuComputeDefaultPitchBytes(adapter->CurrentWidth, &pitch)) {
            /* Keep the cached state internally consistent for callers that query before a modeset. */
            adapter->CurrentPitch = pitch;
        }
        if (height == 0) {
            height = 1;
        }

        const ULONGLONG size64 = (ULONGLONG)pitch * (ULONGLONG)height;
        const ULONGLONG maxSize = (ULONGLONG)(SIZE_T)~(SIZE_T)0;
        if (size64 == 0 || size64 > maxSize) {
            return STATUS_INTEGER_OVERFLOW;
        }

        info->Size = (SIZE_T)size64;
        info->Alignment = 0;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.Value = 0;
        info->Flags.Primary = 1;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        return STATUS_SUCCESS;
    }
    default:
        return STATUS_NOT_SUPPORTED;
    }
}

static NTSTATUS APIENTRY AeroGpuDdiCreateAllocation(_In_ const HANDLE hAdapter,
                                                    _Inout_ DXGKARG_CREATEALLOCATION* pCreate)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCreate || !pCreate->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * WDDM allocation lifetime model used by this driver:
     * - Both DxgkDdiCreateAllocation and DxgkDdiOpenAllocation allocate an
     *   AEROGPU_ALLOCATION wrapper per returned hAllocation.
     * - Windows 7 may release those handles via either DxgkDdiCloseAllocation
     *   or DxgkDdiDestroyAllocation depending on the object and sharing model.
     *
     * To avoid double-free/use-after-free across different Win7 call patterns,
     * the driver tracks all live wrappers in adapter->Allocations and only frees
     * handles that are still tracked.
     */
    /*
     * On Windows 7/WDDM 1.1, DXGKARG_CREATEALLOCATION::Flags.CreateShared is used for shared
     * handle creation (notably DWM redirected surfaces).
     */
    const BOOLEAN isShared = pCreate->Flags.CreateShared ? TRUE : FALSE;
    const ULONG callSeq = (ULONG)InterlockedIncrement(&adapter->CreateAllocationCallSeq);

#if DBG
    BOOLEAN logCall = FALSE;
    /*
     * WDDM resources may be represented as multiple allocations (mips/arrays/planes).
     *
     * AeroGPU's MVP shared-surface interop assumes a single backing allocation, so
     * we log shared/multi-allocation creation requests to characterize real-world
     * behavior (notably DWM redirected surfaces) and to aid bring-up debugging.
     *
     * Guard + rate-limit to avoid excessive DbgPrint spam in hot paths.
      */
    {
        const BOOLEAN interesting = (AEROGPU_KMD_TRACE_CREATEALLOCATION != 0) || isShared || (pCreate->NumAllocations != 1);
        if (interesting) {
            enum { kLogLimit = 64 };
            static LONG s_logCount = 0;
            const LONG n = InterlockedIncrement(&s_logCount);
            if (n <= kLogLimit) {
                logCall = TRUE;
                AEROGPU_LOG("CreateAllocation: NumAllocations=%u CreateShared=%u Flags=0x%08X",
                            (unsigned)pCreate->NumAllocations,
                            (unsigned)isShared,
                            (unsigned)pCreate->Flags.Value);

                for (UINT i = 0; i < pCreate->NumAllocations; ++i) {
                    const DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];
                    AEROGPU_LOG("  alloc[%u]: Size=%Iu Alignment=%Iu Flags=0x%08X PrivSize=%u Priv=%p",
                                (unsigned)i,
                                info->Size,
                                info->Alignment,
                                (unsigned)info->Flags.Value,
                                (unsigned)info->PrivateDriverDataSize,
                                info->pPrivateDriverData);
                    if (info->pPrivateDriverData &&
                        info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_private_data)) {
                        const aerogpu_wddm_alloc_private_data* priv =
                            (const aerogpu_wddm_alloc_private_data*)info->pPrivateDriverData;
                        AEROGPU_LOG("    priv: magic=0x%08lx ver=%lu flags=0x%08lx alloc_id=%lu share_token=0x%I64x size_bytes=%I64u",
                                    (ULONG)priv->magic,
                                    (ULONG)priv->version,
                                    (ULONG)priv->flags,
                                    (ULONG)priv->alloc_id,
                                    (ULONGLONG)priv->share_token,
                                    (ULONGLONG)priv->size_bytes);
                    }
                }
            } else if (n == (kLogLimit + 1)) {
                AEROGPU_LOG0("CreateAllocation: log limit reached; suppressing further messages");
            }
        }
    }
#endif

    /*
     * MVP restriction: shared resources must be represented as a single allocation.
     *
     * The guest↔host shared-surface protocol currently only supports one backing
     * allocation per share token. Enforce this invariant in KMD to ensure we fail
     * predictably (rather than corrupting host-side shared-surface tables) if an
     * API attempts to share a resource that would require multiple allocations.
     */
    if (isShared && pCreate->NumAllocations != 1) {
#if DBG
        AEROGPU_LOG("CreateAllocation: rejecting shared resource with NumAllocations=%u (MVP supports only single-allocation shared surfaces)",
                    (unsigned)pCreate->NumAllocations);
#endif
        return STATUS_NOT_SUPPORTED;
    }

    NTSTATUS status = STATUS_SUCCESS;
    UINT i = 0;
    for (i = 0; i < pCreate->NumAllocations; ++i) {
        DXGK_ALLOCATIONINFO* info = &pCreate->pAllocationInfo[i];
        info->hAllocation = NULL;
        const ULONG preFlags = info->Flags.Value;

        ULONG allocId = 0;
        ULONGLONG shareToken = 0;
        ULONG privFlags = 0;
        ULONG kind = 0;
        ULONG width = 0;
        ULONG height = 0;
        ULONG format = 0;
        ULONG rowPitchBytes = 0;
        ULONG pitchBytes = 0;
        aerogpu_wddm_u64 reserved0 = 0;
        ULONG privVersion = 0;

        if (info->pPrivateDriverData && info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_private_data)) {
            status = STATUS_BUFFER_TOO_SMALL;
            goto Rollback;
        }
        if (isShared && (!info->pPrivateDriverData || info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_private_data))) {
            status = STATUS_BUFFER_TOO_SMALL;
            goto Rollback;
        }

        /*
         * WDDM allocation private driver data (if provided).
         *
         * The UMD provides a per-allocation private-data buffer; the AeroGPU KMD
         * writes stable IDs (notably `share_token`) into it so dxgkrnl can
         * preserve the blob for cross-process `OpenResource`.
         *
         * For standard allocations created by dxgkrnl (for example primary
         * surfaces), the runtime may not provide an AeroGPU private-data blob; in
         * that case we synthesize an internal alloc_id from a reserved namespace.
         */
        if (info->pPrivateDriverData && info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_private_data)) {
            const aerogpu_wddm_alloc_private_data* priv =
                (const aerogpu_wddm_alloc_private_data*)info->pPrivateDriverData;

            if (priv->magic == AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC) {
                privVersion = (ULONG)priv->version;
                reserved0 = priv->reserved0;
                if (priv->version != AEROGPU_WDDM_ALLOC_PRIV_VERSION &&
                    priv->version != AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
                    status = STATUS_INVALID_PARAMETER;
                    goto Rollback;
                }
                if (priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 &&
                    info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_priv_v2)) {
                    status = STATUS_INVALID_PARAMETER;
                    goto Rollback;
                }
                if (priv->alloc_id == 0 || priv->alloc_id > AEROGPU_WDDM_ALLOC_ID_UMD_MAX) {
                    status = STATUS_INVALID_PARAMETER;
                    goto Rollback;
                }

                privFlags = (ULONG)priv->flags;
                const BOOLEAN privShared = (privFlags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED) ? TRUE : FALSE;
                if (privShared != isShared) {
                    status = STATUS_INVALID_PARAMETER;
                    goto Rollback;
                }
                if (!privShared && priv->share_token != 0) {
                    status = STATUS_INVALID_PARAMETER;
                    goto Rollback;
                }

                allocId = (ULONG)priv->alloc_id;
                privFlags = (ULONG)priv->flags;

                /*
                 * Optional surface metadata.
                 *
                 * reserved0 is a shared UMD/KMD extension field used by multiple
                 * stacks (e.g. D3D9 shared-surface descriptors). Only interpret
                 * it as a pitch encoding when the descriptor marker is not set.
                 */
                pitchBytes = 0;
                if (!AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(reserved0)) {
                    pitchBytes = (ULONG)(reserved0 & 0xFFFFFFFFu);
                    if (pitchBytes != 0 && (aerogpu_wddm_u64)pitchBytes > (aerogpu_wddm_u64)info->Size) {
                        status = STATUS_INVALID_PARAMETER;
                        goto Rollback;
                    }
                }
                if (priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
                    const aerogpu_wddm_alloc_priv_v2* priv2 = (const aerogpu_wddm_alloc_priv_v2*)info->pPrivateDriverData;
                    kind = (ULONG)priv2->kind;
                    width = (ULONG)priv2->width;
                    height = (ULONG)priv2->height;
                    format = (ULONG)priv2->format;
                    rowPitchBytes = (ULONG)priv2->row_pitch_bytes;
                }

                /*
                 * For v2 blobs, prefer the explicit row pitch for surface locks.
                 *
                 * The v2 private-data blob carries `row_pitch_bytes` as the
                 * canonical packed layout row pitch chosen by the UMD (and
                 * consumed by the host-side executor). Use it whenever present
                 * so DxgkDdiLock returns a pitch consistent with the UMD layout,
                 * even if `reserved0` is repurposed by other extensions.
                 */
                if (rowPitchBytes != 0) {
                    if ((aerogpu_wddm_u64)rowPitchBytes > (aerogpu_wddm_u64)info->Size) {
                        status = STATUS_INVALID_PARAMETER;
                        goto Rollback;
                    }
                    pitchBytes = rowPitchBytes;
                }
            }
        }

        if (allocId == 0) {
            if (isShared) {
                /* Shared allocations must carry AeroGPU private data so the UMD can recover stable IDs on OpenResource. */
                status = STATUS_INVALID_PARAMETER;
                goto Rollback;
            }

            allocId = (ULONG)InterlockedIncrement(&adapter->NextKmdAllocId);
            if (allocId < AEROGPU_WDDM_ALLOC_ID_KMD_MIN) {
                AEROGPU_LOG("CreateAllocation: allocation id overflow (wrapped into UMD range), failing with 0x%08lx",
                            STATUS_INTEGER_OVERFLOW);
                status = STATUS_INTEGER_OVERFLOW;
                goto Rollback;
            }
            shareToken = 0;
        }

        if (isShared) {
            shareToken = AeroGpuGenerateShareToken(adapter);
        } else {
            shareToken = 0;
        }

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            goto Rollback;
        }

        RtlZeroMemory(alloc, sizeof(*alloc));
        alloc->AllocationId = allocId;
        alloc->ShareToken = shareToken;
        alloc->SizeBytes = info->Size;
        alloc->Flags = privFlags;
        alloc->Kind = kind;
        alloc->Width = width;
        alloc->Height = height;
        alloc->Format = format;
        alloc->RowPitchBytes = rowPitchBytes;
        if (info->Flags.Primary) {
            alloc->Flags |= AEROGPU_KMD_ALLOC_FLAG_PRIMARY;
        }
        alloc->LastKnownPa.QuadPart = 0;
        alloc->PitchBytes = pitchBytes;
        ExInitializeFastMutex(&alloc->CpuMapMutex);
        alloc->CpuMapRefCount = 0;
        alloc->CpuMapUserVa = NULL;
        alloc->CpuMapKernelVa = NULL;
        alloc->CpuMapMdl = NULL;
        alloc->CpuMapSize = 0;
        alloc->CpuMapPageOffset = 0;
        alloc->CpuMapWritePending = FALSE;

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;

        if (privVersion != 0 && info->pPrivateDriverData && info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_private_data)) {
            if (privVersion == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 &&
                info->PrivateDriverDataSize >= sizeof(aerogpu_wddm_alloc_priv_v2)) {
                aerogpu_wddm_alloc_priv_v2* outPriv2 = (aerogpu_wddm_alloc_priv_v2*)info->pPrivateDriverData;
                outPriv2->magic = AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC;
                outPriv2->version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
                outPriv2->alloc_id = (aerogpu_wddm_u32)allocId;
                outPriv2->flags = (aerogpu_wddm_u32)privFlags;
                outPriv2->share_token = (aerogpu_wddm_u64)shareToken;
                outPriv2->size_bytes = (aerogpu_wddm_u64)info->Size;
                outPriv2->reserved0 = reserved0;
                outPriv2->reserved1 = 0;
            } else {
                aerogpu_wddm_alloc_private_data outPriv;
                outPriv.magic = AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC;
                outPriv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
                outPriv.alloc_id = (aerogpu_wddm_u32)allocId;
                outPriv.flags = (aerogpu_wddm_u32)privFlags;
                outPriv.share_token = (aerogpu_wddm_u64)shareToken;
                outPriv.size_bytes = (aerogpu_wddm_u64)info->Size;
                outPriv.reserved0 = reserved0;
                RtlCopyMemory(info->pPrivateDriverData, &outPriv, sizeof(outPriv));
            }
        }

        if (!AeroGpuTrackAllocation(adapter, alloc)) {
            /*
             * For shared allocations, share-token ref tracking is required for correct
             * host-side lifetime management (final close -> RELEASE_SHARED_SURFACE).
             * If we cannot allocate/track the token, fail CreateAllocation rather than
             * leaking the host-side mapping.
             */
            ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
            info->hAllocation = NULL;
            status = STATUS_INSUFFICIENT_RESOURCES;
            goto Rollback;
        }

        AeroGpuTraceCreateAllocation(adapter,
                                     callSeq,
                                     (ULONG)i,
                                     (ULONG)pCreate->NumAllocations,
                                     (ULONG)pCreate->Flags.Value,
                                     allocId,
                                     shareToken,
                                     (ULONGLONG)info->Size,
                                     preFlags,
                                     (ULONG)info->Flags.Value,
                                     privFlags,
                                     pitchBytes);

#if DBG
        if (logCall) {
            AEROGPU_LOG("CreateAllocation: alloc_id=%lu shared=%lu share_token=0x%I64x size=%Iu flags=0x%08X->0x%08X",
                        alloc->AllocationId,
                        isShared ? 1ul : 0ul,
                        alloc->ShareToken,
                        alloc->SizeBytes,
                        (unsigned)preFlags,
                        (unsigned)info->Flags.Value);
        }
#endif
    }

    return STATUS_SUCCESS;

Rollback:
    /*
     * If CreateAllocation fails after creating one or more allocation handles,
     * WDDM expects the driver to clean up those partial results.
     */
    for (UINT j = 0; j < i; ++j) {
        HANDLE hAllocation = pCreate->pAllocationInfo[j].hAllocation;
        if (hAllocation) {
            AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
            pCreate->pAllocationInfo[j].hAllocation = NULL;
        }
    }
    return status;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyAllocation(_In_ const HANDLE hAdapter,
                                                     _In_ const DXGKARG_DESTROYALLOCATION* pDestroy)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!pDestroy) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pDestroy->NumAllocations; ++i) {
        HANDLE hAllocation = pDestroy->pAllocationList[i].hAllocation;
        AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDescribeAllocation(_In_ const HANDLE hAdapter,
                                                     _Inout_ DXGKARG_DESCRIBEALLOCATION* pDescribe)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pDescribe || !pDescribe->pAllocationInfo) {
        return STATUS_INVALID_PARAMETER;
    }

    DXGK_ALLOCATIONINFO* info = pDescribe->pAllocationInfo;
    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)pDescribe->hAllocation;

    RtlZeroMemory(info, sizeof(*info));
    info->Size = alloc ? alloc->SizeBytes : 0;
    info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
    info->Flags.CpuVisible = 1;
    info->Flags.Aperture = 1;
    info->SupportedReadSegmentSet = 1;
    info->SupportedWriteSegmentSet = 1;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiOpenAllocation(_In_ const HANDLE hAdapter,
                                                 _Inout_ DXGKARG_OPENALLOCATION* pOpen)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pOpen || !pOpen->pOpenAllocation) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * MVP restriction: shared resources must be single-allocation.
     *
     * Even though the create path rejects multi-allocation shared resources, be
     * defensive here as well: older guests (or future driver changes) may try to
     * open a shared resource that spans multiple allocations (mips/planes/etc).
     * The current shared-surface protocol associates one share token with a
     * single backing allocation, so fail deterministically instead of creating a
     * partially-represented resource.
     */
    if (pOpen->NumAllocations != 1) {
#if DBG
        AEROGPU_LOG("OpenAllocation: rejecting shared resource with NumAllocations=%u (MVP supports only single-allocation shared surfaces)",
                    (unsigned)pOpen->NumAllocations);
#endif
        return STATUS_NOT_SUPPORTED;
    }

    NTSTATUS st = STATUS_SUCCESS;

    for (UINT i = 0; i < pOpen->NumAllocations; ++i) {
        DXGK_OPENALLOCATIONINFO* info = &pOpen->pOpenAllocation[i];
        /*
         * Defensive init: treat hAllocation as an output-only field and clear it
         * before validation so the cleanup path never attempts to free an
         * uninitialized value (or an unrelated handle passed in by dxgkrnl).
         */
        info->hAllocation = NULL;

        if (!info->pPrivateDriverData || info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_private_data)) {
            AEROGPU_LOG("OpenAllocation: missing/too small private data (have=%lu need=%Iu)",
                       (ULONG)info->PrivateDriverDataSize,
                       sizeof(aerogpu_wddm_alloc_private_data));
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        const aerogpu_wddm_alloc_private_data* priv = (const aerogpu_wddm_alloc_private_data*)info->pPrivateDriverData;
        if (priv->magic != AEROGPU_WDDM_ALLOC_PRIVATE_DATA_MAGIC ||
            (priv->version != AEROGPU_WDDM_ALLOC_PRIV_VERSION && priv->version != AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) ||
            priv->alloc_id == 0 || priv->alloc_id > AEROGPU_WDDM_ALLOC_ID_UMD_MAX) {
            AEROGPU_LOG("OpenAllocation: invalid private data (magic=0x%08lx version=%lu alloc_id=%lu)",
                       (ULONG)priv->magic,
                       (ULONG)priv->version,
                       (ULONG)priv->alloc_id);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }
        if (priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2 &&
            info->PrivateDriverDataSize < sizeof(aerogpu_wddm_alloc_priv_v2)) {
            AEROGPU_LOG("OpenAllocation: private data too small for v2 (have=%lu need=%Iu)",
                        (ULONG)info->PrivateDriverDataSize,
                        sizeof(aerogpu_wddm_alloc_priv_v2));
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        if ((priv->flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_SHARED) == 0 || priv->share_token == 0) {
            AEROGPU_LOG("OpenAllocation: expected shared private data (alloc_id=%lu flags=0x%08lx share_token=0x%I64x)",
                       (ULONG)priv->alloc_id,
                       (ULONG)priv->flags,
                       (ULONGLONG)priv->share_token);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        if (priv->size_bytes == 0 || priv->size_bytes > (aerogpu_wddm_u64)(SIZE_T)(~(SIZE_T)0)) {
            AEROGPU_LOG("OpenAllocation: invalid size_bytes (alloc_id=%lu size_bytes=%I64u)",
                       (ULONG)priv->alloc_id,
                       (ULONGLONG)priv->size_bytes);
            st = STATUS_INVALID_PARAMETER;
            goto Cleanup;
        }

        ULONG pitchBytes = 0;
        ULONG kind = 0;
        ULONG width = 0;
        ULONG height = 0;
        ULONG format = 0;
        ULONG rowPitchBytes = 0;
        if (!AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(priv->reserved0)) {
            pitchBytes = (ULONG)(priv->reserved0 & 0xFFFFFFFFu);
            if (pitchBytes != 0 && (aerogpu_wddm_u64)pitchBytes > priv->size_bytes) {
                AEROGPU_LOG("OpenAllocation: invalid pitch_bytes in private data (alloc_id=%lu pitch=%lu size=%I64u)",
                           (ULONG)priv->alloc_id,
                           pitchBytes,
                           (ULONGLONG)priv->size_bytes);
                st = STATUS_INVALID_PARAMETER;
                goto Cleanup;
            }
        }
        if (priv->version == AEROGPU_WDDM_ALLOC_PRIV_VERSION_2) {
            const aerogpu_wddm_alloc_priv_v2* priv2 = (const aerogpu_wddm_alloc_priv_v2*)info->pPrivateDriverData;
            kind = (ULONG)priv2->kind;
            width = (ULONG)priv2->width;
            height = (ULONG)priv2->height;
            format = (ULONG)priv2->format;
            rowPitchBytes = (ULONG)priv2->row_pitch_bytes;
        }

        /*
         * Prefer explicit v2 `row_pitch_bytes` when available.
         *
         * `reserved0` may carry a D3D9 shared-surface descriptor encoding (bit63
         * marker) or legacy pitch metadata; the v2 row pitch is the canonical
         * packed layout pitch used by the UMD + host.
         */
        if (rowPitchBytes != 0) {
            pitchBytes = rowPitchBytes;
            if ((aerogpu_wddm_u64)pitchBytes > priv->size_bytes) {
                AEROGPU_LOG("OpenAllocation: invalid row_pitch_bytes in private data (alloc_id=%lu pitch=%lu size=%I64u)",
                            (ULONG)priv->alloc_id,
                            pitchBytes,
                            (ULONGLONG)priv->size_bytes);
                st = STATUS_INVALID_PARAMETER;
                goto Cleanup;
            }
        }

        AEROGPU_ALLOCATION* alloc =
            (AEROGPU_ALLOCATION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*alloc), AEROGPU_POOL_TAG);
        if (!alloc) {
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Cleanup;
        }

        RtlZeroMemory(alloc, sizeof(*alloc));
        alloc->AllocationId = (ULONG)priv->alloc_id;
        alloc->ShareToken = (ULONGLONG)priv->share_token;
        alloc->SizeBytes = (SIZE_T)priv->size_bytes;
        alloc->Flags = ((ULONG)priv->flags) | AEROGPU_KMD_ALLOC_FLAG_OPENED;
        alloc->Kind = kind;
        alloc->Width = width;
        alloc->Height = height;
        alloc->Format = format;
        alloc->RowPitchBytes = rowPitchBytes;
        alloc->LastKnownPa.QuadPart = 0;
        alloc->PitchBytes = pitchBytes;
        ExInitializeFastMutex(&alloc->CpuMapMutex);
        alloc->CpuMapRefCount = 0;
        alloc->CpuMapUserVa = NULL;
        alloc->CpuMapKernelVa = NULL;
        alloc->CpuMapMdl = NULL;
        alloc->CpuMapSize = 0;
        alloc->CpuMapPageOffset = 0;
        alloc->CpuMapWritePending = FALSE;

        info->hAllocation = (HANDLE)alloc;
        info->SegmentId = AEROGPU_SEGMENT_ID_SYSTEM;
        info->Flags.CpuVisible = 1;
        info->Flags.Aperture = 1;
        info->SupportedReadSegmentSet = 1;
        info->SupportedWriteSegmentSet = 1;

        if (!AeroGpuTrackAllocation(adapter, alloc)) {
            /*
             * Shared allocations must be tracked so the KMD can emit
             * RELEASE_SHARED_SURFACE on final close. If we cannot track the token,
             * fail OpenAllocation deterministically instead of leaking the host-side
             * mapping.
             */
            ExFreePoolWithTag(alloc, AEROGPU_POOL_TAG);
            info->hAllocation = NULL;
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Cleanup;
        }

        AEROGPU_LOG("OpenAllocation: alloc_id=%lu share_token=0x%I64x size=%Iu",
                   alloc->AllocationId,
                   alloc->ShareToken,
                   alloc->SizeBytes);
    }

    return STATUS_SUCCESS;

Cleanup:
    for (UINT j = 0; j < pOpen->NumAllocations; ++j) {
        HANDLE hAllocation = pOpen->pOpenAllocation[j].hAllocation;
        if (hAllocation) {
            AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
            pOpen->pOpenAllocation[j].hAllocation = NULL;
        }
    }
    return st;
}

static NTSTATUS APIENTRY AeroGpuDdiCloseAllocation(_In_ const HANDLE hAdapter,
                                                   _In_ const DXGKARG_CLOSEALLOCATION* pClose)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!pClose) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    for (UINT i = 0; i < pClose->NumAllocations; ++i) {
        HANDLE hAllocation = pClose->pAllocationList[i].hAllocation;
        AeroGpuUntrackAndFreeAllocation(adapter, hAllocation);
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiLock(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_LOCK* pLock)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pLock) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)pLock->hAllocation;
    if (!alloc) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pLock->SegmentId != AEROGPU_SEGMENT_ID_SYSTEM) {
        return STATUS_NOT_SUPPORTED;
    }

    SIZE_T offset = (SIZE_T)pLock->Offset;
    SIZE_T size = (SIZE_T)pLock->Size;
    if (offset > alloc->SizeBytes) {
        return STATUS_INVALID_PARAMETER;
    }
    if (size == 0) {
        size = alloc->SizeBytes - offset;
    }
    if (size > (alloc->SizeBytes - offset)) {
        return STATUS_INVALID_PARAMETER;
    }

    const BOOLEAN doNotWait = pLock->Flags.DoNotWait ? TRUE : FALSE;
    NTSTATUS waitSt = AeroGpuWaitForAllocationIdle(adapter, alloc, doNotWait);
    if (!NT_SUCCESS(waitSt)) {
        return waitSt;
    }

    ExAcquireFastMutex(&alloc->CpuMapMutex);

    NTSTATUS st = STATUS_SUCCESS;
    if (alloc->CpuMapRefCount <= 0) {
        ULONGLONG physBase = pLock->PhysicalAddress.QuadPart;
        if (physBase == 0) {
            physBase = (ULONGLONG)alloc->LastKnownPa.QuadPart;
        }
        if (physBase == 0) {
            st = STATUS_DEVICE_NOT_READY;
            goto Exit;
        }
        alloc->LastKnownPa.QuadPart = physBase;

        const SIZE_T pageOffset = (SIZE_T)(physBase & (PAGE_SIZE - 1));

        PHYSICAL_ADDRESS physAligned;
        physAligned.QuadPart = physBase & ~(ULONGLONG)(PAGE_SIZE - 1);

        SIZE_T mapSize = alloc->SizeBytes + pageOffset;
        mapSize = (mapSize + (PAGE_SIZE - 1)) & ~(SIZE_T)(PAGE_SIZE - 1);

        if (mapSize == 0 || mapSize > MAXULONG) {
            st = STATUS_INVALID_BUFFER_SIZE;
            goto Exit;
        }

        PVOID kva = MmMapIoSpace(physAligned, mapSize, MmCached);
        if (!kva) {
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Exit;
        }

        PMDL mdl = IoAllocateMdl(kva, (ULONG)mapSize, FALSE, FALSE, NULL);
        if (!mdl) {
            MmUnmapIoSpace(kva, mapSize);
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Exit;
        }

        MmBuildMdlForNonPagedPool(mdl);

        PVOID uva = MmMapLockedPagesSpecifyCache(mdl, UserMode, MmCached, NULL, FALSE, NormalPagePriority);
        if (!uva) {
            IoFreeMdl(mdl);
            MmUnmapIoSpace(kva, mapSize);
            st = STATUS_INSUFFICIENT_RESOURCES;
            goto Exit;
        }

        alloc->CpuMapUserVa = uva;
        alloc->CpuMapKernelVa = kva;
        alloc->CpuMapMdl = mdl;
        alloc->CpuMapSize = mapSize;
        alloc->CpuMapPageOffset = pageOffset;
        alloc->CpuMapRefCount = 1;
        alloc->CpuMapWritePending = FALSE;
    } else {
        alloc->CpuMapRefCount++;
    }

    if (!alloc->CpuMapUserVa) {
        st = STATUS_INVALID_DEVICE_STATE;
        goto Exit;
    }

    const BOOLEAN cpuWillRead = pLock->Flags.WriteOnly ? FALSE : TRUE;
    const BOOLEAN cpuWillWrite = pLock->Flags.ReadOnly ? FALSE : TRUE;

    if (cpuWillRead && alloc->CpuMapMdl) {
        /* Invalidate for device -> CPU reads (staging readback). */
        KeFlushIoBuffers(alloc->CpuMapMdl, /*ReadOperation*/ TRUE, /*DmaOperation*/ TRUE);
    }

    if (cpuWillWrite) {
        alloc->CpuMapWritePending = TRUE;
    }

    pLock->pData = (PUCHAR)alloc->CpuMapUserVa + alloc->CpuMapPageOffset + offset;

    /*
     * Pitch metadata (optional).
     *
     * On Win7, the runtime's D3DKMTLock path can return row/slice pitch for
     * surface allocations. dxgkrnl may pre-populate Pitch/SlicePitch, but for
     * AeroGPU system-memory allocations the pitch is defined by the allocation's
     * private metadata (PitchBytes) or the current scanout pitch (for primaries).
     *
     * Prefer the driver-defined pitch whenever available so user-mode observes a
     * consistent linear layout (the AeroGPU UMD and host-side executor both rely
     * on this for packed Texture2D uploads).
     */
    ULONG desiredPitch = alloc->PitchBytes;
    if (desiredPitch == 0 && alloc->Kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D && alloc->RowPitchBytes != 0) {
        desiredPitch = alloc->RowPitchBytes;
    }
    if (desiredPitch == 0 && (alloc->Flags & AEROGPU_KMD_ALLOC_FLAG_PRIMARY) && adapter->CurrentPitch != 0) {
        desiredPitch = adapter->CurrentPitch;
    }
    if (desiredPitch != 0) {
#if DBG
        static LONG g_PitchOverrideLogs = 0;
        if (pLock->Pitch != 0 && pLock->Pitch != desiredPitch) {
            AEROGPU_LOG_RATELIMITED(g_PitchOverrideLogs,
                                    8,
                                    "Lock: overriding dxgkrnl Pitch=%lu with driver pitch=%lu (alloc_id=%lu)",
                                    (ULONG)pLock->Pitch,
                                    desiredPitch,
                                    alloc->AllocationId);
        }
#endif
        pLock->Pitch = desiredPitch;
    }

    /* For primary surfaces, also provide a consistent SlicePitch derived from the final Pitch. */
    if ((alloc->Flags & AEROGPU_KMD_ALLOC_FLAG_PRIMARY) && pLock->Pitch != 0 && adapter->CurrentHeight != 0) {
        ULONGLONG slice = (ULONGLONG)pLock->Pitch * (ULONGLONG)adapter->CurrentHeight;
        if (slice > (ULONGLONG)MAXULONG) {
            slice = (ULONGLONG)MAXULONG;
        }
        if (pLock->SlicePitch == 0 || pLock->SlicePitch != (ULONG)slice) {
            pLock->SlicePitch = (ULONG)slice;
        }
    } else if (alloc->Kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D &&
               pLock->Pitch != 0 &&
               alloc->Height != 0) {
        /*
         * For non-primary Texture2D allocations, expose SlicePitch for the mip0
         * layout (pitch * rows_in_layout).
         *
         * Keep SlicePitch consistent with Pitch when we override Pitch above; this
         * avoids user-mode observing mismatched Pitch/SlicePitch pairs.
         */
        ULONG rows = alloc->Height;
        if (AeroGpuDxgiFormatIsBlockCompressed(alloc->Format)) {
            rows = (alloc->Height + 3u) / 4u;
        }
        if (rows != 0) {
            ULONGLONG slice = (ULONGLONG)pLock->Pitch * (ULONGLONG)rows;
            if (slice > (ULONGLONG)alloc->SizeBytes) {
                slice = (ULONGLONG)alloc->SizeBytes;
            }
            if (slice > (ULONGLONG)MAXULONG) {
                slice = (ULONGLONG)MAXULONG;
            }
            if (pLock->SlicePitch == 0 || pLock->SlicePitch != (ULONG)slice) {
                pLock->SlicePitch = (ULONG)slice;
            }
        }
    }

Exit:
    if (!NT_SUCCESS(st)) {
        if (alloc->CpuMapRefCount <= 0) {
            AeroGpuAllocationUnmapCpu(alloc);
        }
    }

    ExReleaseFastMutex(&alloc->CpuMapMutex);
    return st;
}

static NTSTATUS APIENTRY AeroGpuDdiUnlock(_In_ const HANDLE hAdapter, _In_ const DXGKARG_UNLOCK* pUnlock)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pUnlock) {
        return STATUS_INVALID_PARAMETER;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return STATUS_INVALID_DEVICE_STATE;
    }

    AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)pUnlock->hAllocation;
    if (!alloc) {
        return STATUS_INVALID_PARAMETER;
    }

    ExAcquireFastMutex(&alloc->CpuMapMutex);

    if (alloc->CpuMapRefCount <= 0) {
        ExReleaseFastMutex(&alloc->CpuMapMutex);
        return STATUS_INVALID_PARAMETER;
    }

    alloc->CpuMapRefCount--;

    if (alloc->CpuMapRefCount == 0) {
        if (alloc->CpuMapWritePending && alloc->CpuMapMdl) {
            /* Flush for CPU -> device reads. */
            KeFlushIoBuffers(alloc->CpuMapMdl, /*ReadOperation*/ FALSE, /*DmaOperation*/ TRUE);
        }
        AeroGpuAllocationUnmapCpu(alloc);
    }

    ExReleaseFastMutex(&alloc->CpuMapMutex);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCreateDevice(_In_ const HANDLE hAdapter,
                                                _Inout_ DXGKARG_CREATEDEVICE* pCreate)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pCreate) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_DEVICE* dev =
        (AEROGPU_DEVICE*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*dev), AEROGPU_POOL_TAG);
    if (!dev) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(dev, sizeof(*dev));
    dev->Adapter = adapter;

    pCreate->hDevice = (HANDLE)dev;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyDevice(_In_ const HANDLE hDevice)
{
    if (hDevice) {
        ExFreePoolWithTag((PVOID)hDevice, AEROGPU_POOL_TAG);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiCreateContext(_In_ const HANDLE hDevice,
                                                 _Inout_ DXGKARG_CREATECONTEXT* pCreate)
{
    AEROGPU_DEVICE* dev = (AEROGPU_DEVICE*)hDevice;
    if (!dev || !pCreate) {
        return STATUS_INVALID_PARAMETER;
    }

    AEROGPU_ADAPTER* adapter = dev->Adapter;

    AEROGPU_CONTEXT* ctx =
        (AEROGPU_CONTEXT*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*ctx), AEROGPU_POOL_TAG);
    if (!ctx) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(ctx, sizeof(*ctx));
    ctx->Device = dev;
    ctx->ContextId = 0;
    if (adapter) {
        ULONG id = (ULONG)InterlockedIncrement(&adapter->NextContextId);
        if (id == 0) {
            id = (ULONG)InterlockedIncrement(&adapter->NextContextId);
        }
        ctx->ContextId = id;
    }
    pCreate->hContext = (HANDLE)ctx;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiDestroyContext(_In_ const HANDLE hContext)
{
    if (hContext) {
        ExFreePoolWithTag((PVOID)hContext, AEROGPU_POOL_TAG);
    }
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuBuildAndAttachMeta(_Inout_ AEROGPU_ADAPTER* Adapter,
                                                   _In_ UINT AllocationCount,
                                                   _In_reads_opt_(AllocationCount) const DXGK_ALLOCATIONLIST* AllocationList,
                                                   _In_ BOOLEAN SkipAllocTable,
                                                   _Out_ AEROGPU_SUBMISSION_META** MetaOut)
{
    *MetaOut = NULL;

    if (!Adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!AllocationCount || !AllocationList) {
        return STATUS_SUCCESS;
    }

    if (AllocationCount > AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT) {
        return STATUS_INVALID_PARAMETER;
    }

    SIZE_T allocBytes = 0;
    NTSTATUS st = RtlSizeTMult((SIZE_T)AllocationCount, sizeof(aerogpu_legacy_submission_desc_allocation), &allocBytes);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }

    SIZE_T metaSize = 0;
    st = RtlSizeTAdd(FIELD_OFFSET(AEROGPU_SUBMISSION_META, Allocations), allocBytes, &metaSize);
    if (!NT_SUCCESS(st)) {
        return STATUS_INTEGER_OVERFLOW;
    }

    AEROGPU_SUBMISSION_META* meta =
        (AEROGPU_SUBMISSION_META*)ExAllocatePoolWithTag(NonPagedPool, metaSize, AEROGPU_POOL_TAG);
    if (!meta) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(meta, metaSize);

    meta->AllocationCount = AllocationCount;

    const BOOLEAN buildAllocTable = (Adapter->AbiKind == AEROGPU_ABI_KIND_V1 && !SkipAllocTable);
    if (buildAllocTable) {
        st = AeroGpuBuildAllocTable(
            Adapter, AllocationList, AllocationCount, &meta->AllocTableVa, &meta->AllocTablePa, &meta->AllocTableSizeBytes);
        if (!NT_SUCCESS(st)) {
            ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
            return st;
        }
    }

    for (UINT i = 0; i < AllocationCount; ++i) {
        AEROGPU_ALLOCATION* alloc = (AEROGPU_ALLOCATION*)AllocationList[i].hAllocation;
        meta->Allocations[i].allocation_handle = (uint64_t)(ULONG_PTR)AllocationList[i].hAllocation;
        meta->Allocations[i].gpa = (uint64_t)AllocationList[i].PhysicalAddress.QuadPart;
        meta->Allocations[i].size_bytes = (uint32_t)(alloc ? alloc->SizeBytes : 0);
        meta->Allocations[i].alloc_id = (uint32_t)(alloc ? alloc->AllocationId : 0);

        /*
         * AeroGpuBuildAllocTable updates LastKnownPa when it runs, but alloc tables can be
         * intentionally skipped (or omitted on non-v1 ABIs). Keep LastKnownPa updated so
         * DxgkDdiLock can fall back to it when PhysicalAddress isn't provided by dxgkrnl.
         */
        if (!buildAllocTable && alloc) {
            ExAcquireFastMutex(&alloc->CpuMapMutex);
            alloc->LastKnownPa.QuadPart = AllocationList[i].PhysicalAddress.QuadPart;
            ExReleaseFastMutex(&alloc->CpuMapMutex);
        }
    }

    *MetaOut = meta;
    return STATUS_SUCCESS;
}

/*
 * Determine whether a command stream requires `alloc_id` resolution via the per-submit allocation
 * table.
 *
 * This is used to decide whether a v1 submission must include an allocation table.
 *
 * NOTE: This is intentionally a minimal parser:
 * - It only looks for CREATE_BUFFER / CREATE_TEXTURE2D packets and inspects their backing_alloc_id.
 * - It treats RESOURCE_DIRTY_RANGE and COPY_* WRITEBACK_DST as requiring an alloc table (these packets
 *   imply host access to guest-backed memory and are invalid without a guest allocation backing).
 * - Any malformed stream is treated as "no reference" here; the host will validate the stream.
 */
static BOOLEAN AeroGpuCmdStreamRequiresAllocTable(_In_reads_bytes_opt_(SizeBytes) const VOID* CmdStream,
                                                  _In_ ULONG SizeBytes)
{
    if (!CmdStream || SizeBytes < sizeof(struct aerogpu_cmd_stream_header)) {
        return FALSE;
    }
 
    const UCHAR* bytes = (const UCHAR*)CmdStream;
    struct aerogpu_cmd_stream_header sh;
    RtlCopyMemory(&sh, bytes, sizeof(sh));
 
    if (sh.magic != AEROGPU_CMD_STREAM_MAGIC) {
        return FALSE;
    }
 
    if (sh.size_bytes < sizeof(struct aerogpu_cmd_stream_header) || sh.size_bytes > SizeBytes) {
        return FALSE;
    }
 
    ULONG offset = sizeof(struct aerogpu_cmd_stream_header);
    const ULONG streamSize = sh.size_bytes;
 
    while (offset < streamSize) {
        const ULONG remaining = streamSize - offset;
        if (remaining < sizeof(struct aerogpu_cmd_hdr)) {
            break;
        }
        struct aerogpu_cmd_hdr hdr;
        RtlCopyMemory(&hdr, bytes + offset, sizeof(hdr));
 
        if (hdr.size_bytes < sizeof(struct aerogpu_cmd_hdr) || (hdr.size_bytes & 3u) != 0) {
            return FALSE;
        }
 
        ULONG end = 0;
        NTSTATUS st = RtlULongAdd(offset, hdr.size_bytes, &end);
        if (!NT_SUCCESS(st) || end > streamSize) {
            return FALSE;
        }

        if (hdr.opcode == AEROGPU_CMD_CREATE_BUFFER) {
            /* backing_alloc_id is at offset 24 from the packet start. */
            if (hdr.size_bytes >= 28) {
                uint32_t backingAllocId = 0;
                RtlCopyMemory(&backingAllocId, bytes + offset + 24, sizeof(backingAllocId));
                if (backingAllocId != 0) {
                    return TRUE;
                }
            }
        } else if (hdr.opcode == AEROGPU_CMD_CREATE_TEXTURE2D) {
            /* backing_alloc_id is at offset 40 from the packet start. */
            if (hdr.size_bytes >= 44) {
                uint32_t backingAllocId = 0;
                RtlCopyMemory(&backingAllocId, bytes + offset + 40, sizeof(backingAllocId));
                if (backingAllocId != 0) {
                    return TRUE;
                }
            }
        } else if (hdr.opcode == AEROGPU_CMD_RESOURCE_DIRTY_RANGE) {
            return TRUE;
        } else if (hdr.opcode == AEROGPU_CMD_COPY_BUFFER) {
            /* flags is at offset 40 from the packet start. */
            if (hdr.size_bytes >= 44) {
                uint32_t flags = 0;
                RtlCopyMemory(&flags, bytes + offset + 40, sizeof(flags));
                if ((flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0) {
                    return TRUE;
                }
            }
        } else if (hdr.opcode == AEROGPU_CMD_COPY_TEXTURE2D) {
            /* flags is at offset 56 from the packet start. */
            if (hdr.size_bytes >= 60) {
                uint32_t flags = 0;
                RtlCopyMemory(&flags, bytes + offset + 56, sizeof(flags));
                if ((flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0) {
                    return TRUE;
                }
            }
        }

        offset = end;
    }

    return FALSE;
}

static NTSTATUS APIENTRY AeroGpuDdiRender(_In_ const HANDLE hContext, _Inout_ DXGKARG_RENDER* pRender)
{
    AEROGPU_CONTEXT* ctx = (AEROGPU_CONTEXT*)hContext;
    AEROGPU_ADAPTER* adapter = (ctx && ctx->Device) ? ctx->Device->Adapter : NULL;
    if (!adapter || !pRender || !pRender->pDmaBufferPrivateData ||
        pRender->DmaBufferPrivateDataSize < sizeof(AEROGPU_DMA_PRIV)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (AeroGpuIsDeviceErrorLatched(adapter)) {
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pRender->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_RENDER;
    priv->Reserved0 = ctx ? ctx->ContextId : 0;
    priv->MetaHandle = 0;

    /*
     * Render/Present can run during power transitions (or after the device is
     * disabled). Avoid allocating per-submit metadata when the adapter is not
     * ready to accept submissions; SubmitCommand can rebuild the metadata from
     * the allocation list once the device is back in D0.
     */
    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0) &&
        (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0);
    if (!poweredOn) {
        return STATUS_SUCCESS;
    }

    if (pRender->AllocationListSize && pRender->pAllocationList) {
        ULONG pendingCount = 0;
        ULONGLONG pendingBytes = 0;
        if (AeroGpuMetaHandleAtCapacity(adapter, &pendingCount, &pendingBytes)) {
#if DBG
            AEROGPU_LOG_RATELIMITED(g_PendingMetaHandleCapLogCount,
                                    8,
                                    "DdiRender: pending meta handle cap hit (count=%lu/%lu bytes=%I64u/%I64u)",
                                    pendingCount,
                                    (ULONG)AEROGPU_PENDING_META_HANDLES_MAX_COUNT,
                                    pendingBytes,
                                    (ULONGLONG)AEROGPU_PENDING_META_HANDLES_MAX_BYTES);
#endif
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        AEROGPU_SUBMISSION_META* meta = NULL;
        NTSTATUS st = AeroGpuBuildAndAttachMeta(adapter,
                                                pRender->AllocationListSize,
                                                pRender->pAllocationList,
                                                /*SkipAllocTable*/ FALSE,
                                                &meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }

        st = AeroGpuMetaHandleStore(adapter, meta, &priv->MetaHandle);
        if (!NT_SUCCESS(st)) {
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return st;
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiPresent(_In_ const HANDLE hContext, _Inout_ DXGKARG_PRESENT* pPresent)
{
    AEROGPU_CONTEXT* ctx = (AEROGPU_CONTEXT*)hContext;
    AEROGPU_ADAPTER* adapter = (ctx && ctx->Device) ? ctx->Device->Adapter : NULL;
    if (!adapter || !pPresent || !pPresent->pDmaBufferPrivateData ||
        pPresent->DmaBufferPrivateDataSize < sizeof(AEROGPU_DMA_PRIV)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (AeroGpuIsDeviceErrorLatched(adapter)) {
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }

    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pPresent->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_PRESENT;
    priv->Reserved0 = ctx ? ctx->ContextId : 0;
    priv->MetaHandle = 0;

    /* See AeroGpuDdiRender: skip allocating metadata when the device can't accept submissions. */
    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0) &&
        (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0);
    if (!poweredOn) {
        return STATUS_SUCCESS;
    }

    if (pPresent->AllocationListSize && pPresent->pAllocationList) {
        ULONG pendingCount = 0;
        ULONGLONG pendingBytes = 0;
        if (AeroGpuMetaHandleAtCapacity(adapter, &pendingCount, &pendingBytes)) {
#if DBG
            AEROGPU_LOG_RATELIMITED(g_PendingMetaHandleCapLogCount,
                                    8,
                                    "DdiPresent: pending meta handle cap hit (count=%lu/%lu bytes=%I64u/%I64u)",
                                    pendingCount,
                                    (ULONG)AEROGPU_PENDING_META_HANDLES_MAX_COUNT,
                                    pendingBytes,
                                    (ULONGLONG)AEROGPU_PENDING_META_HANDLES_MAX_BYTES);
#endif
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        AEROGPU_SUBMISSION_META* meta = NULL;
        NTSTATUS st = AeroGpuBuildAndAttachMeta(adapter,
                                                pPresent->AllocationListSize,
                                                pPresent->pAllocationList,
                                                /*SkipAllocTable*/ FALSE,
                                                &meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }

        st = AeroGpuMetaHandleStore(adapter, meta, &priv->MetaHandle);
        if (!NT_SUCCESS(st)) {
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return st;
        }
    }

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiBuildPagingBuffer(_In_ const HANDLE hAdapter,
                                                     _Inout_ DXGKARG_BUILDPAGINGBUFFER* pBuildPagingBuffer)
{
    UNREFERENCED_PARAMETER(hAdapter);
    if (!pBuildPagingBuffer || !pBuildPagingBuffer->pDmaBufferPrivateData ||
        pBuildPagingBuffer->DmaBufferPrivateDataSize < sizeof(AEROGPU_DMA_PRIV)) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Emit no-op paging buffers; system-memory-only segment keeps paging simple. */
    pBuildPagingBuffer->DmaBufferSize = 0;
    AEROGPU_DMA_PRIV* priv = (AEROGPU_DMA_PRIV*)pBuildPagingBuffer->pDmaBufferPrivateData;
    priv->Type = AEROGPU_SUBMIT_PAGING;
    priv->Reserved0 = 0;
    priv->MetaHandle = 0;
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSubmitCommand(_In_ const HANDLE hAdapter,
                                                  _In_ const DXGKARG_SUBMITCOMMAND* pSubmitCommand)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pSubmitCommand) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONG fence32 = (ULONG)pSubmitCommand->SubmissionFenceId;
    ULONGLONG fence = (ULONGLONG)fence32;
    ULONG dmaSizeBytes = (ULONG)pSubmitCommand->DmaBufferSize;
    ULONG type = (dmaSizeBytes != 0) ? AEROGPU_SUBMIT_RENDER : AEROGPU_SUBMIT_PAGING;
    ULONG contextId = 0;
    ULONGLONG metaHandle = 0;
    if (pSubmitCommand->pDmaBufferPrivateData &&
        pSubmitCommand->DmaBufferPrivateDataSize >= sizeof(AEROGPU_DMA_PRIV)) {
        const AEROGPU_DMA_PRIV* priv = (const AEROGPU_DMA_PRIV*)pSubmitCommand->pDmaBufferPrivateData;
        type = priv->Type;
        contextId = priv->Reserved0;
        metaHandle = priv->MetaHandle;
    }

    if (AeroGpuIsDeviceErrorLatched(adapter)) {
        /* Best-effort: drain any per-submit meta handle so we don't leak on device-lost. */
        if (metaHandle != 0) {
            AEROGPU_SUBMISSION_META* metaEarly = AeroGpuMetaHandleTake(adapter, metaHandle);
            if (metaEarly) {
                AeroGpuFreeSubmissionMeta(adapter, metaEarly);
            }
        }
        return STATUS_GRAPHICS_DEVICE_REMOVED;
    }

    /*
     * If the adapter is not in D0 / not accepting submissions, fail fast.
     *
     * Note: if a Render/Present path already built a per-submit allocation table
     * and stored it behind a MetaHandle, take + free it here so we don't leak
     * when SubmitCommand is rejected.
     */
    const BOOLEAN poweredOn =
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);
    const BOOLEAN accepting = (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0);
    if (!poweredOn || !accepting || !adapter->Bar0) {
        if (metaHandle != 0) {
            AEROGPU_SUBMISSION_META* metaEarly = AeroGpuMetaHandleTake(adapter, metaHandle);
            if (metaEarly) {
                AeroGpuFreeSubmissionMeta(adapter, metaEarly);
            }
        }
        return STATUS_DEVICE_NOT_READY;
    }

    AEROGPU_SUBMISSION_META* meta = NULL;
    if (metaHandle != 0) {
        meta = AeroGpuMetaHandleTake(adapter, metaHandle);
        if (!meta) {
            /*
             * Be robust against stale MetaHandles (e.g. after power transitions,
             * TDR recovery, or scheduler cancellation). If the submit args carry
             * an allocation list, rebuild the metadata on-demand; otherwise,
             * continue without it (subsequent validation may still reject the
             * submission if an alloc table is required).
             */
#if DBG
            static volatile LONG g_MissingMetaHandleLogs = 0;
            const LONG n = InterlockedIncrement(&g_MissingMetaHandleLogs);
            if ((n <= 8) || ((n & 1023) == 0)) {
                AEROGPU_LOG("SubmitCommand: MetaHandle=0x%I64x not found; rebuilding if possible (fence=%I64u)",
                            metaHandle,
                            (ULONGLONG)pSubmitCommand->SubmissionFenceId);
            }
#endif
        }
    }

    /*
     * Some WDDM submission paths can bypass DxgkDdiRender/DxgkDdiPresent and call
     * DxgkDdiSubmitCommand directly (e.g. when the D3D9 runtime routes through
     * SubmitCommandCb). In that case, AEROGPU_DMA_PRIV.MetaHandle may be 0, but
     * an allocation list is still available in the submit args.
     *
     * Build the per-submit allocation table on-demand so guest-backed resources
     * remain resolvable by alloc_id.
     */
    if (!meta && dmaSizeBytes != 0 && pSubmitCommand->AllocationListSize && pSubmitCommand->pAllocationList) {
        NTSTATUS st = AeroGpuBuildAndAttachMeta(adapter,
                                                pSubmitCommand->AllocationListSize,
                                                pSubmitCommand->pAllocationList,
                                                /*SkipAllocTable*/ FALSE,
                                                &meta);
        if (!NT_SUCCESS(st)) {
            return st;
        }
    }

    /*
     * When MetaHandle is missing, the per-context ID may not have been stamped
     * into AEROGPU_DMA_PRIV. Recover it directly from the submit args so the
     * emulator can still isolate per-context state.
     */
    if (contextId == 0 && pSubmitCommand->hContext) {
        AEROGPU_CONTEXT* ctx = (AEROGPU_CONTEXT*)pSubmitCommand->hContext;
        if (ctx) {
            contextId = ctx->ContextId;
        }
    }

    PHYSICAL_ADDRESS dmaPa;
    dmaPa.QuadPart = 0;
    PVOID dmaVa = NULL;

    /*
     * Defensive: some user-mode/runtime paths report DMA buffer *capacity* rather
     * than bytes-used. The AeroGPU command stream carries its own length in the
     * stream header; prefer that size when it is self-consistent so we never
     * copy uninitialized bytes into the ring submission.
     */
    if (dmaSizeBytes != 0 && pSubmitCommand->pDmaBuffer &&
        dmaSizeBytes >= sizeof(struct aerogpu_cmd_stream_header)) {
        struct aerogpu_cmd_stream_header hdr;
        RtlCopyMemory(&hdr, pSubmitCommand->pDmaBuffer, sizeof(hdr));
        if (hdr.magic == AEROGPU_CMD_STREAM_MAGIC &&
            hdr.size_bytes >= sizeof(struct aerogpu_cmd_stream_header) &&
            hdr.size_bytes <= (uint32_t)dmaSizeBytes) {
            dmaSizeBytes = (ULONG)hdr.size_bytes;
        }
    }

    /*
     * Cap the effective DMA copy size (after header shrink) to avoid extremely
     * large contiguous allocations from pathological user-mode submissions.
     */
    const ULONG maxDmaBytes = g_AeroGpuMaxDmaBufferBytes;
#if DBG
    static volatile LONG g_SubmitDmaTooLargeLogCount = 0;
#endif

    if (dmaSizeBytes != 0) {
        if (dmaSizeBytes > maxDmaBytes) {
#if DBG
            AEROGPU_LOG_RATELIMITED(g_SubmitDmaTooLargeLogCount,
                                    8,
                                    "SubmitCommand: DMA buffer too large: fence=%I64u size=%lu max=%lu",
                                    fence,
                                    dmaSizeBytes,
                                    maxDmaBytes);
#endif
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }

        if (!pSubmitCommand->pDmaBuffer) {
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }

        /*
         * This is a temporary DMA copy buffer that is immediately and fully
         * overwritten below via RtlCopyMemory, so avoid zeroing it.
         */
        dmaVa = AeroGpuAllocContiguousNoInit(adapter, dmaSizeBytes, &dmaPa);
        if (!dmaVa) {
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlCopyMemory(dmaVa, pSubmitCommand->pDmaBuffer, dmaSizeBytes);
    } else if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        /*
         * Paging submissions use a 0-byte DMA buffer in this bring-up driver, but the
         * versioned (AGPU) ABI expects `cmd_gpa/cmd_size_bytes` to describe an AeroGPU
         * command stream. Provide a minimal NOP stream so the submission is well-formed
         * and future host-side validators can accept it.
         */
        dmaSizeBytes = sizeof(struct aerogpu_cmd_stream_header) + sizeof(struct aerogpu_cmd_hdr);

        if (dmaSizeBytes > maxDmaBytes) {
#if DBG
            AEROGPU_LOG_RATELIMITED(g_SubmitDmaTooLargeLogCount,
                                    8,
                                    "SubmitCommand: DMA buffer too large: fence=%I64u size=%lu max=%lu",
                                    fence,
                                    dmaSizeBytes,
                                    maxDmaBytes);
#endif
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }

        /* Fully initialized below (header + NOP packet). */
        dmaVa = AeroGpuAllocContiguousNoInit(adapter, dmaSizeBytes, &dmaPa);
        if (!dmaVa) {
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        struct aerogpu_cmd_stream_header stream;
        RtlZeroMemory(&stream, sizeof(stream));
        stream.magic = AEROGPU_CMD_STREAM_MAGIC;
        stream.abi_version = AEROGPU_ABI_VERSION_U32;
        stream.size_bytes = (uint32_t)dmaSizeBytes;
        stream.flags = AEROGPU_CMD_STREAM_FLAG_NONE;
        stream.reserved0 = 0;
        stream.reserved1 = 0;

        struct aerogpu_cmd_hdr nop;
        RtlZeroMemory(&nop, sizeof(nop));
        nop.opcode = AEROGPU_CMD_NOP;
        nop.size_bytes = (uint32_t)sizeof(struct aerogpu_cmd_hdr);

        RtlCopyMemory(dmaVa, &stream, sizeof(stream));
        RtlCopyMemory((PUCHAR)dmaVa + sizeof(stream), &nop, sizeof(nop));
    }

    PVOID allocTableVa = NULL;
    PHYSICAL_ADDRESS allocTablePa;
    UINT allocTableSizeBytes = 0;
    UINT allocCount = 0;
    allocTablePa.QuadPart = 0;
    if (meta) {
        allocTableVa = meta->AllocTableVa;
        allocTablePa = meta->AllocTablePa;
        allocTableSizeBytes = meta->AllocTableSizeBytes;
        allocCount = meta->AllocationCount;
    }
 
    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        /*
         * v1 ABI: allocation table is required for any submission whose command stream requires
         * alloc_id resolution (guest-backed CREATE_*, RESOURCE_DIRTY_RANGE, COPY_* WRITEBACK_DST),
         * or whose allocation list includes any allocations with non-zero AllocationId (the KMD
         * will encode those into the table).
         *
         * If the command stream requires an alloc table but we were not able to build one, fail
         * the submission instead of sending an incomplete descriptor to the host/emulator.
         */
        const BOOLEAN cmdNeedsAllocTable = AeroGpuCmdStreamRequiresAllocTable(dmaVa, dmaSizeBytes);
        const BOOLEAN listHasAllocIds = (allocTableSizeBytes != 0);
        const BOOLEAN needsAllocTable = cmdNeedsAllocTable || listHasAllocIds;
  
        if (cmdNeedsAllocTable && !listHasAllocIds) {
            AEROGPU_LOG("SubmitCommand: command stream requires alloc table but alloc table is missing (fence=%I64u)",
                        fence);
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }
 
        if (!needsAllocTable) {
            allocTableVa = NULL;
            allocTablePa.QuadPart = 0;
            allocTableSizeBytes = 0;
        }
    }

    PVOID descVa = NULL;
    SIZE_T descSize = 0;
    PHYSICAL_ADDRESS descPa;
    descPa.QuadPart = 0;

    if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
        if (allocCount > AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT) {
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }

        SIZE_T allocBytes = 0;
        NTSTATUS st = RtlSizeTMult((SIZE_T)allocCount, sizeof(aerogpu_legacy_submission_desc_allocation), &allocBytes);
        if (!NT_SUCCESS(st) ||
            !NT_SUCCESS(RtlSizeTAdd(sizeof(aerogpu_legacy_submission_desc_header), allocBytes, &descSize)) ||
            descSize > UINT32_MAX) {
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INTEGER_OVERFLOW;
        }

        if (descSize > (SIZE_T)maxDmaBytes) {
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INVALID_PARAMETER;
        }

        aerogpu_legacy_submission_desc_header* desc =
            (aerogpu_legacy_submission_desc_header*)AeroGpuAllocContiguousNoInit(adapter, descSize, &descPa);
        descVa = desc;
        if (!desc) {
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            AeroGpuFreeSubmissionMeta(adapter, meta);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        desc->version = AEROGPU_LEGACY_SUBMISSION_DESC_VERSION;
        desc->type = type;
        desc->fence = (uint32_t)fence32;
        desc->reserved0 = 0;
        desc->dma_buffer_gpa = (uint64_t)dmaPa.QuadPart;
        desc->dma_buffer_size = dmaSizeBytes;
        desc->allocation_count = allocCount;

        if (allocCount && meta) {
            aerogpu_legacy_submission_desc_allocation* out = (aerogpu_legacy_submission_desc_allocation*)(desc + 1);
            RtlCopyMemory(out, meta->Allocations, allocBytes);
        }
    }

    AEROGPU_SUBMISSION* sub =
        (AEROGPU_SUBMISSION*)ExAllocatePoolWithTag(NonPagedPool, sizeof(*sub), AEROGPU_POOL_TAG);
    if (!sub) {
        AeroGpuFreeContiguousNonCached(adapter, descVa, descSize);
        AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
        AeroGpuFreeSubmissionMeta(adapter, meta);
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    RtlZeroMemory(sub, sizeof(*sub));
    sub->Fence = 0;
    sub->DmaCopyVa = dmaVa;
    sub->DmaCopySize = dmaSizeBytes;
    sub->DmaCopyPa = dmaPa;
    sub->DescVa = descVa;
    sub->DescSize = descSize;
    sub->DescPa = descPa;
    sub->AllocTableVa = NULL;
    sub->AllocTablePa.QuadPart = 0;
    sub->AllocTableSizeBytes = 0;

    KIRQL oldIrql;
    KeAcquireSpinLock(&adapter->PendingLock, &oldIrql);

    /*
     * Submit first, then record tracking information, but keep the pending lock
     * held across both so the fence completion DPC can't run before the
     * submission is visible in PendingSubmissions.
     */
    NTSTATUS ringSt = STATUS_SUCCESS;
    if (AeroGpuIsDeviceErrorLatched(adapter)) {
        ringSt = STATUS_GRAPHICS_DEVICE_REMOVED;
    } else if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        fence = AeroGpuV1ExtendFenceLocked(adapter, fence32);
        uint32_t submitFlags = 0;
        if (type == AEROGPU_SUBMIT_PRESENT) {
            submitFlags |= AEROGPU_SUBMIT_FLAG_PRESENT;
        }

        const uint64_t allocTableGpa = allocTableSizeBytes ? (uint64_t)allocTablePa.QuadPart : 0;
        ringSt = AeroGpuV1RingPushSubmit(adapter,
                                         submitFlags,
                                         (uint32_t)contextId,
                                         dmaPa,
                                         dmaSizeBytes,
                                         allocTableGpa,
                                         (uint32_t)allocTableSizeBytes,
                                         fence,
                                         NULL);
    } else {
        fence = (ULONGLONG)fence32;
        ringSt = AeroGpuLegacyRingPushSubmit(adapter, fence32, (ULONG)descSize, descPa);
    }

    if (NT_SUCCESS(ringSt)) {
        sub->Fence = fence;
        sub->AllocTableVa = allocTableVa;
        sub->AllocTablePa = allocTablePa;
        sub->AllocTableSizeBytes = allocTableSizeBytes;

        InsertTailList(&adapter->PendingSubmissions, &sub->ListEntry);
        AeroGpuAtomicWriteU64(&adapter->LastSubmittedFence, fence);
    }

    KeReleaseSpinLock(&adapter->PendingLock, oldIrql);

    if (!NT_SUCCESS(ringSt)) {
        ExFreePoolWithTag(sub, AEROGPU_POOL_TAG);
        AeroGpuFreeContiguousNonCached(adapter, descVa, descSize);
        AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
        AeroGpuFreeSubmissionMeta(adapter, meta);
        return ringSt;
    }

    /* Track successful submissions for dbgctl perf counters. */
    InterlockedIncrement64(&adapter->PerfTotalSubmissions);
    if (type == AEROGPU_SUBMIT_PRESENT) {
        InterlockedIncrement64(&adapter->PerfTotalPresents);
    } else if (type == AEROGPU_SUBMIT_RENDER) {
        InterlockedIncrement64(&adapter->PerfTotalRenderSubmits);
    }

    if (meta) {
        ExFreePoolWithTag(meta, AEROGPU_POOL_TAG);
    }

    AeroGpuLogSubmission(adapter, fence, type, dmaSizeBytes);

    return STATUS_SUCCESS;
}

static BOOLEAN APIENTRY AeroGpuDdiInterruptRoutine(_In_ const PVOID MiniportDeviceContext,
                                                   _In_ ULONG MessageNumber)
{
    UNREFERENCED_PARAMETER(MessageNumber);
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter || !adapter->Bar0) {
        return FALSE;
    }

    /*
     * Be defensive during power transitions:
     * - dxgkrnl can deliver an interrupt while the adapter is transitioning away from D0
     *   (or after we have marked it non-D0 but before IRQ_ENABLE is fully quiesced).
     * - During resume-to-D0, the driver temporarily blocks submissions while reinitialising ring/IRQ
     *   state; avoid running normal ISR logic during that window as well.
     *
     * In both cases, skip normal ISR processing and best-effort ACK any pending bits to deassert a
     * level-triggered line.
     */
    const DXGK_DEVICE_POWER_STATE powerState =
        (DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0);
    const BOOLEAN acceptingSubmissions =
        (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
    if (powerState != DxgkDevicePowerStateD0) {
        /*
         * The adapter is in a non-D0 state (or transitioning away from D0).
         *
         * Avoid normal ISR processing; best-effort ACK any pending bits to deassert a level-triggered line.
         *
         * NOTE: We return TRUE to claim the interrupt here because we cannot safely query full device state
         * in a powered-down transition window and want to avoid unhandled interrupt storms.
         */
        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        }
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1 &&
            adapter->Bar0Length >= (AEROGPU_LEGACY_REG_INT_ACK + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
        }
        return TRUE;
    }

    if (!acceptingSubmissions) {
        /*
         * The adapter is in D0 but the submission path is not ready (resume/teardown window).
         *
         * Best-effort clear any device-pending bits, but avoid claiming unrelated shared interrupts:
         * only return TRUE when we observe an enabled pending bit from this device.
         */
        BOOLEAN shouldClaim = FALSE;

        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            const ULONG status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
            ULONG enableMask = 0;
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
                enableMask = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
            } else {
                enableMask = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->IrqEnableMask);
            }
            if ((status & enableMask) != 0) {
                shouldClaim = TRUE;
            }
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        }

        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1 &&
            adapter->Bar0Length >= (AEROGPU_LEGACY_REG_INT_STATUS + sizeof(ULONG))) {
            const ULONG legacyStatus = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_INT_STATUS);
            if (legacyStatus != 0) {
                shouldClaim = TRUE;
            }
        }
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1 &&
            adapter->Bar0Length >= (AEROGPU_LEGACY_REG_INT_ACK + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
        }

        return shouldClaim;
    }
    BOOLEAN any = FALSE;
    BOOLEAN queueDpc = FALSE;

    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        const ULONG status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
        const ULONG known = (AEROGPU_IRQ_FENCE | AEROGPU_IRQ_SCANOUT_VBLANK | AEROGPU_IRQ_ERROR);
        /*
         * Only process enabled IRQ_STATUS bits.
         *
         * This is important for:
         * - vblank: dxgkrnl toggles delivery via DxgkDdiControlInterrupt. A vblank status bit may
         *   latch while the IRQ is masked; if a fence interrupt later fires, we must ACK it but not
         *   notify dxgkrnl.
         * - error: after observing IRQ_ERROR, the ISR masks off ERROR delivery to avoid storms from
         *   a level-triggered/sticky status bit. We must not repeatedly treat the sticky bit as a
         *   new error on every subsequent (enabled) vblank/fence interrupt.
         */
        ULONG enableMask = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->IrqEnableMask);
        if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
            /*
             * Prefer the device's IRQ_ENABLE register over the cached mask.
             *
             * IRQ line assertion is defined by the device contract as (STATUS & ENABLE) != 0, so
             * using the live ENABLE value avoids corner cases where the cached mask and hardware
             * state diverge (e.g. device reset).
             */
            enableMask = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
        }
        const ULONG pending = status & enableMask;
        const ULONG handled = pending & known;
        const ULONG unknown = status & ~known;
        if (handled == 0) {
            if (status != 0) {
                /*
                 * Defensive: if the device reports an IRQ_STATUS bit we don't understand,
                 * still ACK it to avoid interrupt storms from a stuck level-triggered line.
                 */
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, status);
                static LONG g_UnexpectedIrqWarned = 0;
                if (pending != 0 || unknown != 0) {
                    InterlockedIncrement64(&adapter->PerfIrqSpurious);

                    if (pending != 0) {
                        /*
                         * The device asserted the interrupt line due to an enabled bit that this
                         * driver does not understand (pending & ~known != 0).
                         *
                         * Claim the interrupt to avoid starving other shared ISR handlers.
                         */
                        InterlockedIncrement(&adapter->IrqIsrCount);
                        if (InterlockedExchange(&g_UnexpectedIrqWarned, 1) == 0) {
                            DbgPrintEx(
                                DPFLTR_IHVVIDEO_ID,
                                DPFLTR_ERROR_LEVEL,
                                "aerogpu-kmd: unexpected IRQ_STATUS bits (status=0x%08lx pending=0x%08lx enable=0x%08lx)\n",
                                status,
                                pending,
                                enableMask);
                        }
                        return TRUE;
                    }
                }
                /*
                 * `status` has only known bits, but none of them are currently enabled.
                 *
                 * This can happen due to ControlInterrupt races (e.g. a vblank bit latched while
                 * masked) or due to unrelated shared interrupts. We ACK the status bits so they
                 * don't remain sticky, but return FALSE so a shared-interrupt chain can continue
                 * dispatching other ISR handlers.
                 */
                return FALSE;
            }
            return FALSE;
        }

        if (unknown != 0) {
            InterlockedIncrement64(&adapter->PerfIrqSpurious);
        }

        /* Ack in the ISR to deassert the (level-triggered) interrupt line. */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, status);

        ULONGLONG completedFence64 = 0;
        ULONGLONG lastSubmittedFenceSnapshot = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
        ULONGLONG lastCompletedFenceSnapshot = AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
        BOOLEAN haveCompletedFence = FALSE;
        /*
         * Update completed fence tracking whenever the device reports a fence advancement, even if
         * dxgkrnl has temporarily masked DMA_COMPLETED interrupt delivery.
         *
         * The KMD still needs a reasonably fresh LastCompletedFence for internal bookkeeping:
         * - retiring PendingSubmissions (contiguous DMA buffers, dbgctl READ_GPA, etc.)
         * - debugging forward progress via dbgctl QUERY_FENCE/QUERY_PERF
         *
         * Note: we only *notify* dxgkrnl of DMA_COMPLETED when the fence interrupt is enabled
         * (handled & IRQ_FENCE), but we track the fence regardless when IRQ_STATUS reports it.
         */
        if ((status & AEROGPU_IRQ_FENCE) != 0 || (handled & AEROGPU_IRQ_ERROR) != 0) {
            completedFence64 = AeroGpuReadCompletedFence(adapter);

            /*
             * Clamp in the *extended* fence domain.
             *
             * For the v1 protocol, the KMD must submit monotonically increasing 64-bit fences
             * (see AeroGpuV1ExtendFenceLocked). When reporting to dxgkrnl we truncate to 32-bit.
             */
            lastSubmittedFenceSnapshot = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
            lastCompletedFenceSnapshot = AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
            if (completedFence64 < lastCompletedFenceSnapshot) {
                completedFence64 = lastCompletedFenceSnapshot;
            }
            if (completedFence64 > lastSubmittedFenceSnapshot) {
                completedFence64 = lastSubmittedFenceSnapshot;
            }

            AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, completedFence64);
            haveCompletedFence = TRUE;
        }
        const ULONG completedFence32 = (ULONG)completedFence64;

        BOOLEAN sentDxgkFault = FALSE;
        ULONG faultedFence32 = 0;

        if ((handled & AEROGPU_IRQ_ERROR) != 0) {
            InterlockedExchange(&adapter->DeviceErrorLatched, 1);
            /*
             * Record a guest-time anchor for post-mortem inspection. This is a monotonic
             * timestamp in 100ns units since boot.
             */
            AeroGpuAtomicWriteU64(&adapter->LastErrorTime100ns, KeQueryInterruptTime());

            /*
             * Prevent interrupt storms if the device keeps asserting ERROR as a
             * level-triggered interrupt. We cannot take IrqEnableLock at DIRQL, so
             * update the cached mask atomically and write the new value directly.
             */
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
                const ULONG oldEnable =
                    (ULONG)InterlockedAnd((volatile LONG*)&adapter->IrqEnableMask, ~(LONG)AEROGPU_IRQ_ERROR);
                const ULONG newEnable = oldEnable & ~AEROGPU_IRQ_ERROR;
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, newEnable);
            }

            /*
             * Cache structured error payload when supported (ABI 1.3+).
             *
             * These registers remain valid until overwritten by a subsequent error and are useful
             * for post-mortem inspection even after the device has been powered down.
             */
            const ULONG abiMinor = (ULONG)(adapter->DeviceAbiVersion & 0xFFFFu);
            const BOOLEAN haveErrorRegs =
                ((adapter->DeviceFeatures & AEROGPU_FEATURE_ERROR_INFO) != 0) &&
                (abiMinor >= 3) &&
                (adapter->Bar0Length >= (AEROGPU_MMIO_REG_ERROR_COUNT + sizeof(ULONG)));
            if (haveErrorRegs) {
                ULONG code = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_CODE);
                if (code == 0) {
                    /* Treat unknown/invalid values as INTERNAL for consumers. */
                    code = (ULONG)AEROGPU_ERROR_INTERNAL;
                }
                const ULONG mmioCount = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_COUNT);
                InterlockedExchange((volatile LONG*)&adapter->LastErrorCode, (LONG)code);
                InterlockedExchange((volatile LONG*)&adapter->LastErrorMmioCount, (LONG)mmioCount);
            } else {
                /* Best-effort: no structured error payload; still record "internal" as the last seen error kind. */
                InterlockedExchange((volatile LONG*)&adapter->LastErrorCode, (LONG)AEROGPU_ERROR_INTERNAL);
                InterlockedExchange((volatile LONG*)&adapter->LastErrorMmioCount, 0);
            }
            /*
             * Choose a faulted fence ID that dxgkrnl can associate with a DMA buffer.
             *
             * If the interrupt also carried a fence completion bit, the completed fence is the best
             * approximation. If the device signaled ERROR without FENCE (for example, a failure that
             * arrives before a vsync-delayed fence completes), report the *next* in-flight fence so the
             * faulted fence ID is not trivially <= the last completed fence.
             */
            ULONGLONG errorFence = 0;
            ULONGLONG mmioErrorFence = 0;
            BOOLEAN haveMmioErrorFence = FALSE;
            if (haveErrorRegs && AeroGpuTryReadErrorFence64(adapter, &mmioErrorFence)) {
                const ULONGLONG lastCompletedForError = haveCompletedFence ? completedFence64 : lastCompletedFenceSnapshot;
                if (mmioErrorFence >= lastCompletedForError && mmioErrorFence <= lastSubmittedFenceSnapshot) {
                    /*
                     * If the device did not report a fence completion bit in this interrupt, prefer to
                     * report an in-flight fence (> last_completed) so dxgkrnl can associate the fault
                     * with a queued DMA buffer.
                     */
                    if (((handled & AEROGPU_IRQ_FENCE) != 0) || (mmioErrorFence > lastCompletedForError)) {
                        errorFence = mmioErrorFence;
                        haveMmioErrorFence = TRUE;
                    }
                }
            }

            if (!haveMmioErrorFence) {
                errorFence = haveCompletedFence ? completedFence64 : lastCompletedFenceSnapshot;
                if (((handled & AEROGPU_IRQ_FENCE) == 0) && (errorFence < lastSubmittedFenceSnapshot) && (errorFence != ~0ull)) {
                    ULONGLONG nextFence = errorFence + 1;
                    if (nextFence > lastSubmittedFenceSnapshot) {
                        nextFence = lastSubmittedFenceSnapshot;
                    }
                    errorFence = nextFence;
                }
            }
            AeroGpuAtomicWriteU64(&adapter->LastErrorFence, errorFence);
            faultedFence32 = (ULONG)errorFence;

            const ULONGLONG n = (ULONGLONG)InterlockedIncrement64((volatile LONGLONG*)&adapter->ErrorIrqCount);

            /*
             * Surface a meaningful WDDM fault to dxgkrnl so user mode sees device-hung semantics
             * (instead of a silent success with only a one-time kernel log).
             *
             * Do not spam dxgkrnl: notify the first few times and then only at exponentially
             * increasing intervals.
             */
            BOOLEAN shouldNotify = FALSE;
            if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                if (n <= 4 || ((n & (n - 1)) == 0)) {
                    const ULONGLONG prevNotified = AeroGpuAtomicExchangeU64(&adapter->LastNotifiedErrorFence, errorFence);
                    if (prevNotified != errorFence) {
                        shouldNotify = TRUE;
                    }
                }
            }

            if (shouldNotify && adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_FAULTED;
                notify.DmaFaulted.FaultedFenceId = (ULONG)errorFence;
                notify.DmaFaulted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                notify.DmaFaulted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
                sentDxgkFault = TRUE;
            }

#if DBG
            /* Keep a breadcrumb trail without spamming the kernel debugger. */
            if (n <= 4 || ((n & (n - 1)) == 0)) {
                const ULONG abiMinor = (ULONG)(adapter->DeviceAbiVersion & 0xFFFFu);
                if ((adapter->DeviceFeatures & AEROGPU_FEATURE_ERROR_INFO) != 0 &&
                    abiMinor >= 3 &&
                    adapter->Bar0Length >= (AEROGPU_MMIO_REG_ERROR_COUNT + sizeof(ULONG))) {
                    const ULONG code = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_CODE);
                    const ULONGLONG mmioFence = AeroGpuReadRegU64HiLoHi(adapter,
                                                                       AEROGPU_MMIO_REG_ERROR_FENCE_LO,
                                                                       AEROGPU_MMIO_REG_ERROR_FENCE_HI);
                    const ULONG mmioCount = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_COUNT);
                    DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                               DPFLTR_ERROR_LEVEL,
                               "aerogpu-kmd: device IRQ error (IRQ_STATUS=0x%08lx fence=%lu count=%I64u mmio_code=%lu(%s) mmio_fence=0x%I64x mmio_count=%lu)\n",
                               status,
                               (ULONG)errorFence,
                               (unsigned long long)n,
                               code,
                               AeroGpuErrorCodeName(code),
                               (unsigned long long)mmioFence,
                               mmioCount);
                } else {
                    DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                               DPFLTR_ERROR_LEVEL,
                               "aerogpu-kmd: device IRQ error (IRQ_STATUS=0x%08lx fence=%lu count=%I64u)\n",
                               status,
                               (ULONG)errorFence,
                               (unsigned long long)n);
                }
            }
#endif

            any = TRUE;
            queueDpc = TRUE;
        }

        if ((handled & AEROGPU_IRQ_FENCE) != 0) {
            InterlockedIncrement64(&adapter->PerfIrqFenceDelivered);
            InterlockedIncrement(&adapter->IrqIsrFenceCount);
            any = TRUE;
            queueDpc = TRUE;

            /*
             * If we notified dxgkrnl of a DMA fault for this interrupt, avoid reporting DMA_COMPLETED
             * for the *same* fence value. If the device signaled both FENCE and ERROR, the completed
             * fence may still be meaningful for retiring earlier work.
             */
            if ((!sentDxgkFault || faultedFence32 != completedFence32) && adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
                notify.DmaCompleted.SubmissionFenceId = completedFence32;
                notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
            }
        }

        if ((handled & AEROGPU_IRQ_SCANOUT_VBLANK) != 0) {
            /*
             * Defensive: the vblank IRQ bit may be asserted even if the device does not
             * expose the optional vblank timing registers (or if the feature bit is
             * not advertised). In that case, ACK it but avoid touching the vblank MMIO
             * register block.
             */
            if (!adapter->SupportsVblank) {
                InterlockedIncrement64(&adapter->PerfIrqSpurious);
                any = TRUE;
            } else {
                InterlockedIncrement64(&adapter->PerfIrqVblankDelivered);
                InterlockedIncrement(&adapter->IrqIsrVblankCount);
                /*
                 * Keep a guest-time anchor of the most recent vblank so GetScanLine callers don't
                 * need to poll the vblank sequence counter at high frequency.
                 */
                const ULONGLONG now100ns = KeQueryInterruptTime();
                const ULONGLONG seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                             AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                             AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
                const ULONGLONG timeNs = AeroGpuReadRegU64HiLoHi(adapter,
                                                                AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                                AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
                const ULONG periodNs = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
                if (periodNs != 0) {
                    adapter->VblankPeriodNs = periodNs;
                }
                AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
                AeroGpuAtomicWriteU64(&adapter->LastVblankTimeNs, timeNs);
                AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);

                any = TRUE;
                queueDpc = TRUE;

                if (adapter->DxgkInterface.DxgkCbNotifyInterrupt && adapter->VblankInterruptTypeValid) {
                    KeMemoryBarrier();
                    const DXGK_INTERRUPT_TYPE vblankType = adapter->VblankInterruptType;

                    DXGKARGCB_NOTIFY_INTERRUPT notify;
                    RtlZeroMemory(&notify, sizeof(notify));
                    notify.InterruptType = vblankType;

                    /*
                     * ABI-critical: for DXGK_INTERRUPT_TYPE_CRTC_VSYNC, dxgkrnl expects
                     * DXGKARGCB_NOTIFY_INTERRUPT.CrtcVsync.VidPnSourceId to identify the
                     * VidPn source that vblanked.
                     */
                    if (notify.InterruptType != DXGK_INTERRUPT_TYPE_CRTC_VSYNC) {
#if DBG
                        static volatile LONG g_UnexpectedVblankNotifyTypeLogs = 0;
                        const LONG n = InterlockedIncrement(&g_UnexpectedVblankNotifyTypeLogs);
                        if ((n <= 8) || ((n & 1023) == 0)) {
                            AEROGPU_LOG(
                                "InterruptRoutine: vblank uses unexpected InterruptType=%lu; expected DXGK_INTERRUPT_TYPE_CRTC_VSYNC",
                                (ULONG)notify.InterruptType);
                        }
#endif
                    } else {
                        notify.CrtcVsync.VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
                        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
                    }
                }
            }
        }

        if (any) {
            InterlockedIncrement(&adapter->IrqIsrCount);
        }
    } else {
        const ULONG legacyStatus = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_INT_STATUS);
        const ULONG legacyKnown = AEROGPU_LEGACY_INT_FENCE;
        if ((legacyStatus & AEROGPU_LEGACY_INT_FENCE) == 0) {
            if (legacyStatus != 0) {
                InterlockedIncrement64(&adapter->PerfIrqSpurious);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, legacyStatus);
                static LONG g_UnexpectedLegacyIrqWarned = 0;
                if (InterlockedExchange(&g_UnexpectedLegacyIrqWarned, 1) == 0) {
                    DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                               DPFLTR_ERROR_LEVEL,
                               "aerogpu-kmd: unexpected legacy INT_STATUS bits (status=0x%08lx)\n",
                               legacyStatus);
                }
                any = TRUE;
            }
        } else {
            if ((legacyStatus & ~legacyKnown) != 0) {
                InterlockedIncrement64(&adapter->PerfIrqSpurious);
            }
            InterlockedIncrement64(&adapter->PerfIrqFenceDelivered);
            InterlockedIncrement(&adapter->IrqIsrFenceCount);
            const ULONGLONG completedFence64 =
                (ULONGLONG)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_FENCE_COMPLETED);
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, legacyStatus);

            ULONG completedFence32 = (ULONG)completedFence64;
            const ULONG lastCompleted32 = (ULONG)AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
            const ULONG lastSubmitted32 = (ULONG)AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
            if (completedFence32 < lastCompleted32) {
                completedFence32 = lastCompleted32;
            }
            if (completedFence32 > lastSubmitted32) {
                completedFence32 = lastSubmitted32;
            }

            AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, (ULONGLONG)completedFence32);
            any = TRUE;
            queueDpc = TRUE;

            if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                DXGKARGCB_NOTIFY_INTERRUPT notify;
                RtlZeroMemory(&notify, sizeof(notify));
                notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
                notify.DmaCompleted.SubmissionFenceId = completedFence32;
                notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
            }
        }

        /*
         * Legacy ABI vblank/error interrupts use the newer IRQ_STATUS/IRQ_ENABLE/IRQ_ACK
         * block (if present), even though fence interrupts are still delivered via
         * the legacy INT_STATUS/ACK registers.
         */
        const BOOLEAN haveIrqRegs =
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG));
        if (haveIrqRegs) {
            const ULONG irqStatus = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
            ULONG enableMask = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->IrqEnableMask);
            if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
                /* Prefer the device's IRQ_ENABLE register over the cached mask (see v1 ISR path). */
                enableMask = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
            }
            const ULONG pending = irqStatus & enableMask;

            /*
             * Ack the full IRQ_STATUS word (not just enabled bits) to clear any stale latched status
             * that may have accumulated while delivery was masked (for example vblank). This mirrors
             * the v1 ISR behavior and prevents "stale" interrupts from firing immediately on a later
             * re-enable.
             *
             * IRQ assertion is still defined by (IRQ_STATUS & IRQ_ENABLE) != 0, so we only *claim*
             * the interrupt when `pending != 0` below.
             */
            if (irqStatus != 0) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, irqStatus);
            }

            if (pending != 0) {
                const ULONG known = AEROGPU_IRQ_SCANOUT_VBLANK | AEROGPU_IRQ_ERROR;
                const ULONG unknown = pending & ~known;
                if (unknown != 0) {
                    InterlockedIncrement64(&adapter->PerfIrqSpurious);
                    static LONG g_UnexpectedLegacyMmioIrqWarned = 0;
                    if (InterlockedExchange(&g_UnexpectedLegacyMmioIrqWarned, 1) == 0) {
                        DbgPrintEx(
                            DPFLTR_IHVVIDEO_ID,
                            DPFLTR_ERROR_LEVEL,
                            "aerogpu-kmd: unexpected legacy IRQ_STATUS bits (status=0x%08lx pending=0x%08lx enable=0x%08lx)\n",
                            irqStatus,
                            pending,
                            enableMask);
                    }
                }

                any = TRUE;

                if ((pending & AEROGPU_IRQ_ERROR) != 0) {
                    InterlockedExchange(&adapter->DeviceErrorLatched, 1);
                    /* Legacy device models do not expose structured error MMIO registers; treat as INTERNAL. */
                    InterlockedExchange((volatile LONG*)&adapter->LastErrorCode, (LONG)AEROGPU_ERROR_INTERNAL);
                    InterlockedExchange((volatile LONG*)&adapter->LastErrorMmioCount, 0);
                    AeroGpuAtomicWriteU64(&adapter->LastErrorTime100ns, KeQueryInterruptTime());

                    /*
                     * Mask off further ERROR IRQ generation to avoid storms if the legacy
                     * device model leaves the status bit asserted. This block uses the
                     * versioned IRQ_STATUS/ENABLE/ACK registers.
                     */
                    if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG))) {
                        const ULONG oldEnable =
                            (ULONG)InterlockedAnd((volatile LONG*)&adapter->IrqEnableMask, ~(LONG)AEROGPU_IRQ_ERROR);
                        const ULONG newEnable = oldEnable & ~AEROGPU_IRQ_ERROR;
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, newEnable);
                    }
                    const ULONGLONG completedFence64 = AeroGpuReadCompletedFence(adapter);
                    ULONG completedFence32 = (ULONG)completedFence64;
                    const ULONG lastCompleted32 = (ULONG)AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
                    const ULONG lastSubmitted32 = (ULONG)AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
                    if (completedFence32 < lastCompleted32) {
                        completedFence32 = lastCompleted32;
                    }
                    if (completedFence32 > lastSubmitted32) {
                        completedFence32 = lastSubmitted32;
                    }
                    AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, (ULONGLONG)completedFence32);

                    /* Legacy MMIO ERROR interrupts do not carry a fence completion bit; report the next in-flight fence. */
                    ULONG errorFence32 = completedFence32;
                    if ((errorFence32 < lastSubmitted32) && (errorFence32 != 0xFFFFFFFFu)) {
                        ULONG nextFence = errorFence32 + 1;
                        if (nextFence > lastSubmitted32) {
                            nextFence = lastSubmitted32;
                        }
                        errorFence32 = nextFence;
                    }
                    const ULONGLONG errorFence = (ULONGLONG)errorFence32;
                    AeroGpuAtomicWriteU64(&adapter->LastErrorFence, errorFence);
                    const ULONGLONG n =
                        (ULONGLONG)InterlockedIncrement64((volatile LONGLONG*)&adapter->ErrorIrqCount);

                    BOOLEAN shouldNotify = FALSE;
                    if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                        if (n <= 4 || ((n & (n - 1)) == 0)) {
                            const ULONGLONG prevNotified = AeroGpuAtomicExchangeU64(&adapter->LastNotifiedErrorFence, errorFence);
                            if (prevNotified != errorFence) {
                                shouldNotify = TRUE;
                            }
                        }
                    }

                    if (shouldNotify && adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
                        DXGKARGCB_NOTIFY_INTERRUPT notify;
                        RtlZeroMemory(&notify, sizeof(notify));
                        notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_FAULTED;
                        notify.DmaFaulted.FaultedFenceId = (ULONG)errorFence;
                        notify.DmaFaulted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
                        notify.DmaFaulted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
                        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
                    }

#if DBG
                    if (n <= 4 || ((n & (n - 1)) == 0)) {
                        DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                                   DPFLTR_ERROR_LEVEL,
                                   "aerogpu-kmd: legacy device IRQ error (IRQ_STATUS=0x%08lx fence=%lu count=%I64u)\n",
                                   irqStatus,
                                   (ULONG)errorFence,
                                   (unsigned long long)n);
                    }
#endif

                    queueDpc = TRUE;
                }

                if ((pending & AEROGPU_IRQ_SCANOUT_VBLANK) != 0 && adapter->SupportsVblank) {
                    InterlockedIncrement64(&adapter->PerfIrqVblankDelivered);
                    InterlockedIncrement(&adapter->IrqIsrVblankCount);
                    const BOOLEAN haveVblankRegs =
                        adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG));
                    if (!haveVblankRegs) {
#if DBG
                        static LONG g_LegacyVblankRegsMissingWarned = 0;
                        if (InterlockedExchange(&g_LegacyVblankRegsMissingWarned, 1) == 0) {
                            DbgPrintEx(DPFLTR_IHVVIDEO_ID,
                                       DPFLTR_ERROR_LEVEL,
                                       "aerogpu-kmd: legacy device signaled vblank IRQ but BAR0 lacks vblank timing regs; ignoring\n");
                        }
#endif
                    } else {
                        const ULONGLONG now100ns = KeQueryInterruptTime();
                        const ULONGLONG seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                                     AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                                     AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
                        const ULONGLONG timeNs = AeroGpuReadRegU64HiLoHi(adapter,
                                                                        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                                        AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
                        const ULONG periodNs = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
                        if (periodNs != 0) {
                            adapter->VblankPeriodNs = periodNs;
                        }
                        AeroGpuAtomicWriteU64(&adapter->LastVblankSeq, seq);
                        AeroGpuAtomicWriteU64(&adapter->LastVblankTimeNs, timeNs);
                        AeroGpuAtomicWriteU64(&adapter->LastVblankInterruptTime100ns, now100ns);

                        queueDpc = TRUE;

                        if (adapter->DxgkInterface.DxgkCbNotifyInterrupt && adapter->VblankInterruptTypeValid) {
                            KeMemoryBarrier();
                            const DXGK_INTERRUPT_TYPE vblankType = adapter->VblankInterruptType;

                            DXGKARGCB_NOTIFY_INTERRUPT notify;
                            RtlZeroMemory(&notify, sizeof(notify));
                            notify.InterruptType = vblankType;

                            /*
                             * ABI-critical: for DXGK_INTERRUPT_TYPE_CRTC_VSYNC, dxgkrnl expects
                             * DXGKARGCB_NOTIFY_INTERRUPT.CrtcVsync.VidPnSourceId to identify the
                             * VidPn source that vblanked.
                             */
                            if (notify.InterruptType != DXGK_INTERRUPT_TYPE_CRTC_VSYNC) {
#if DBG
                                static volatile LONG g_UnexpectedLegacyVblankNotifyTypeLogs = 0;
                                const LONG n = InterlockedIncrement(&g_UnexpectedLegacyVblankNotifyTypeLogs);
                                if ((n <= 8) || ((n & 1023) == 0)) {
                                    AEROGPU_LOG(
                                        "InterruptRoutine: legacy vblank uses unexpected InterruptType=%lu; expected DXGK_INTERRUPT_TYPE_CRTC_VSYNC",
                                        (ULONG)notify.InterruptType);
                                }
#endif
                            } else {
                                notify.CrtcVsync.VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
                                adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
                            }
                        }
                    }
                }
            }
        }

        if (any) {
            InterlockedIncrement(&adapter->IrqIsrCount);
        }
    }

    if (queueDpc && adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    return any;
}

static VOID APIENTRY AeroGpuDdiDpcRoutine(_In_ const PVOID MiniportDeviceContext)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)MiniportDeviceContext;
    if (!adapter) {
        return;
    }

    InterlockedIncrement(&adapter->IrqDpcCount);

    if (adapter->DxgkInterface.DxgkCbNotifyDpc) {
        adapter->DxgkInterface.DxgkCbNotifyDpc(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuRetireSubmissionsUpToFence(adapter, AeroGpuAtomicReadU64(&adapter->LastCompletedFence));
    AeroGpuCleanupInternalSubmissions(adapter);
}

static __forceinline BOOLEAN AeroGpuIsVblankControlInterruptType(_In_ DXGK_INTERRUPT_TYPE InterruptType)
{
    /*
     * Win7 WDDM 1.1 uses DXGK_INTERRUPT_TYPE_CRTC_VSYNC for vblank/vsync control
     * and delivery (see file header comment).
     */
    return (InterruptType == DXGK_INTERRUPT_TYPE_CRTC_VSYNC);
}

#if DBG
static __forceinline BOOLEAN AeroGpuShouldLogUnexpectedControlInterruptType()
{
    /*
     * Dxgkrnl can call DxgkDdiControlInterrupt repeatedly (per waiter, per
     * modeset, etc). Keep unexpected-type logging rate-limited so a misbehaving
     * guest doesn't spam the kernel debugger.
     *
     * Log:
     *  - the first handful of occurrences, then
     *  - every ~1024th call thereafter.
     */
    static volatile LONG g_UnexpectedControlInterruptTypeLogs = 0;
    const LONG n = InterlockedIncrement(&g_UnexpectedControlInterruptTypeLogs);
    return (n <= 8) || ((n & 1023) == 0);
}
#endif

static NTSTATUS APIENTRY AeroGpuDdiControlInterrupt(_In_ const HANDLE hAdapter,
                                                    _In_ const DXGK_INTERRUPT_TYPE InterruptType,
                                                    _In_ BOOLEAN EnableInterrupt)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }
    if (!adapter->Bar0) {
        /* Be tolerant of dxgkrnl calling ControlInterrupt during teardown. */
        return STATUS_SUCCESS;
    }

    const BOOLEAN poweredOn =
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);
    const BOOLEAN accepting = (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
    /*
     * Once the device has asserted IRQ_ERROR, never re-enable ERROR delivery.
     *
     * Do not fail the ControlInterrupt callback itself: dxgkrnl may call it as
     * part of teardown/recovery paths. Submission paths already fail fast with
     * STATUS_GRAPHICS_DEVICE_REMOVED to surface device-lost semantics.
     */

    /* Fence/DMA completion interrupt gating. */
    if (InterruptType == DXGK_INTERRUPT_TYPE_DMA_COMPLETED) {
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            /* Legacy ABI does not expose an INTx enable mask for fence interrupts. */
            return STATUS_SUCCESS;
        }
        const BOOLEAN haveIrqRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG));
        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
            ULONG enable = adapter->IrqEnableMask;
            if (EnableInterrupt) {
                enable |= AEROGPU_IRQ_FENCE;
            } else {
                enable &= ~AEROGPU_IRQ_FENCE;
            }
            if (AeroGpuIsDeviceErrorLatched(adapter)) {
                /* Never re-enable ERROR delivery once an IRQ_ERROR has been observed. */
                enable &= ~AEROGPU_IRQ_ERROR;
            }
            adapter->IrqEnableMask = enable;
            if (poweredOn && accepting && haveIrqRegs) {
                /*
                 * Only unmask device IRQ generation when we have successfully registered an ISR
                 * with dxgkrnl. If RegisterInterrupt failed, leaving IRQ_ENABLE non-zero can
                 * trigger an unhandled interrupt storm.
                 */
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                if (!EnableInterrupt) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_FENCE);
                }
                /*
                 * Race hardening: an IRQ_ERROR can be latched in the ISR while we hold IrqEnableLock
                 * (DIRQL preempts DISPATCH_LEVEL). If that happens between the latch check above and
                 * this IRQ_ENABLE programming, we may have re-enabled ERROR delivery. Re-check and
                 * force ERROR masked if the device is now in a latched error state.
                 */
                if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                    enable &= ~AEROGPU_IRQ_ERROR;
                    adapter->IrqEnableMask = enable;
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                }
            }
            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }
        return STATUS_SUCCESS;
    }

    /* VBlank / vsync interrupt gating. */
    if (AeroGpuIsVblankControlInterruptType(InterruptType)) {
        if (!adapter->SupportsVblank) {
            return STATUS_NOT_SUPPORTED;
        }
        if (adapter->Bar0Length < (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
            return STATUS_NOT_SUPPORTED;
        }

        /*
         * Record the vblank interrupt type that dxgkrnl expects.
         *
         * Note: dxgkrnl may call ControlInterrupt during initialization to
         * disable the interrupt before ever enabling it. Treat that as a no-op.
         */
        if (!adapter->VblankInterruptTypeValid) {
            if (!EnableInterrupt) {
                return STATUS_SUCCESS;
            }
            adapter->VblankInterruptType = InterruptType;
            KeMemoryBarrier();
            adapter->VblankInterruptTypeValid = TRUE;
        } else if (adapter->VblankInterruptType != InterruptType) {
            return STATUS_NOT_SUPPORTED;
        }

        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);

            ULONG enable = adapter->IrqEnableMask;

            /*
             * Clear any pending vblank status before enabling delivery.
             *
             * Some device models may latch the vblank status bit even while the
             * IRQ is masked; without this defensive ACK, a later enable could
             * trigger an immediate "stale" interrupt and break
             * D3DKMTWaitForVerticalBlankEvent pacing.
             *
             * Only clear the bit when transitioning from disabled -> enabled to
             * avoid dropping an in-flight vblank interrupt if dxgkrnl calls
             * EnableInterrupt repeatedly.
             */
            if (EnableInterrupt && (enable & AEROGPU_IRQ_SCANOUT_VBLANK) == 0) {
                if (poweredOn && accepting) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
                }
            }

            if (EnableInterrupt) {
                enable |= AEROGPU_IRQ_SCANOUT_VBLANK;
            } else {
                enable &= ~AEROGPU_IRQ_SCANOUT_VBLANK;
            }
            if (AeroGpuIsDeviceErrorLatched(adapter)) {
                /* Never re-enable ERROR delivery once an IRQ_ERROR has been observed. */
                enable &= ~AEROGPU_IRQ_ERROR;
            }
            adapter->IrqEnableMask = enable;
            if (poweredOn && accepting) {
                /*
                 * Only unmask device IRQ generation when we have successfully registered an ISR
                 * with dxgkrnl. This mirrors StartDevice and avoids unhandled interrupt storms.
                 */
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                /*
                 * Same race hardening as DMA_COMPLETED: if an IRQ_ERROR was latched while we held
                 * IrqEnableLock, ensure we did not re-enable ERROR delivery.
                 */
                if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                    enable &= ~AEROGPU_IRQ_ERROR;
                    adapter->IrqEnableMask = enable;
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
                }
            }

            /* Be robust against stale pending bits when disabling. */
            if (!EnableInterrupt) {
                if (poweredOn && accepting) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
                }
            }

            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
        }

        return STATUS_SUCCESS;
    }

#if DBG
    if (AeroGpuShouldLogUnexpectedControlInterruptType()) {
        AEROGPU_LOG("ControlInterrupt: unsupported InterruptType=%lu EnableInterrupt=%lu",
                    (ULONG)InterruptType,
                    EnableInterrupt ? 1ul : 0ul);
    }
#endif

    return STATUS_NOT_SUPPORTED;
}

static NTSTATUS APIENTRY AeroGpuDdiResetFromTimeout(_In_ const HANDLE hAdapter)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * Block new submissions while we are tearing down/resetting ring state. WDDM is expected to
     * quiesce scheduling around the TDR path, but be defensive against concurrent SubmitCommand
     * calls that could race with our pending-list cleanup.
     *
     * We re-enable submissions in DxgkDdiRestartFromTimeout once the device is back in a known
     * good state.
     */
    InterlockedExchange(&adapter->AcceptingSubmissions, 0);

    /* dbgctl perf counters: record resets (TDR recovery path). */
    InterlockedIncrement64(&adapter->PerfResetFromTimeoutCount);
    InterlockedExchange64(&adapter->PerfLastResetTime100ns, (LONGLONG)KeQueryInterruptTime());

    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);

    /*
     * Keep recovery simple: clear the ring pointers and treat all in-flight
     * work as completed to unblock dxgkrnl. A well-behaved emulator should not
     * require this path under normal usage.
     */
    if (poweredOn && adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG))) {
        /*
         * Disable IRQs while resetting ring state so we don't race ISR/DPC paths
         * with partially-reset bookkeeping.
         */
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
    }

    /*
     * Detach the pending submission list under PendingLock so we can free it
     * without racing concurrent SubmitCommand calls.
     */
    LIST_ENTRY pendingToFree;
    InitializeListHead(&pendingToFree);
    LIST_ENTRY internalToFree;
    InitializeListHead(&internalToFree);

    ULONGLONG completedFence = 0;
    {
        KIRQL pendingIrql;
        KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);

        completedFence = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
        AeroGpuAtomicWriteU64(&adapter->LastCompletedFence, completedFence);

        if (adapter->RingVa) {
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                if (adapter->RingSizeBytes >= sizeof(struct aerogpu_ring_header)) {
                    /*
                     * The ring header lives at the start of the ring mapping. Use RingVa directly
                     * instead of trusting the cached RingHeader pointer during recovery paths.
                     */
                    adapter->RingHeader = (struct aerogpu_ring_header*)adapter->RingVa;
                    const ULONG tail = adapter->RingTail;
                    adapter->RingHeader->head = tail;
                    adapter->RingHeader->tail = tail;
                    KeMemoryBarrier();
                } else {
                    adapter->RingHeader = NULL;
                }

                if (poweredOn && adapter->Bar0Length >= (AEROGPU_MMIO_REG_RING_CONTROL + sizeof(ULONG))) {
                    AeroGpuWriteRegU32(adapter,
                                       AEROGPU_MMIO_REG_RING_CONTROL,
                                       AEROGPU_RING_CONTROL_ENABLE | AEROGPU_RING_CONTROL_RESET);
                }
            } else {
                adapter->RingTail = 0;
                adapter->LegacyRingHeadIndex = 0;
                adapter->LegacyRingHeadSeq = 0;
                adapter->LegacyRingTailSeq = 0;
                if (poweredOn) {
                    if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_TAIL + sizeof(ULONG))) {
                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
                    }
                    if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_INT_ACK + sizeof(ULONG))) {
                        AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
                    }
                }
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }

        while (!IsListEmpty(&adapter->PendingSubmissions)) {
            InsertTailList(&pendingToFree, RemoveHeadList(&adapter->PendingSubmissions));
        }
        while (!IsListEmpty(&adapter->RecentSubmissions)) {
            InsertTailList(&pendingToFree, RemoveHeadList(&adapter->RecentSubmissions));
        }
        adapter->RecentSubmissionCount = 0;
        adapter->RecentSubmissionBytes = 0;
        while (!IsListEmpty(&adapter->PendingInternalSubmissions)) {
            InsertTailList(&internalToFree, RemoveHeadList(&adapter->PendingInternalSubmissions));
        }

        KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
    }

    /*
     * Keep device IRQ generation disabled until DxgkDdiRestartFromTimeout.
     *
     * DxgkDdiResetFromTimeout runs while the OS is resetting scheduling state; enabling the
     * device's level-triggered interrupt line here can create interrupt storms or stale pending
     * bits before RestartFromTimeout has restored a consistent ring/MMIO configuration.
     */

    if (adapter->DxgkInterface.DxgkCbNotifyInterrupt) {
        DXGKARGCB_NOTIFY_INTERRUPT notify;
        RtlZeroMemory(&notify, sizeof(notify));
        notify.InterruptType = DXGK_INTERRUPT_TYPE_DMA_COMPLETED;
        notify.DmaCompleted.SubmissionFenceId = (ULONG)completedFence;
        notify.DmaCompleted.NodeOrdinal = AEROGPU_NODE_ORDINAL;
        notify.DmaCompleted.EngineOrdinal = AEROGPU_ENGINE_ORDINAL;
        adapter->DxgkInterface.DxgkCbNotifyInterrupt(adapter->StartInfo.hDxgkHandle, &notify);
    }

    if (adapter->DxgkInterface.DxgkCbQueueDpcForIsr) {
        adapter->DxgkInterface.DxgkCbQueueDpcForIsr(adapter->StartInfo.hDxgkHandle);
    }

    AeroGpuMetaHandleFreeAll(adapter);
    while (!IsListEmpty(&pendingToFree)) {
        PLIST_ENTRY entry = RemoveHeadList(&pendingToFree);
        AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
        AeroGpuFreeSubmission(adapter, sub);
    }
    while (!IsListEmpty(&internalToFree)) {
        PLIST_ENTRY entry = RemoveHeadList(&internalToFree);
        AEROGPU_PENDING_INTERNAL_SUBMISSION* sub =
            CONTAINING_RECORD(entry, AEROGPU_PENDING_INTERNAL_SUBMISSION, ListEntry);
        AeroGpuFreeInternalSubmission(adapter, sub);
    }

    /* Reset/teardown path: do not retain pooled contiguous allocations across TDR recovery. */
    AeroGpuContigPoolPurge(adapter);
    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiRestartFromTimeout(_In_ const HANDLE hAdapter)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    /*
     * dxgkrnl calls DxgkDdiRestartFromTimeout after DxgkDdiResetFromTimeout. The intent is to
     * restore the device to a known-good state that can accept new submissions without requiring
     * a full device restart.
     *
     * This is a best-effort restart routine; be defensive and tolerate calls when BAR0/ring
     * state is partially initialised (e.g. during teardown or failed start paths).
     */

    /* Ensure submission paths are blocked while we rebuild ring/MMIO state. */
    InterlockedExchange(&adapter->AcceptingSubmissions, 0);

    /* Clear any KMD-side latched "device error" state recorded from IRQ_ERROR. */
    InterlockedExchange(&adapter->DeviceErrorLatched, 0);
    /* Allow future IRQ_ERROR notifications even if fence IDs repeat after TDR. */
    AeroGpuAtomicWriteU64(&adapter->LastNotifiedErrorFence, (ULONGLONG)(LONGLONG)-1);

    if (!adapter->Bar0) {
        return STATUS_SUCCESS;
    }
    if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) != DxgkDevicePowerStateD0) {
        /* Avoid touching MMIO while the device is in a non-D0 state. */
        return STATUS_SUCCESS;
    }

    const BOOLEAN haveIrqRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG));
    const BOOLEAN haveIrqEnable = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG));

    /*
     * Disable IRQ generation while we repair ring/programming state so ISR/DPC paths never see a
     * partially-restored configuration.
     */
    if (haveIrqEnable) {
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);
    }
    if (haveIrqRegs) {
        /* Clear any stale pending status, including AEROGPU_IRQ_ERROR. */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
    }

    /* Drop any stale vblank anchor state so GetScanLine recalibrates after recovery. */
    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankSeq, 0);
    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankTimeNs, 0);
    InterlockedExchange64((volatile LONGLONG*)&adapter->LastVblankInterruptTime100ns, 0);
    adapter->VblankPeriodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;

    if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
        const BOOLEAN haveRingRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_RING_CONTROL + sizeof(ULONG));
        const BOOLEAN haveFenceRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_FENCE_GPA_HI + sizeof(ULONG));

        {
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            const ULONG ringEntryCount = adapter->RingEntryCount;
            const BOOLEAN ringEntryCountPow2 =
                (ringEntryCount != 0 && (ringEntryCount & (ringEntryCount - 1)) == 0) ? TRUE : FALSE;
            const ULONGLONG ringMinBytes =
                (ULONGLONG)sizeof(struct aerogpu_ring_header) + ((ULONGLONG)ringEntryCount * (ULONGLONG)sizeof(struct aerogpu_submit_desc));
            const BOOLEAN ringSizeOk = (ringMinBytes <= (ULONGLONG)adapter->RingSizeBytes) ? TRUE : FALSE;
            const BOOLEAN haveRing = (adapter->RingVa && ringEntryCountPow2 && ringSizeOk) ? TRUE : FALSE;
            if (!haveRing) {
                adapter->RingHeader = NULL;
            }

            if (haveRing && adapter->RingSizeBytes >= sizeof(struct aerogpu_ring_header)) {
                /* Ring header lives at the start of the ring mapping. */
                adapter->RingHeader = (struct aerogpu_ring_header*)adapter->RingVa;

                /*
                 * Re-initialise the ring header "static" fields in case the device/guest clobbered
                 * them while wedged. This is safe because the ring has been drained/reset in
                 * ResetFromTimeout and we are about to resync head/tail.
                 */
                adapter->RingHeader->magic = AEROGPU_RING_MAGIC;
                adapter->RingHeader->abi_version = AEROGPU_ABI_VERSION_U32;
                adapter->RingHeader->size_bytes = (uint32_t)adapter->RingSizeBytes;
                adapter->RingHeader->entry_count = (uint32_t)adapter->RingEntryCount;
                adapter->RingHeader->entry_stride_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
                adapter->RingHeader->flags = 0;

                const ULONG tail = adapter->RingTail;
                adapter->RingHeader->head = tail;
                adapter->RingHeader->tail = tail;
                KeMemoryBarrier();
            }

            if (haveRingRegs) {
                if (haveRing) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, adapter->RingPa.LowPart);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, (ULONG)(adapter->RingPa.QuadPart >> 32));
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, adapter->RingSizeBytes);

                    if (haveFenceRegs) {
                        if (adapter->FencePageVa && ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_FENCE_PAGE) != 0)) {
                            /*
                             * Reinitialise the fence page header + completed fence to a sensible
                             * value before reprogramming the device-visible GPA.
                             */
                            adapter->FencePageVa->magic = AEROGPU_FENCE_PAGE_MAGIC;
                            adapter->FencePageVa->abi_version = AEROGPU_ABI_VERSION_U32;
                            AeroGpuAtomicWriteU64((volatile ULONGLONG*)&adapter->FencePageVa->completed_fence,
                                                  AeroGpuAtomicReadU64(&adapter->LastCompletedFence));
                            KeMemoryBarrier();

                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, adapter->FencePagePa.LowPart);
                            AeroGpuWriteRegU32(adapter,
                                               AEROGPU_MMIO_REG_FENCE_GPA_HI,
                                               (ULONG)(adapter->FencePagePa.QuadPart >> 32));
                        } else {
                            /* Ensure the device will not DMA to an uninitialised fence page. */
                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);
                        }
                    }

                    /* Ensure the ring is enabled post-reset. */
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, AEROGPU_RING_CONTROL_ENABLE);
                } else {
                    /* Defensive: disable ring execution to prevent DMA from stale/uninitialised pointers. */
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_CONTROL, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_LO, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_GPA_HI, 0);
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_RING_SIZE_BYTES, 0);
                    if (haveFenceRegs) {
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_LO, 0);
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_FENCE_GPA_HI, 0);
                    }
                }
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }
    } else {
        /*
         * Legacy ABI: re-program ring base/size registers in case the device reset cleared them.
         * Fence interrupts are delivered via legacy INT_STATUS/ACK (no enable mask), but some
         * legacy device models also expose the newer IRQ_STATUS/ENABLE/ACK block for vblank.
         */
        if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_INT_ACK + sizeof(ULONG))) {
            AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_INT_ACK, 0xFFFFFFFFu);
        }

        if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_DOORBELL + sizeof(ULONG))) {
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            BOOLEAN ringOk = FALSE;
            if (adapter->RingVa && adapter->RingEntryCount != 0) {
                const ULONGLONG minRingBytes =
                    (ULONGLONG)adapter->RingEntryCount * (ULONGLONG)sizeof(aerogpu_legacy_ring_entry);
                ringOk = (minRingBytes <= (ULONGLONG)adapter->RingSizeBytes) ? TRUE : FALSE;
            }

            if (ringOk) {
                if (adapter->RingTail >= adapter->RingEntryCount) {
                    adapter->RingTail = 0;
                }
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, adapter->RingPa.LowPart);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, (ULONG)(adapter->RingPa.QuadPart >> 32));
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, adapter->RingEntryCount);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, adapter->RingTail);
            } else {
                /* Defensive: disable ring execution to prevent DMA from stale/uninitialised pointers. */
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_ENTRY_COUNT, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_LO, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_BASE_HI, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, 0);
                adapter->RingTail = 0;
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }
    }

    /*
     * Re-enable interrupt delivery through dxgkrnl (it may have been disabled during TDR).
     * Do this before unmasking device IRQ generation so any immediately-pending IRQ is handled.
     */
    if (adapter->InterruptRegistered && adapter->DxgkInterface.DxgkCbEnableInterrupt) {
        adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
    }

    /* Restore the device IRQ enable mask to the cached value. */
    if (haveIrqEnable) {
        KIRQL irqIrql;
        KeAcquireSpinLock(&adapter->IrqEnableLock, &irqIrql);

        ULONG enable = adapter->IrqEnableMask;
        if (adapter->InterruptRegistered) {
            /*
             * Ensure baseline IRQ delivery is restored post-restart.
             *
             * - ERROR: some device models latch ERROR as a level-triggered interrupt; the ISR masks
             *   it off to avoid storms. RestartFromTimeout clears DeviceErrorLatched so we must
             *   re-enable ERROR delivery for future diagnostics.
             * - FENCE: required for forward progress on the v1 ABI; legacy devices deliver fences
             *   via INT_STATUS/ACK and do not use IRQ_ENABLE for fence completion.
             */
            enable |= AEROGPU_IRQ_ERROR;
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                enable |= AEROGPU_IRQ_FENCE;
            }
        }

        adapter->IrqEnableMask = enable;
        /*
         * Only unmask the device interrupt line when we have successfully registered an ISR with
         * dxgkrnl. This mirrors StartDevice: if RegisterInterrupt failed, enabling the device IRQ
         * mask can create an interrupt storm that the OS cannot route back to this miniport.
         */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, adapter->InterruptRegistered ? enable : 0);
        KeReleaseSpinLock(&adapter->IrqEnableLock, irqIrql);

        if (haveIrqRegs) {
            /* Drop any stale pending bits that may have latched while masked. */
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, 0xFFFFFFFFu);
        }
    }

    /*
     * Best-effort: reapply scanout/cursor programming after the restart.
     *
     * The emulator device model keeps scanout state across ring resets today, but a real device
     * (or future emulator versions) may drop mode/scanout/cursor registers when the backend is
     * wedged and recovers. Restoring these registers helps the desktop remain visible post-TDR.
     */
    if (!adapter->PostDisplayOwnershipReleased) {
        /*
         * Guard against partial BAR0 mappings: AeroGpuProgramScanout assumes the
         * relevant scanout register block exists.
         */
        if ((adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
            AeroGpuProgramScanout(adapter, adapter->CurrentScanoutFbPa);
        } else if (!(adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) &&
                   adapter->Bar0Length >= (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
            AeroGpuProgramScanout(adapter, adapter->CurrentScanoutFbPa);
        }

        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0 &&
            adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
            BOOLEAN cursorShapeValid = FALSE;
            BOOLEAN cursorVisible = FALSE;
            LONG cursorX = 0;
            LONG cursorY = 0;
            ULONG cursorHotX = 0;
            ULONG cursorHotY = 0;
            ULONG cursorWidth = 0;
            ULONG cursorHeight = 0;
            ULONG cursorFormat = 0;
            ULONG cursorPitchBytes = 0;
            PVOID cursorVa = NULL;
            PHYSICAL_ADDRESS cursorPa;
            SIZE_T cursorSizeBytes = 0;
            cursorPa.QuadPart = 0;

            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                cursorShapeValid = adapter->CursorShapeValid;
                cursorVisible = adapter->CursorVisible;
                cursorX = adapter->CursorX;
                cursorY = adapter->CursorY;
                cursorHotX = adapter->CursorHotX;
                cursorHotY = adapter->CursorHotY;
                cursorWidth = adapter->CursorWidth;
                cursorHeight = adapter->CursorHeight;
                cursorFormat = adapter->CursorFormat;
                cursorPitchBytes = adapter->CursorPitchBytes;
                cursorVa = adapter->CursorFbVa;
                cursorPa = adapter->CursorFbPa;
                cursorSizeBytes = adapter->CursorFbSizeBytes;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }

            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            if (cursorShapeValid && cursorVa && cursorSizeBytes != 0) {
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, (ULONG)cursorX);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, (ULONG)cursorY);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X, cursorHotX);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y, cursorHotY);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, cursorWidth);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, cursorHeight);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, cursorFormat);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, cursorPitchBytes);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, cursorPa.LowPart);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, (ULONG)(cursorPa.QuadPart >> 32));
                KeMemoryBarrier();
                AeroGpuWriteRegU32(adapter,
                                   AEROGPU_MMIO_REG_CURSOR_ENABLE,
                                   (cursorVisible && cursorShapeValid) ? 1u : 0u);
            } else {
                /* Ensure the device does not DMA from a stale cursor GPA. */
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, 0);
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, 0);
            }
        }
    }

    BOOLEAN ringReady = FALSE;
    {
        /*
         * AeroGpu*SubmitPathUsable reads ring header fields; take RingLock so we don't race
         * AeroGpuRingCleanup during teardown.
         */
        KIRQL ringIrql;
        KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            ringReady = AeroGpuV1SubmitPathUsable(adapter);
        } else {
            ringReady = AeroGpuLegacySubmitPathUsable(adapter);
        }
        KeReleaseSpinLock(&adapter->RingLock, ringIrql);
    }
    if (ringReady) {
        /* Ensure the submission paths are unblocked once the restart has restored ring/MMIO state. */
        InterlockedExchange(&adapter->AcceptingSubmissions, 1);
    }

    return STATUS_SUCCESS;
}

static BOOLEAN AeroGpuCursorMmioUsable(_In_ const AEROGPU_ADAPTER* Adapter)
{
    if (!Adapter || !Adapter->Bar0) {
        return FALSE;
    }

    if ((Adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) == 0) {
        return FALSE;
    }

    /*
     * Cursor registers live at fixed offsets in the versioned MMIO map. Some legacy
     * bring-up models may expose FEATURE bits but not a full 64 KiB BAR. Guard
     * against out-of-bounds MMIO.
     */
    if (Adapter->Bar0Length < (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
        return FALSE;
    }

    return TRUE;
}

static VOID AeroGpuCursorDisable(_Inout_ AEROGPU_ADAPTER* Adapter)
{
    if (!AeroGpuCursorMmioUsable(Adapter)) {
        return;
    }

    AeroGpuWriteRegU32(Adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
}

static NTSTATUS APIENTRY AeroGpuDdiSetPointerPosition(_In_ const HANDLE hAdapter,
                                                     _In_ const DXGKARG_SETPOINTERPOSITION* pPos)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pPos) {
        return STATUS_INVALID_PARAMETER;
    }

    if (pPos->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    BOOLEAN cursorVisible = FALSE;
    BOOLEAN cursorShapeValid = FALSE;
    LONG cursorX = 0;
    LONG cursorY = 0;
    {
        KIRQL cursorIrql;
        KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
        adapter->CursorVisible = pPos->Visible ? TRUE : FALSE;
        adapter->CursorX = pPos->X;
        adapter->CursorY = pPos->Y;
        cursorVisible = adapter->CursorVisible;
        cursorShapeValid = adapter->CursorShapeValid;
        cursorX = adapter->CursorX;
        cursorY = adapter->CursorY;
        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
    }

    if (!adapter->Bar0) {
        /* Be tolerant of pointer calls during early init or teardown. */
        return STATUS_SUCCESS;
    }

    if (adapter->Bar0Length < (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
        return STATUS_NOT_SUPPORTED;
    }

    const BOOLEAN poweredOn =
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0) ? TRUE : FALSE;
    if (!poweredOn) {
        /*
         * Cache cursor state but avoid touching MMIO while the adapter is not in
         * D0. Cursor registers will be restored in DxgkDdiSetPowerState.
         */
        return STATUS_SUCCESS;
    }

    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, (ULONG)cursorX);
    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, (ULONG)cursorY);
    AeroGpuWriteRegU32(adapter,
                       AEROGPU_MMIO_REG_CURSOR_ENABLE,
                       (cursorVisible && cursorShapeValid && !adapter->PostDisplayOwnershipReleased) ? 1u : 0u);

    return STATUS_SUCCESS;
}

static NTSTATUS APIENTRY AeroGpuDdiSetPointerShape(_In_ const HANDLE hAdapter,
                                                  _In_ const DXGKARG_SETPOINTERSHAPE* pShape)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter) {
        return STATUS_INVALID_PARAMETER;
    }

    if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) == 0) {
        /*
         * Prefer a hard NOT_SUPPORTED so dxgkrnl falls back to software cursor
         * composition instead of assuming hardware cursor state is applied.
         */
        return STATUS_NOT_SUPPORTED;
    }

    if (!adapter->Bar0) {
        return STATUS_DEVICE_NOT_READY;
    }

    if (adapter->Bar0Length < (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG))) {
        return STATUS_NOT_SUPPORTED;
    }

    const BOOLEAN poweredOn =
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0) ? TRUE : FALSE;

    /* Defensive: treat null shape as "disable hardware cursor". */
    if (!pShape) {
        if (poweredOn) {
            AeroGpuCursorDisable(adapter);
        }
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            adapter->CursorShapeValid = FALSE;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
        return STATUS_SUCCESS;
    }

    if (pShape->VidPnSourceId != AEROGPU_VIDPN_SOURCE_ID) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!pShape->pPixels || pShape->Width == 0 || pShape->Height == 0) {
        if (poweredOn) {
            AeroGpuCursorDisable(adapter);
        }
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            adapter->CursorShapeValid = FALSE;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
        return STATUS_SUCCESS;
    }

    const DXGK_POINTERFLAGS flags = pShape->Flags;

    const ULONG width = pShape->Width;
    ULONG height = pShape->Height;
    const ULONG heightIn = height;

    /*
     * WDDM contract: for monochrome pointers, `pPixels` contains an AND mask followed
     * by an XOR mask, each `height` rows. The incoming `Height` is the total mask
     * height (2 * cursor_height). Convert it to the actual cursor height before
     * sizing allocations and programming the device.
     */
    if (flags.Monochrome) {
        if ((height & 1u) != 0) {
            return STATUS_INVALID_PARAMETER;
        }
        height >>= 1;
        if (height == 0) {
            return STATUS_INVALID_PARAMETER;
        }
    }

    /* Sanity cap to avoid runaway allocations on malformed inputs. */
    if (width > 512u || height > 512u) {
        return STATUS_INVALID_PARAMETER;
    }

    /* We only implement 32bpp cursor formats in the MVP. */
    if (width > (0xFFFFFFFFu / 4u)) {
        return STATUS_INVALID_PARAMETER;
    }

    const ULONG dstPitchBytes = width * 4u;

    const ULONGLONG size64 = (ULONGLONG)dstPitchBytes * (ULONGLONG)height;
    if (dstPitchBytes != 0 && (size64 / dstPitchBytes) != height) {
        return STATUS_INVALID_PARAMETER;
    }

    /* Size must be representable as SIZE_T for MmAllocateContiguousMemory*. */
    if (size64 == 0 || size64 > (ULONGLONG)(SIZE_T)~0ULL) {
        return STATUS_INVALID_PARAMETER;
    }

    const SIZE_T requiredBytes = (SIZE_T)size64;

    /* Cursor is small; keep an additional hard cap for safety (1 MiB). */
    if (requiredBytes > (SIZE_T)(1024u * 1024u)) {
        return STATUS_INVALID_PARAMETER;
    }

    PVOID cursorFbVa = NULL;
    PHYSICAL_ADDRESS cursorFbPa;
    SIZE_T cursorFbSizeBytes = 0;
    cursorFbPa.QuadPart = 0;

    {
        KIRQL cursorIrql;
        KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
        cursorFbVa = adapter->CursorFbVa;
        cursorFbPa = adapter->CursorFbPa;
        cursorFbSizeBytes = adapter->CursorFbSizeBytes;
        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
    }

    if (!cursorFbVa || cursorFbSizeBytes < requiredBytes) {
        if (poweredOn) {
            AeroGpuCursorDisable(adapter);
        }

        /* Detach the old cursor buffer under the cursor lock before freeing. */
        PVOID oldVa = NULL;
        SIZE_T oldSize = 0;
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            oldVa = adapter->CursorFbVa;
            oldSize = adapter->CursorFbSizeBytes;
            adapter->CursorFbVa = NULL;
            adapter->CursorFbPa.QuadPart = 0;
            adapter->CursorFbSizeBytes = 0;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
        AeroGpuFreeContiguousNonCached(adapter, oldVa, oldSize);

        cursorFbVa = AeroGpuAllocContiguous(adapter, requiredBytes, &cursorFbPa);
        if (!cursorFbVa) {
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        cursorFbSizeBytes = requiredBytes;

        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            adapter->CursorFbVa = cursorFbVa;
            adapter->CursorFbPa = cursorFbPa;
            adapter->CursorFbSizeBytes = cursorFbSizeBytes;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
    } else {
        /* Ensure deterministic contents even when reusing a larger buffer. */
        RtlZeroMemory(cursorFbVa, cursorFbSizeBytes);
    }

    ULONG hotX = pShape->XHot;
    ULONG hotY = pShape->YHot;
    if (hotX >= width) {
        hotX = width ? (width - 1) : 0;
    }
    if (hotY >= height) {
        hotY = height ? (height - 1) : 0;
    }

    ULONG format = AEROGPU_FORMAT_B8G8R8A8_UNORM;

    /*
     * Cursor shape encoding:
     * - Color / masked color: 32bpp pixels in A8R8G8B8 or X8R8G8B8 (little-endian BGRA/BGRX;
     *   X8 formats do not carry alpha and are treated as fully opaque for display).
     * - Monochrome: AND mask + XOR mask, each 1bpp, stacked vertically (classic Windows cursor encoding).
     *
     * We always write a 32bpp BGRA/BGRX cursor into the protocol cursor framebuffer and program
     * CURSOR_FORMAT accordingly.
     */
    if (flags.Monochrome) {
        const ULONG srcPitch = pShape->Pitch;
        if (srcPitch == 0) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const ULONG minMaskPitch = (width + 7u) / 8u;
        if (srcPitch < minMaskPitch) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        /*
         * The mask buffer is `heightIn` rows (AND + XOR). We only read `height`
         * rows per plane.
         */
        if (heightIn != height * 2u) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const ULONGLONG maskPlane64 = (ULONGLONG)srcPitch * (ULONGLONG)height;
        if (maskPlane64 == 0 || (srcPitch != 0 && (maskPlane64 / srcPitch) != height) ||
            maskPlane64 > (ULONGLONG)(SIZE_T)~0ULL) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const SIZE_T maskPlaneBytes = (SIZE_T)maskPlane64;
        const UCHAR* pixels = (const UCHAR*)pShape->pPixels;
        const UCHAR* andMask = pixels;
        const UCHAR* xorMask = pixels + maskPlaneBytes;

        UCHAR* dst = (UCHAR*)cursorFbVa;

        for (ULONG y = 0; y < height; ++y) {
            const UCHAR* andRow = andMask + (SIZE_T)y * (SIZE_T)srcPitch;
            const UCHAR* xorRow = xorMask + (SIZE_T)y * (SIZE_T)srcPitch;
            UCHAR* dstRow = dst + (SIZE_T)y * (SIZE_T)dstPitchBytes;

            for (ULONG x = 0; x < width; ++x) {
                const ULONG byteIndex = x >> 3;
                const UCHAR bit = (UCHAR)(0x80u >> (x & 7u));
                const UCHAR a = andRow[byteIndex] & bit;
                const UCHAR xo = xorRow[byteIndex] & bit;

                /* Map AND/XOR to a best-effort RGBA value (cannot represent invert). */
                UCHAR r = 0, g = 0, b = 0, alpha = 0;
                if (a && !xo) {
                    /* Transparent. */
                    alpha = 0;
                } else if (!a && !xo) {
                    /* Black. */
                    r = g = b = 0;
                    alpha = 0xFF;
                } else if (!a && xo) {
                    /* White. */
                    r = g = b = 0xFF;
                    alpha = 0xFF;
                } else { /* a && xo */
                    /* Invert (approximate as white). */
                    r = g = b = 0xFF;
                    alpha = 0xFF;
                }

                const SIZE_T off = (SIZE_T)x * 4u;
                dstRow[off + 0] = b;
                dstRow[off + 1] = g;
                dstRow[off + 2] = r;
                dstRow[off + 3] = alpha;
            }
        }

        format = AEROGPU_FORMAT_B8G8R8A8_UNORM;
    } else if (flags.MaskedColor) {
        /*
         * Masked-color cursor: color bitmap + 1bpp AND mask.
         *
         * WDDM contracts vary across Windows versions/paths. In practice, we've observed two
         * plausible layouts:
         * 1) `Pitch` is the color pitch (>= width*4) and the AND mask is stored immediately after
         *    the color bitmap.
         * 2) `Pitch` is the AND-mask pitch (< width*4) and the color bitmap is stored after the
         *    mask.
         *
         * We conservatively handle both by inferring the layout from `Pitch`.
         */
        const ULONG srcPitch = pShape->Pitch;
        if (srcPitch == 0) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const UCHAR* pixels = (const UCHAR*)pShape->pPixels;
        const ULONG minMaskPitch = (width + 7u) / 8u;

        ULONG maskPitch = 0;
        if (!AeroGpuSafeAlignUpU32(minMaskPitch, 4u, &maskPitch) || maskPitch == 0) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const UCHAR* colorBase = NULL;
        const UCHAR* maskBase = NULL;
        ULONG colorPitch = 0;

        SIZE_T colorPlaneBytes = 0;
        SIZE_T maskPlaneBytes = 0;

        if (srcPitch >= dstPitchBytes) {
            /* Layout A: [color][mask]. `Pitch` is the color pitch. */
            colorPitch = srcPitch;

            const ULONGLONG colorBytes64 = (ULONGLONG)colorPitch * (ULONGLONG)height;
            if (colorPitch != 0 && (colorBytes64 / colorPitch) != height || colorBytes64 > (ULONGLONG)(SIZE_T)~0ULL) {
                if (poweredOn) {
                    AeroGpuCursorDisable(adapter);
                }
                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    adapter->CursorShapeValid = FALSE;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }
                return STATUS_INVALID_PARAMETER;
            }
            colorPlaneBytes = (SIZE_T)colorBytes64;

            const ULONGLONG maskBytes64 = (ULONGLONG)maskPitch * (ULONGLONG)height;
            if (maskPitch != 0 && (maskBytes64 / maskPitch) != height || maskBytes64 > (ULONGLONG)(SIZE_T)~0ULL) {
                if (poweredOn) {
                    AeroGpuCursorDisable(adapter);
                }
                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    adapter->CursorShapeValid = FALSE;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }
                return STATUS_INVALID_PARAMETER;
            }
            maskPlaneBytes = (SIZE_T)maskBytes64;

            colorBase = pixels;
            maskBase = pixels + colorPlaneBytes;
        } else {
            /* Layout B: [mask][color]. `Pitch` is the mask pitch (use it directly). */
            if (srcPitch < minMaskPitch) {
                if (poweredOn) {
                    AeroGpuCursorDisable(adapter);
                }
                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    adapter->CursorShapeValid = FALSE;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }
                return STATUS_INVALID_PARAMETER;
            }

            maskPitch = srcPitch;
            colorPitch = dstPitchBytes;

            const ULONGLONG maskBytes64 = (ULONGLONG)maskPitch * (ULONGLONG)height;
            if (maskPitch != 0 && (maskBytes64 / maskPitch) != height || maskBytes64 > (ULONGLONG)(SIZE_T)~0ULL) {
                if (poweredOn) {
                    AeroGpuCursorDisable(adapter);
                }
                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    adapter->CursorShapeValid = FALSE;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }
                return STATUS_INVALID_PARAMETER;
            }
            maskPlaneBytes = (SIZE_T)maskBytes64;

            const ULONGLONG colorBytes64 = (ULONGLONG)colorPitch * (ULONGLONG)height;
            if (colorPitch != 0 && (colorBytes64 / colorPitch) != height || colorBytes64 > (ULONGLONG)(SIZE_T)~0ULL) {
                if (poweredOn) {
                    AeroGpuCursorDisable(adapter);
                }
                {
                    KIRQL cursorIrql;
                    KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                    adapter->CursorShapeValid = FALSE;
                    KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                }
                return STATUS_INVALID_PARAMETER;
            }
            colorPlaneBytes = (SIZE_T)colorBytes64;

            maskBase = pixels;
            colorBase = pixels + maskPlaneBytes;
        }

        UCHAR* dst = (UCHAR*)cursorFbVa;

        /* Detect whether the source color bitmap has meaningful alpha (A8R8G8B8 vs X8R8G8B8). */
        BOOLEAN anyAlphaNonZero = FALSE;
        for (ULONG y = 0; y < height && !anyAlphaNonZero; ++y) {
            const UCHAR* srcRow = colorBase + (SIZE_T)y * (SIZE_T)colorPitch;
            for (ULONG x = 0; x < width; ++x) {
                const UCHAR a = srcRow[(SIZE_T)x * 4u + 3u];
                if (a != 0) {
                    anyAlphaNonZero = TRUE;
                    break;
                }
            }
        }

        for (ULONG y = 0; y < height; ++y) {
            const UCHAR* srcRow = colorBase + (SIZE_T)y * (SIZE_T)colorPitch;
            const UCHAR* maskRow = maskBase + (SIZE_T)y * (SIZE_T)maskPitch;
            UCHAR* dstRow = dst + (SIZE_T)y * (SIZE_T)dstPitchBytes;

            /* Copy the color pixels (ignore any source padding). */
            RtlCopyMemory(dstRow, srcRow, (SIZE_T)dstPitchBytes);

            /* Apply the 1bpp AND mask to alpha: bit=1 => transparent. */
            for (ULONG x = 0; x < width; ++x) {
                const ULONG byteIndex = x >> 3;
                const UCHAR bit = (UCHAR)(0x80u >> (x & 7u));
                const BOOLEAN transparent = (maskRow[byteIndex] & bit) ? TRUE : FALSE;
                UCHAR* px = dstRow + (SIZE_T)x * 4u;
                if (transparent) {
                    px[3] = 0;
                } else if (!anyAlphaNonZero && px[3] == 0) {
                    /* XRGB sources typically have alpha=0; force opaque for visible pixels. */
                    px[3] = 0xFF;
                }
            }
        }

        format = AEROGPU_FORMAT_B8G8R8A8_UNORM;
    } else if (flags.Color) {
        const ULONG srcPitch = pShape->Pitch;
        if (srcPitch == 0 || srcPitch < dstPitchBytes) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const ULONGLONG srcSize64 = (ULONGLONG)srcPitch * (ULONGLONG)height;
        if (srcPitch != 0 && (srcSize64 / srcPitch) != height) {
            if (poweredOn) {
                AeroGpuCursorDisable(adapter);
            }
            {
                KIRQL cursorIrql;
                KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
                adapter->CursorShapeValid = FALSE;
                KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
            }
            return STATUS_INVALID_PARAMETER;
        }

        const UCHAR* src = (const UCHAR*)pShape->pPixels;
        UCHAR* dst = (UCHAR*)cursorFbVa;

        BOOLEAN anyAlphaNonZero = FALSE;
        for (ULONG y = 0; y < height; ++y) {
            const UCHAR* srcRow = src + (SIZE_T)y * (SIZE_T)srcPitch;
            UCHAR* dstRow = dst + (SIZE_T)y * (SIZE_T)dstPitchBytes;
            RtlCopyMemory(dstRow, srcRow, (SIZE_T)dstPitchBytes);

            /* Detect XRGB inputs (alpha always 0) and switch to BGRX for display. */
            for (ULONG x = 0; x < width; ++x) {
                const UCHAR a = dstRow[(SIZE_T)x * 4u + 3u];
                if (a != 0) {
                    anyAlphaNonZero = TRUE;
                    break;
                }
            }
        }

        format = anyAlphaNonZero ? AEROGPU_FORMAT_B8G8R8A8_UNORM : AEROGPU_FORMAT_B8G8R8X8_UNORM;
    } else {
        if (poweredOn) {
            AeroGpuCursorDisable(adapter);
        }
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            adapter->CursorShapeValid = FALSE;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
        return STATUS_INVALID_PARAMETER;
    }

    BOOLEAN cursorVisible = FALSE;
    BOOLEAN cursorShapeValid = FALSE;
    LONG cursorX = 0;
    LONG cursorY = 0;
    PHYSICAL_ADDRESS cursorPa;
    cursorPa.QuadPart = 0;
    {
        KIRQL cursorIrql;
        KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
        adapter->CursorWidth = width;
        adapter->CursorHeight = height;
        adapter->CursorPitchBytes = dstPitchBytes;
        adapter->CursorFormat = format;
        adapter->CursorHotX = hotX;
        adapter->CursorHotY = hotY;
        adapter->CursorShapeValid = TRUE;
        cursorVisible = adapter->CursorVisible;
        cursorShapeValid = adapter->CursorShapeValid;
        cursorX = adapter->CursorX;
        cursorY = adapter->CursorY;
        cursorPa = adapter->CursorFbPa;
        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
    }

    if (poweredOn) {
        /* Program cursor registers. */
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, (ULONG)cursorX);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, (ULONG)cursorY);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X, hotX);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y, hotY);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, width);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, height);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, format);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, dstPitchBytes);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, cursorPa.LowPart);
        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, (ULONG)(cursorPa.QuadPart >> 32));

        KeMemoryBarrier();

        AeroGpuWriteRegU32(adapter,
                           AEROGPU_MMIO_REG_CURSOR_ENABLE,
                           (cursorVisible && cursorShapeValid && !adapter->PostDisplayOwnershipReleased) ? 1u : 0u);
    }

    return STATUS_SUCCESS;
}

static BOOLEAN AeroGpuTryReadLegacySubmissionDescHeader(_In_ AEROGPU_ADAPTER* Adapter,
                                                        _In_ ULONGLONG DescGpa,
                                                        _Out_ aerogpu_legacy_submission_desc_header* Out)
{
    if (!Adapter || !Out) {
        return FALSE;
    }
    RtlZeroMemory(Out, sizeof(*Out));

    if (DescGpa == 0) {
        return FALSE;
    }

    /*
     * Only peek at legacy submission descriptors when the GPA matches a
     * driver-tracked submission descriptor allocation. This avoids unsafe
     * MmGetVirtualForPhysical translations of arbitrary/corrupted GPAs.
     */
    KIRQL pendingIrql;
    KeAcquireSpinLock(&Adapter->PendingLock, &pendingIrql);

    BOOLEAN found = FALSE;
    const LIST_ENTRY* lists[] = {&Adapter->PendingSubmissions, &Adapter->RecentSubmissions};
    for (UINT listIdx = 0; listIdx < (UINT)(sizeof(lists) / sizeof(lists[0])) && !found; ++listIdx) {
        const LIST_ENTRY* head = lists[listIdx];
        for (PLIST_ENTRY entry = head->Flink; entry != head; entry = entry->Flink) {
            const AEROGPU_SUBMISSION* sub = CONTAINING_RECORD(entry, AEROGPU_SUBMISSION, ListEntry);
            if (!sub || !sub->DescVa || sub->DescSize < sizeof(*Out)) {
                continue;
            }
            if ((ULONGLONG)sub->DescPa.QuadPart != DescGpa) {
                continue;
            }

            __try {
                RtlCopyMemory(Out, sub->DescVa, sizeof(*Out));
                found = TRUE;
            } __except (EXCEPTION_EXECUTE_HANDLER) {
                found = FALSE;
            }
            break;
        }
    }

    KeReleaseSpinLock(&Adapter->PendingLock, pendingIrql);

    if (!found) {
        return FALSE;
    }
    if (Out->version != AEROGPU_LEGACY_SUBMISSION_DESC_VERSION) {
        return FALSE;
    }
    return TRUE;
}

static NTSTATUS APIENTRY AeroGpuDdiEscape(_In_ const HANDLE hAdapter, _Inout_ DXGKARG_ESCAPE* pEscape)
{
    AEROGPU_ADAPTER* adapter = (AEROGPU_ADAPTER*)hAdapter;
    if (!adapter || !pEscape || !pEscape->pPrivateDriverData || pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_header)) {
        return STATUS_INVALID_PARAMETER;
    }

    const BOOLEAN poweredOn =
        (adapter->Bar0 != NULL) &&
        ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) == DxgkDevicePowerStateD0);
    const BOOLEAN acceptingSubmissions =
        (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
    /*
     * Some dbgctl escapes read device MMIO state for diagnostics. During resume/teardown windows,
     * dxgkrnl may report the adapter as D0 (DevicePowerState==D0) before the driver has fully
     * restored ring/IRQ state, and MMIO reads can be unreliable. Gate optional MMIO reads on the
     * same "ready" signal used by submission paths.
     */
    const BOOLEAN mmioSafe = poweredOn && acceptingSubmissions;

    aerogpu_escape_header* hdr = (aerogpu_escape_header*)pEscape->pPrivateDriverData;
    if (hdr->version != AEROGPU_ESCAPE_VERSION) {
        return STATUS_NOT_SUPPORTED;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_device_v2_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_device_v2_out* out = (aerogpu_escape_query_device_v2_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;

        uint32_t magic = 0;
        uint32_t version = 0;
        uint64_t features = 0;
        if (mmioSafe) {
            magic = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_MAGIC);
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                version = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
                features = (uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                           ((uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
            } else {
                version = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
                /*
                 * Legacy devices do not guarantee FEATURES_LO/HI exist, but some
                 * bring-up device models expose them to allow incremental migration.
                 * If the values look plausible, report them for debugging.
                 */
                if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_FEATURES_HI + sizeof(ULONG))) {
                    const uint64_t maybeFeatures = (uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_LO) |
                                                   ((uint64_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_FEATURES_HI) << 32);
                    if ((maybeFeatures & ~(uint64_t)AEROGPU_KMD_LEGACY_PLAUSIBLE_FEATURES_MASK) == 0) {
                        features = maybeFeatures;
                    }
                }
            }
        } else {
            /* Return last-known values without touching MMIO while powered down. */
            magic = (uint32_t)adapter->DeviceMmioMagic;
            version = (uint32_t)adapter->DeviceAbiVersion;
            features = (uint64_t)adapter->DeviceFeatures;
        }

        out->detected_mmio_magic = magic;
        out->abi_version_u32 = version;
        out->features_lo = features;
        out->features_hi = 0;
        out->reserved0 = 0;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_DEVICE) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_device_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_device_out* out = (aerogpu_escape_query_device_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_DEVICE;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;
        if (!adapter->Bar0) {
            out->mmio_version = 0;
        } else if (!mmioSafe) {
            out->mmio_version = adapter->DeviceAbiVersion;
        } else if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            out->mmio_version = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ABI_VERSION);
        } else {
            out->mmio_version = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_VERSION);
        }
        out->reserved0 = 0;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_FENCE) {
        /*
         * Backward-compatible: older bring-up tools may send the original 32-byte
         * `aerogpu_escape_query_fence_out` (hdr + last_submitted + last_completed).
         *
         * The current struct is 48 bytes; only write fields that fit in the
         * caller-provided buffer.
         */
        if (pEscape->PrivateDriverDataSize <
             (offsetof(aerogpu_escape_query_fence_out, last_completed_fence) + sizeof(aerogpu_escape_u64))) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        ULONGLONG lastSubmittedFence = 0;
        ULONGLONG lastCompletedFence = 0;
        {
            KIRQL pendingIrql;
            KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);
            lastSubmittedFence = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
            lastCompletedFence = AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
            KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
        }

        ULONGLONG completedFence = lastCompletedFence;
        if (poweredOn) {
            ULONGLONG mmioFence = AeroGpuReadCompletedFence(adapter);
            /* Clamp for monotonicity + robustness against device reset/tearing. */
            if (mmioFence < lastCompletedFence) {
                mmioFence = lastCompletedFence;
            }
            if (mmioFence > lastSubmittedFence) {
                mmioFence = lastSubmittedFence;
            }
            completedFence = mmioFence;
        }

        aerogpu_escape_query_fence_out* out = (aerogpu_escape_query_fence_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_FENCE;
        out->hdr.size = (aerogpu_escape_u32)min((SIZE_T)sizeof(*out), (SIZE_T)pEscape->PrivateDriverDataSize);
        out->hdr.reserved0 = 0;
        out->last_submitted_fence = (uint64_t)lastSubmittedFence;
        out->last_completed_fence = (uint64_t)completedFence;

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_fence_out, error_irq_count) + sizeof(aerogpu_escape_u64))) {
            out->error_irq_count = (uint64_t)AeroGpuAtomicReadU64(&adapter->ErrorIrqCount);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_fence_out, last_error_fence) + sizeof(aerogpu_escape_u64))) {
            out->last_error_fence = (uint64_t)AeroGpuAtomicReadU64(&adapter->LastErrorFence);
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_PERF) {
        /*
         * Backward-compatible: older dbgctl builds may send a smaller
         * `aerogpu_escape_query_perf_out` buffer. This struct is extended by appending
         * fields; only write fields that fit in the caller-provided buffer.
         */
        if (pEscape->PrivateDriverDataSize <
            (offsetof(aerogpu_escape_query_perf_out, reserved0) + sizeof(aerogpu_escape_u32))) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_query_perf_out* out = (aerogpu_escape_query_perf_out*)pEscape->pPrivateDriverData;
        const SIZE_T outSize = min((SIZE_T)sizeof(*out), (SIZE_T)pEscape->PrivateDriverDataSize);
        RtlZeroMemory(out, outSize);
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_PERF;
        out->hdr.size = (aerogpu_escape_u32)outSize;
        out->hdr.reserved0 = 0;

        ULONGLONG lastSubmittedFence = 0;
        ULONGLONG lastCompletedFence = 0;
        {
            KIRQL pendingIrql;
            KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);
            lastSubmittedFence = AeroGpuAtomicReadU64(&adapter->LastSubmittedFence);
            lastCompletedFence = AeroGpuAtomicReadU64(&adapter->LastCompletedFence);
            KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
        }
        if (poweredOn) {
            ULONGLONG mmioFence = AeroGpuReadCompletedFence(adapter);
            /* Clamp for monotonicity + robustness against device reset/tearing. */
            if (mmioFence < lastCompletedFence) {
                mmioFence = lastCompletedFence;
            }
            if (mmioFence > lastSubmittedFence) {
                mmioFence = lastSubmittedFence;
            }
            lastCompletedFence = mmioFence;
        }

        out->last_submitted_fence = (uint64_t)lastSubmittedFence;
        out->last_completed_fence = (uint64_t)lastCompletedFence;

        out->ring0_size_bytes = (uint32_t)adapter->RingSizeBytes;
        out->ring0_entry_count = (uint32_t)adapter->RingEntryCount;

        BOOLEAN ringValid = FALSE;
        {
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            ULONG head = 0;
            ULONG tail = 0;
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                if (AeroGpuV1SubmitPathUsable(adapter)) {
                    const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)adapter->RingVa;
                    head = ringHeader->head;
                    tail = ringHeader->tail;
                    ringValid = TRUE;
                }
            } else if (mmioSafe && AeroGpuLegacySubmitPathUsable(adapter) &&
                       adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_TAIL + sizeof(ULONG))) {
                head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
                tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
                ringValid = TRUE;
            }

            out->ring0_head = (uint32_t)head;
            out->ring0_tail = (uint32_t)tail;

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }

        out->total_submissions = (uint64_t)InterlockedCompareExchange64(&adapter->PerfTotalSubmissions, 0, 0);
        out->total_presents = (uint64_t)InterlockedCompareExchange64(&adapter->PerfTotalPresents, 0, 0);
        out->total_render_submits = (uint64_t)InterlockedCompareExchange64(&adapter->PerfTotalRenderSubmits, 0, 0);
        out->total_internal_submits = (uint64_t)InterlockedCompareExchange64(&adapter->PerfTotalInternalSubmits, 0, 0);

        out->irq_fence_delivered = (uint64_t)InterlockedCompareExchange64(&adapter->PerfIrqFenceDelivered, 0, 0);
        out->irq_vblank_delivered = (uint64_t)InterlockedCompareExchange64(&adapter->PerfIrqVblankDelivered, 0, 0);
        out->irq_spurious = (uint64_t)InterlockedCompareExchange64(&adapter->PerfIrqSpurious, 0, 0);

        out->reset_from_timeout_count = (uint64_t)InterlockedCompareExchange64(&adapter->PerfResetFromTimeoutCount, 0, 0);
        out->last_reset_time_100ns = (uint64_t)InterlockedCompareExchange64(&adapter->PerfLastResetTime100ns, 0, 0);

        /* See aerogpu_dbgctl_escape.h: reserved0 encodes latched + last_error_time_10ms. */
        ULONG packed = 0;
        if (AeroGpuIsDeviceErrorLatched(adapter)) {
            packed |= 0x80000000u;
        }
        {
            const ULONGLONG lastError100ns = AeroGpuAtomicReadU64(&adapter->LastErrorTime100ns);
            if (lastError100ns != 0) {
                ULONGLONG t10ms = lastError100ns / 100000ull;
                if (t10ms > 0x7FFFFFFFull) {
                    t10ms = 0x7FFFFFFFull;
                }
                packed |= (ULONG)t10ms;
            }
        }
        out->reserved0 = packed;

        out->vblank_seq = (uint64_t)AeroGpuAtomicReadU64(&adapter->LastVblankSeq);
        out->last_vblank_time_ns = (uint64_t)AeroGpuAtomicReadU64(&adapter->LastVblankTimeNs);
        out->vblank_period_ns = (uint32_t)adapter->VblankPeriodNs;

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, error_irq_count) + sizeof(aerogpu_escape_u64))) {
            out->error_irq_count = (uint64_t)AeroGpuAtomicReadU64(&adapter->ErrorIrqCount);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, last_error_fence) + sizeof(aerogpu_escape_u64))) {
            out->last_error_fence = (uint64_t)AeroGpuAtomicReadU64(&adapter->LastErrorFence);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, ring_push_failures) + sizeof(aerogpu_escape_u64))) {
            out->ring_push_failures =
                (uint64_t)InterlockedCompareExchange64(&adapter->PerfRingPushFailures, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, selftest_count) + sizeof(aerogpu_escape_u64))) {
            out->selftest_count = (uint64_t)InterlockedCompareExchange64(&adapter->PerfSelftestCount, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, selftest_last_error_code) + sizeof(aerogpu_escape_u32))) {
            out->selftest_last_error_code =
                (uint32_t)InterlockedCompareExchange(&adapter->PerfSelftestLastErrorCode, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, flags) + sizeof(aerogpu_escape_u32))) {
            out->flags = AEROGPU_DBGCTL_QUERY_PERF_FLAGS_VALID;
            if (ringValid) {
                out->flags |= AEROGPU_DBGCTL_QUERY_PERF_FLAG_RING_VALID;
            }
            if (adapter->SupportsVblank) {
                out->flags |= AEROGPU_DBGCTL_QUERY_PERF_FLAG_VBLANK_VALID;
            }
#if DBG
            if (pEscape->PrivateDriverDataSize >=
                (offsetof(aerogpu_escape_query_perf_out, get_scanline_mmio_polls) + sizeof(aerogpu_escape_u64))) {
                out->flags |= AEROGPU_DBGCTL_QUERY_PERF_FLAG_GETSCANLINE_COUNTERS_VALID;
            }
#endif
        }

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, get_scanline_cache_hits) + sizeof(aerogpu_escape_u64))) {
#if DBG
            out->get_scanline_cache_hits =
                (uint64_t)InterlockedCompareExchange64(&adapter->PerfGetScanLineCacheHits, 0, 0);
#endif
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, get_scanline_mmio_polls) + sizeof(aerogpu_escape_u64))) {
#if DBG
            out->get_scanline_mmio_polls =
                (uint64_t)InterlockedCompareExchange64(&adapter->PerfGetScanLineMmioPolls, 0, 0);
#endif
        }

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_count) + sizeof(aerogpu_escape_u32))) {
            ULONG metaCount = 0;
            ULONGLONG metaBytes = 0;
            {
                KIRQL metaIrql;
                KeAcquireSpinLock(&adapter->MetaHandleLock, &metaIrql);
                metaCount = adapter->PendingMetaHandleCount;
                metaBytes = adapter->PendingMetaHandleBytes;
                KeReleaseSpinLock(&adapter->MetaHandleLock, metaIrql);
            }

            out->pending_meta_handle_count = (uint32_t)metaCount;
            if (pEscape->PrivateDriverDataSize >=
                (offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_reserved0) + sizeof(aerogpu_escape_u32))) {
                out->pending_meta_handle_reserved0 = 0;
            }
            if (pEscape->PrivateDriverDataSize >=
                (offsetof(aerogpu_escape_query_perf_out, pending_meta_handle_bytes) + sizeof(aerogpu_escape_u64))) {
                out->pending_meta_handle_bytes = (uint64_t)metaBytes;
            }
        }

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, contig_pool_hit) + sizeof(aerogpu_escape_u64))) {
            out->contig_pool_hit = (uint64_t)InterlockedCompareExchange64(&adapter->PerfContigPoolHit, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, contig_pool_miss) + sizeof(aerogpu_escape_u64))) {
            out->contig_pool_miss = (uint64_t)InterlockedCompareExchange64(&adapter->PerfContigPoolMiss, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, contig_pool_bytes_saved) + sizeof(aerogpu_escape_u64))) {
            out->contig_pool_bytes_saved = (uint64_t)InterlockedCompareExchange64(&adapter->PerfContigPoolBytesSaved, 0, 0);
        }

        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, alloc_table_count) + sizeof(aerogpu_escape_u64))) {
            out->alloc_table_count = (uint64_t)InterlockedCompareExchange64(&adapter->PerfAllocTableCount, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, alloc_table_readonly_entries) + sizeof(aerogpu_escape_u64))) {
            out->alloc_table_readonly_entries =
                (uint64_t)InterlockedCompareExchange64(&adapter->PerfAllocTableReadonlyEntries, 0, 0);
        }
        if (pEscape->PrivateDriverDataSize >=
            (offsetof(aerogpu_escape_query_perf_out, alloc_table_entries) + sizeof(aerogpu_escape_u64))) {
            out->alloc_table_entries = (uint64_t)InterlockedCompareExchange64(&adapter->PerfAllocTableEntries, 0, 0);
        }

        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_RING) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_ring_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_dump_ring_inout* io = (aerogpu_escape_dump_ring_inout*)pEscape->pPrivateDriverData;

        /* Only ring 0 is currently implemented. */
        if (io->ring_id != 0) {
            return STATUS_NOT_SUPPORTED;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->ring_size_bytes = adapter->RingSizeBytes;

        io->desc_capacity = (io->desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS)
                                ? AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS
                                : io->desc_capacity;

        /*
         * Avoid writing to the caller-provided output buffer while holding the
         * ring spin lock. Keep the critical section minimal by copying a bounded
         * snapshot under the lock, then formatting the response after releasing.
         */
        aerogpu_dbgctl_ring_desc local[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
        RtlZeroMemory(local, sizeof(local));
        aerogpu_legacy_ring_entry legacy[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
        RtlZeroMemory(legacy, sizeof(legacy));

        ULONG head = 0;
        ULONG tail = 0;
        ULONG outCount = 0;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

        const BOOLEAN v1RingValid = (adapter->AbiKind == AEROGPU_ABI_KIND_V1) ? AeroGpuV1SubmitPathUsable(adapter) : FALSE;
        const BOOLEAN legacyRingValid =
            (adapter->AbiKind != AEROGPU_ABI_KIND_V1) ? AeroGpuLegacySubmitPathUsable(adapter) : FALSE;

        if (v1RingValid) {
            const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)adapter->RingVa;
            head = ringHeader->head;
            tail = ringHeader->tail;
        } else if (legacyRingValid) {
            /*
             * Legacy head is device-owned (MMIO). Avoid MMIO reads unless the
             * adapter is in D0 and accepting submissions.
             */
            tail = adapter->RingTail;
            if (tail >= adapter->RingEntryCount) {
                if (mmioSafe && adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_TAIL + sizeof(ULONG))) {
                    tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
                }
                if (tail >= adapter->RingEntryCount) {
                    tail = 0;
                }
            }
            if (mmioSafe && adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_HEAD + sizeof(ULONG))) {
                head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
                if (head >= adapter->RingEntryCount) {
                    head %= adapter->RingEntryCount;
                }
            } else {
                head = tail;
            }
        } else {
            head = 0;
            tail = 0;
        }

        ULONG pending = 0;
        if (adapter->RingEntryCount != 0) {
            if (v1RingValid) {
                pending = tail - head;
                if (pending > adapter->RingEntryCount) {
                    pending = adapter->RingEntryCount;
                }
            } else if (legacyRingValid) {
                if (tail >= head) {
                    pending = tail - head;
                } else {
                    pending = tail + adapter->RingEntryCount - head;
                }
            }
        }

        outCount = pending;
        if (outCount > io->desc_capacity) {
            outCount = io->desc_capacity;
        }
        if (adapter->RingVa && adapter->RingEntryCount && outCount) {
            if (v1RingValid) {
                struct aerogpu_submit_desc* ring =
                    (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header));
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) & (adapter->RingEntryCount - 1);
                    const struct aerogpu_submit_desc entry = ring[idx];
                    local[i].signal_fence = (uint64_t)entry.signal_fence;
                    local[i].cmd_gpa = (uint64_t)entry.cmd_gpa;
                    local[i].cmd_size_bytes = entry.cmd_size_bytes;
                    local[i].flags = entry.flags;
                }
            } else if (legacyRingValid) {
                aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) % adapter->RingEntryCount;
                    legacy[i] = ring[idx];
                }
            }
        }

        KeReleaseSpinLock(&adapter->RingLock, oldIrql);

        /* Best-effort legacy header peek after releasing RingLock. */
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            for (ULONG i = 0; i < outCount; ++i) {
                const aerogpu_legacy_ring_entry entry = legacy[i];
                if (entry.type != AEROGPU_LEGACY_RING_ENTRY_SUBMIT) {
                    continue;
                }

                local[i].signal_fence = (uint64_t)entry.submit.fence;
                local[i].cmd_gpa = (uint64_t)entry.submit.desc_gpa;
                local[i].cmd_size_bytes = entry.submit.desc_size;
                local[i].flags = entry.submit.flags;

                aerogpu_legacy_submission_desc_header desc;
                if (AeroGpuTryReadLegacySubmissionDescHeader(adapter, (ULONGLONG)entry.submit.desc_gpa, &desc)) {
                    local[i].signal_fence = (uint64_t)desc.fence;
                    local[i].cmd_gpa = (uint64_t)desc.dma_buffer_gpa;
                    local[i].cmd_size_bytes = desc.dma_buffer_size;
                }
            }
        }

        io->head = head;
        io->tail = tail;
        io->desc_count = outCount;

        RtlZeroMemory(io->desc, sizeof(io->desc));
        for (ULONG i = 0; i < outCount; ++i) {
            io->desc[i] = local[i];
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_RING_V2) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_ring_v2_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_dump_ring_v2_inout* io = (aerogpu_escape_dump_ring_v2_inout*)pEscape->pPrivateDriverData;

        /* Only ring 0 is currently implemented. */
        if (io->ring_id != 0) {
            return STATUS_NOT_SUPPORTED;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_RING_V2;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->ring_size_bytes = adapter->RingSizeBytes;
        io->reserved0 = 0;
        io->reserved1 = 0;

        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_AGPU;
        } else if (adapter->AbiKind == AEROGPU_ABI_KIND_LEGACY) {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_LEGACY;
        } else {
            io->ring_format = AEROGPU_DBGCTL_RING_FORMAT_UNKNOWN;
        }

        io->desc_capacity = (io->desc_capacity > AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS)
                                ? AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS
                                : io->desc_capacity;

        /*
         * Avoid writing to the caller-provided output buffer while holding the
         * ring spin lock. Keep the critical section minimal by copying a bounded
         * snapshot under the lock, then formatting the response after releasing.
         */
        aerogpu_dbgctl_ring_desc_v2 local[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
        RtlZeroMemory(local, sizeof(local));
        aerogpu_legacy_ring_entry legacy[AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS];
        RtlZeroMemory(legacy, sizeof(legacy));

        ULONG head = 0;
        ULONG tail = 0;
        ULONG outCount = 0;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

        const BOOLEAN v1RingValid = (adapter->AbiKind == AEROGPU_ABI_KIND_V1) ? AeroGpuV1SubmitPathUsable(adapter) : FALSE;
        const BOOLEAN legacyRingValid =
            (adapter->AbiKind != AEROGPU_ABI_KIND_V1) ? AeroGpuLegacySubmitPathUsable(adapter) : FALSE;

        if (v1RingValid) {
            const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)adapter->RingVa;
            head = ringHeader->head;
            tail = ringHeader->tail;
        } else if (legacyRingValid) {
            /*
             * Legacy head is device-owned (MMIO). Avoid MMIO reads unless the
             * adapter is in D0 and accepting submissions.
             */
            tail = adapter->RingTail;
            if (tail >= adapter->RingEntryCount) {
                if (mmioSafe && adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_TAIL + sizeof(ULONG))) {
                    tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
                }
                if (tail >= adapter->RingEntryCount) {
                    tail = 0;
                }
            }
            if (mmioSafe && adapter->Bar0Length >= (AEROGPU_LEGACY_REG_RING_HEAD + sizeof(ULONG))) {
                head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
                if (head >= adapter->RingEntryCount) {
                    head %= adapter->RingEntryCount;
                }
            } else {
                head = tail;
            }
        } else {
            head = 0;
            tail = 0;
        }

        ULONG pending = 0;
        if (adapter->RingEntryCount != 0) {
            if (v1RingValid) {
                pending = tail - head;
                if (pending > adapter->RingEntryCount) {
                    pending = adapter->RingEntryCount;
                }
            } else if (legacyRingValid) {
                if (tail >= head) {
                    pending = tail - head;
                } else {
                    pending = tail + adapter->RingEntryCount - head;
                }
            }
        }

        /*
         * Tooling/tests want to be able to inspect the most recent submissions even
         * when the device consumes ring entries very quickly (for example, when the
         * emulator processes the doorbell synchronously). To make this robust, the
         * v2 dump returns a *recent window* of descriptors for the AGPU ring format
         * (ending at tail-1), rather than only the currently-pending [head, tail)
         * region.
         *
         * Legacy format is kept as a pending-only view because its head/tail are
         * not monotonic (masked indices).
         */
        outCount = pending;
        if (v1RingValid) {
            outCount = io->desc_capacity;
            if (outCount > adapter->RingEntryCount) {
                outCount = adapter->RingEntryCount;
            }
            if (tail < outCount) {
                outCount = tail;
            }
        } else if (legacyRingValid && outCount > io->desc_capacity) {
            outCount = io->desc_capacity;
        } else if (!legacyRingValid) {
            outCount = 0;
        }
        if (adapter->RingVa && adapter->RingEntryCount && outCount) {
            if (v1RingValid) {
                struct aerogpu_submit_desc* ring =
                    (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header));
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG start = tail - outCount;
                    const ULONG idx = (start + i) & (adapter->RingEntryCount - 1);
                    const struct aerogpu_submit_desc entry = ring[idx];
                    local[i].fence = (uint64_t)entry.signal_fence;
                    local[i].cmd_gpa = (uint64_t)entry.cmd_gpa;
                    local[i].cmd_size_bytes = entry.cmd_size_bytes;
                    local[i].flags = entry.flags;
                    local[i].alloc_table_gpa = (uint64_t)entry.alloc_table_gpa;
                    local[i].alloc_table_size_bytes = entry.alloc_table_size_bytes;
                    local[i].reserved0 = 0;
                }
            } else if (legacyRingValid) {
                aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                for (ULONG i = 0; i < outCount; ++i) {
                    const ULONG idx = (head + i) % adapter->RingEntryCount;
                    legacy[i] = ring[idx];
                }
            }
        }

        KeReleaseSpinLock(&adapter->RingLock, oldIrql);

        /* Best-effort legacy header peek after releasing RingLock. */
        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            for (ULONG i = 0; i < outCount; ++i) {
                const aerogpu_legacy_ring_entry entry = legacy[i];
                if (entry.type != AEROGPU_LEGACY_RING_ENTRY_SUBMIT) {
                    continue;
                }

                local[i].fence = (uint64_t)entry.submit.fence;
                local[i].cmd_gpa = (uint64_t)entry.submit.desc_gpa;
                local[i].cmd_size_bytes = entry.submit.desc_size;
                local[i].flags = entry.submit.flags;
                local[i].alloc_table_gpa = 0;
                local[i].alloc_table_size_bytes = 0;
                local[i].reserved0 = 0;

                aerogpu_legacy_submission_desc_header desc;
                if (AeroGpuTryReadLegacySubmissionDescHeader(adapter, (ULONGLONG)entry.submit.desc_gpa, &desc)) {
                    local[i].fence = (uint64_t)desc.fence;
                    local[i].cmd_gpa = (uint64_t)desc.dma_buffer_gpa;
                    local[i].cmd_size_bytes = desc.dma_buffer_size;

                    if (desc.type == AEROGPU_SUBMIT_PRESENT) {
                        local[i].flags |= AEROGPU_SUBMIT_FLAG_PRESENT;
                    }
                }
            }
        }

        io->head = head;
        io->tail = tail;
        io->desc_count = outCount;

        RtlZeroMemory(io->desc, sizeof(io->desc));
        for (ULONG i = 0; i < outCount; ++i) {
            io->desc[i] = local[i];
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_SELFTEST) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_selftest_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_selftest_inout* io = (aerogpu_escape_selftest_inout*)pEscape->pPrivateDriverData;
        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_SELFTEST;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;
        io->passed = 0;
        io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
        io->reserved0 = 0;

        InterlockedIncrement64(&adapter->PerfSelftestCount);
        InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);

        if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }

        ULONG timeoutMs = io->timeout_ms ? io->timeout_ms : 2000u;
        if (timeoutMs > 30000u) {
            timeoutMs = 30000u;
        }

        BOOLEAN ringReady = FALSE;
        {
            /*
             * AeroGpu*SubmitPathUsable reads ring header fields; take RingLock so we don't race
             * AeroGpuRingCleanup during teardown.
             */
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                ringReady = AeroGpuV1SubmitPathUsable(adapter);
            } else {
                ringReady = AeroGpuLegacySubmitPathUsable(adapter);
            }
            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }
        if (!ringReady) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY;
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }

        const ULONGLONG startTime = KeQueryInterruptTime();
        const ULONGLONG deadline = startTime + ((ULONGLONG)timeoutMs * 10000ull);

        /*
         * Selftest submits a ring entry directly and polls for head advancement.
         * Require the adapter to be in D0 (and accepting submissions) so we never
         * touch MMIO while powered down.
         */
        if ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) !=
                DxgkDevicePowerStateD0 ||
            InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) == 0 ||
            AeroGpuIsDeviceErrorLatched(adapter)) {
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }

        /*
         * Submit a "no-op" entry using the current completed fence value so we
         * don't advance the device fence beyond what dxgkrnl has issued.
         *
         * Completion is detected by observing ring head advancement, not fence
         * advancement.
         */
        const ULONGLONG completedFence = AeroGpuReadCompletedFence(adapter);
        const ULONGLONG fenceNoop = completedFence;

        /*
         * For the new (AGPU) device ABI, command buffers must begin with an
         * `aerogpu_cmd_stream_header`. Use a minimal NOP stream for selftest.
         */
        PVOID dmaVa = NULL;
        PHYSICAL_ADDRESS dmaPa;
        ULONG dmaSizeBytes = 0;
        dmaPa.QuadPart = 0;

        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            dmaSizeBytes = sizeof(struct aerogpu_cmd_stream_header) + sizeof(struct aerogpu_cmd_hdr);
            dmaVa = AeroGpuAllocContiguousNoInit(adapter, dmaSizeBytes, &dmaPa);
            if (!dmaVa) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }

            struct aerogpu_cmd_stream_header stream;
            RtlZeroMemory(&stream, sizeof(stream));
            stream.magic = AEROGPU_CMD_STREAM_MAGIC;
            stream.abi_version = AEROGPU_ABI_VERSION_U32;
            stream.size_bytes = (uint32_t)dmaSizeBytes;
            stream.flags = AEROGPU_CMD_STREAM_FLAG_NONE;
            stream.reserved0 = 0;
            stream.reserved1 = 0;

            struct aerogpu_cmd_hdr nop;
            RtlZeroMemory(&nop, sizeof(nop));
            nop.opcode = AEROGPU_CMD_NOP;
            nop.size_bytes = (uint32_t)sizeof(struct aerogpu_cmd_hdr);

            RtlCopyMemory(dmaVa, &stream, sizeof(stream));
            RtlCopyMemory((PUCHAR)dmaVa + sizeof(stream), &nop, sizeof(nop));
        }

        PVOID descVa = NULL;
        PHYSICAL_ADDRESS descPa;
        descPa.QuadPart = 0;

        if (adapter->AbiKind != AEROGPU_ABI_KIND_V1) {
            aerogpu_legacy_submission_desc_header* desc =
                (aerogpu_legacy_submission_desc_header*)AeroGpuAllocContiguousNoInit(adapter, sizeof(*desc), &descPa);
            descVa = desc;
            if (!desc) {
                AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }

            desc->version = AEROGPU_LEGACY_SUBMISSION_DESC_VERSION;
            desc->type = AEROGPU_SUBMIT_RENDER;
            desc->fence = (uint32_t)fenceNoop;
            desc->reserved0 = 0;
            desc->dma_buffer_gpa = 0;
            desc->dma_buffer_size = 0;
            desc->allocation_count = 0;
        }

        AEROGPU_PENDING_INTERNAL_SUBMISSION* selftestInternal = AeroGpuAllocPendingInternalSubmission(adapter);
        if (!selftestInternal) {
            AeroGpuFreeContiguousNonCached(adapter, descVa, sizeof(aerogpu_legacy_submission_desc_header));
            AeroGpuFreeContiguousNonCached(adapter, dmaVa, dmaSizeBytes);
            io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_NO_RESOURCES;
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }
        selftestInternal->Kind = AEROGPU_INTERNAL_SUBMISSION_KIND_SELFTEST;
        selftestInternal->ShareToken = 0;
        selftestInternal->CmdVa = dmaVa;
        selftestInternal->CmdSizeBytes = dmaSizeBytes;
        selftestInternal->DescVa = descVa;
        selftestInternal->DescSizeBytes = descVa ? sizeof(aerogpu_legacy_submission_desc_header) : 0;

        /* Push directly to the ring under RingLock for determinism. */
        ULONG headBefore = 0;
        NTSTATUS pushStatus = STATUS_SUCCESS;
        /* Require an idle GPU to avoid perturbing dxgkrnl's fence tracking. */
        {
            KIRQL pendingIrql;
            KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);
            BOOLEAN busy = !IsListEmpty(&adapter->PendingSubmissions) ||
                           (AeroGpuAtomicReadU64(&adapter->LastSubmittedFence) != completedFence);
            KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
            if (busy) {
                pushStatus = STATUS_DEVICE_BUSY;
            }
        }

        if (NT_SUCCESS(pushStatus)) {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->RingLock, &oldIrql);

            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                /*
                 * The v1 ring header lives at the base of the ring mapping. Use RingVa directly
                 * instead of trusting a potentially stale RingHeader pointer.
                 */
                struct aerogpu_ring_header* ringHeader = (struct aerogpu_ring_header*)adapter->RingVa;
                ULONG head = ringHeader->head;
                ULONG tail = adapter->RingTail;
                headBefore = head;

                if (NT_SUCCESS(pushStatus) && head != tail) {
                    pushStatus = STATUS_DEVICE_BUSY;
                }

                ULONG pending = tail - head;
                if (NT_SUCCESS(pushStatus) && pending >= adapter->RingEntryCount) {
                    pushStatus = STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
                } else if (NT_SUCCESS(pushStatus)) {
                    const ULONG slot = tail & (adapter->RingEntryCount - 1);
                    struct aerogpu_submit_desc* entry =
                        (struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header) +
                                                      ((SIZE_T)slot * sizeof(struct aerogpu_submit_desc)));

                    RtlZeroMemory(entry, sizeof(*entry));
                    entry->desc_size_bytes = (uint32_t)sizeof(struct aerogpu_submit_desc);
                    entry->flags = AEROGPU_SUBMIT_FLAG_NO_IRQ;
                    entry->context_id = 0;
                    entry->engine_id = AEROGPU_ENGINE_0;
                    entry->cmd_gpa = (uint64_t)dmaPa.QuadPart;
                    entry->cmd_size_bytes = dmaSizeBytes;
                    entry->alloc_table_gpa = 0;
                    entry->alloc_table_size_bytes = 0;
                    entry->signal_fence = (uint64_t)fenceNoop;

                    KeMemoryBarrier();
                    adapter->RingTail = tail + 1;
                    ringHeader->tail = adapter->RingTail;
                    selftestInternal->RingTailAfter = adapter->RingTail;
                    KeMemoryBarrier();

                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_DOORBELL, 1);
                }
            } else {
                ULONG head = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
                AeroGpuLegacyRingUpdateHeadSeqLocked(adapter, head);
                head = adapter->LegacyRingHeadIndex;
                ULONG tail = adapter->RingTail;
                if (tail >= adapter->RingEntryCount) {
                    /*
                     * Defensive: RingTail is a masked index for the legacy ABI. If the cached value is
                     * corrupted, resync it from the MMIO register to avoid out-of-bounds ring access.
                     */
                    tail = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL);
                    if (tail >= adapter->RingEntryCount) {
                        tail = 0;
                    }
                    adapter->RingTail = tail;
                    /*
                     * Repair the monotonic tail sequence counter to match the observed masked indices.
                     * Internal submission retirement relies on LegacyRingHeadSeq/LegacyRingTailSeq to be
                     * consistent (no modulo arithmetic).
                     */
                    {
                        const ULONG pending =
                            (tail >= head) ? (tail - head) : (tail + adapter->RingEntryCount - head);
                        adapter->LegacyRingTailSeq = adapter->LegacyRingHeadSeq + pending;
                    }
                }
                headBefore = head;

                if (NT_SUCCESS(pushStatus) && head != tail) {
                    pushStatus = STATUS_DEVICE_BUSY;
                }

                ULONG nextTail = (tail + 1) % adapter->RingEntryCount;
                if (NT_SUCCESS(pushStatus) && nextTail == head) {
                    pushStatus = STATUS_GRAPHICS_INSUFFICIENT_DMA_BUFFER;
                } else if (NT_SUCCESS(pushStatus)) {
                    aerogpu_legacy_ring_entry* ring = (aerogpu_legacy_ring_entry*)adapter->RingVa;
                    ring[tail].submit.type = AEROGPU_LEGACY_RING_ENTRY_SUBMIT;
                    ring[tail].submit.flags = 0;
                    ring[tail].submit.fence = (ULONG)fenceNoop;
                    ring[tail].submit.desc_size = (ULONG)sizeof(aerogpu_legacy_submission_desc_header);
                    ring[tail].submit.desc_gpa = (uint64_t)descPa.QuadPart;

                    KeMemoryBarrier();
                    adapter->RingTail = nextTail;
                    adapter->LegacyRingTailSeq += 1;
                    selftestInternal->RingTailAfter = adapter->LegacyRingTailSeq;
                    AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_TAIL, adapter->RingTail);
                    AeroGpuWriteRegU32(adapter, AEROGPU_LEGACY_REG_RING_DOORBELL, 1);
                }
            }

            KeReleaseSpinLock(&adapter->RingLock, oldIrql);
        }

        if (!NT_SUCCESS(pushStatus)) {
            AeroGpuFreeInternalSubmission(adapter, selftestInternal);
            io->error_code = (pushStatus == STATUS_DEVICE_BUSY)
                                 ? AEROGPU_DBGCTL_SELFTEST_ERR_GPU_BUSY
                                 : AEROGPU_DBGCTL_SELFTEST_ERR_RING_NOT_READY;
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }

        /* Poll for ring head advancement. */
        NTSTATUS testStatus = STATUS_TIMEOUT;
        while (KeQueryInterruptTime() < deadline) {
            /*
             * Be robust against teardown/power transitions while the selftest is running.
             *
             * - The ring header pointer can be detached/freed during StopDevice; take RingLock for
             *   v1 ring head reads.
             * - Avoid MMIO reads when leaving D0 or when submissions are blocked.
             */
            const BOOLEAN poweredOn =
                ((DXGK_DEVICE_POWER_STATE)InterlockedCompareExchange(&adapter->DevicePowerState, 0, 0) ==
                 DxgkDevicePowerStateD0);
            const BOOLEAN accepting = (InterlockedCompareExchange(&adapter->AcceptingSubmissions, 0, 0) != 0) ? TRUE : FALSE;
            if (!poweredOn || !accepting || AeroGpuIsDeviceErrorLatched(adapter)) {
                testStatus = STATUS_DEVICE_NOT_READY;
                break;
            }

            ULONG headNow = headBefore;
            if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                KIRQL ringIrql;
                KeAcquireSpinLock(&adapter->RingLock, &ringIrql);
                if (!adapter->RingVa || adapter->RingSizeBytes < sizeof(struct aerogpu_ring_header) || adapter->RingEntryCount == 0) {
                    KeReleaseSpinLock(&adapter->RingLock, ringIrql);
                    testStatus = STATUS_DEVICE_NOT_READY;
                    break;
                }
                const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)adapter->RingVa;
                headNow = ringHeader->head;
                KeReleaseSpinLock(&adapter->RingLock, ringIrql);
            } else {
                if (!adapter->Bar0 || adapter->Bar0Length < (AEROGPU_LEGACY_REG_RING_HEAD + sizeof(ULONG))) {
                    testStatus = STATUS_DEVICE_NOT_READY;
                    break;
                }
                headNow = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_RING_HEAD);
            }
            if (headNow != headBefore) {
                testStatus = STATUS_SUCCESS;
                break;
            }

            LARGE_INTEGER interval;
            interval.QuadPart = -10000; /* 1ms */
            KeDelayExecutionThread(KernelMode, FALSE, &interval);
        }

        if (!NT_SUCCESS(testStatus)) {
            /*
             * The device did not consume the entry in time. Do not free the
             * descriptor/DMA buffer to avoid use-after-free if the device
             * consumes it later.
             */
            {
                KIRQL pendingIrql;
                KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);
                InsertTailList(&adapter->PendingInternalSubmissions, &selftestInternal->ListEntry);
                KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);
            }
            io->passed = 0;
            if (testStatus == STATUS_TIMEOUT) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_TIMEOUT;
            } else {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
            }
            InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
            return STATUS_SUCCESS;
        }

        AeroGpuFreeInternalSubmission(adapter, selftestInternal);

        /*
         * VBlank sanity (optional, gated by device feature bits).
         *
         * Only attempt to validate vblank tick forward progress when scanout is enabled.
         */
        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_VBLANK) != 0) {
            if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }
            const BOOLEAN haveVblankRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS + sizeof(ULONG));
            if (!haveVblankRegs) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_REGS_OUT_OF_RANGE;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }

            BOOLEAN scanoutEnabled = FALSE;
            if (adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
                if (adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_ENABLE + sizeof(ULONG))) {
                    scanoutEnabled = (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE) != 0);
                }
            } else {
                if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_SCANOUT_ENABLE + sizeof(ULONG))) {
                    scanoutEnabled = (AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE) != 0);
                }
            }

            if (scanoutEnabled) {
                if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }
                ULONG periodNs = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
                if (periodNs == 0) {
                    periodNs = AEROGPU_VBLANK_PERIOD_NS_DEFAULT;
                }
                ULONG periodMs = (periodNs + 999999u) / 1000000u;
                if (periodMs == 0) {
                    periodMs = 1;
                }

                ULONG seqWaitMs = periodMs * 2u;
                if (seqWaitMs < 10u) {
                    seqWaitMs = 10u;
                }
                if (seqWaitMs > 2000u) {
                    seqWaitMs = 2000u;
                }
                const ULONGLONG seqNow100ns = KeQueryInterruptTime();
                const ULONGLONG seqWait100ns = ((ULONGLONG)seqWaitMs * 10000ull);
                if (seqNow100ns >= deadline || (deadline - seqNow100ns) < seqWait100ns) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }
                const ULONGLONG seqDeadline = seqNow100ns + seqWait100ns;

                const ULONGLONG seq0 = AeroGpuReadRegU64HiLoHi(adapter,
                                                              AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                              AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
                ULONGLONG seqNow = seq0;
                while (KeQueryInterruptTime() < seqDeadline) {
                    LARGE_INTEGER interval;
                    interval.QuadPart = -10000; /* 1ms */
                    KeDelayExecutionThread(KernelMode, FALSE, &interval);

                    if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                        io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
                        InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                        return STATUS_SUCCESS;
                    }
                    seqNow = AeroGpuReadRegU64HiLoHi(adapter,
                                                     AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                     AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
                    if (seqNow > seq0) {
                        break;
                    }
                    if (seqNow < seq0) {
                        /* Vblank sequence must be monotonic. Treat regressions as failure. */
                        io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK;
                        InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                        return STATUS_SUCCESS;
                    }
                }

                if (seqNow <= seq0) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_SEQ_STUCK;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }

                /*
                 * IRQ enable/ack sanity for vblank.
                 *
                 * To avoid racing with the normal ISR (which ACKs IRQ_STATUS quickly),
                 * temporarily disable dxgkrnl interrupt delivery while we poke the
                 * device IRQ registers.
                 */
                const BOOLEAN haveIrqRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ACK + sizeof(ULONG));
                if (!haveIrqRegs) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_REGS_OUT_OF_RANGE;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }

                const BOOLEAN canDisableOsInterrupts = adapter->InterruptRegistered &&
                                                      adapter->DxgkInterface.DxgkCbDisableInterrupt &&
                                                      adapter->DxgkInterface.DxgkCbEnableInterrupt;
                if (canDisableOsInterrupts) {
                    ULONG savedEnableMask = 0;
                    {
                        KIRQL oldIrql;
                        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                        savedEnableMask = adapter->IrqEnableMask;
                        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                    }

                    /*
                     * Keep OS interrupt delivery disabled only briefly. A long disable window can
                     * starve dxgkrnl of DMA completion interrupts.
                     */
                    ULONG irqWaitMs = periodMs * 3u;
                    if (irqWaitMs < 10u) {
                        irqWaitMs = 10u;
                    }
                    if (irqWaitMs > 250u) {
                        irqWaitMs = 250u;
                    }
                    const ULONGLONG irqNow = KeQueryInterruptTime();
                    ULONGLONG irqDeadline = irqNow + ((ULONGLONG)irqWaitMs * 10000ull);
                    if (irqDeadline > deadline) {
                        irqDeadline = deadline;
                    }

                    if (irqDeadline <= irqNow) {
                        /*
                         * No time budget remaining. Skip the optional IRQ status/ack test rather
                         * than leaving interrupts disabled.
                         */
                        goto skip_vblank_irq_test;
                    }

                    BOOLEAN osInterruptsDisabled = FALSE;
                    adapter->DxgkInterface.DxgkCbDisableInterrupt(adapter->StartInfo.hDxgkHandle);
                    osInterruptsDisabled = TRUE;
                    BOOLEAN ok = FALSE;
                    BOOLEAN aborted = FALSE;

                    /*
                     * Ensure vblank is disabled and ACKed before we start, so we don't
                     * inherit a stale pending bit.
                     */
                    {
                        KIRQL oldIrql;
                        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                        ULONG enable = savedEnableMask & ~AEROGPU_IRQ_SCANOUT_VBLANK;
                        if (AeroGpuIsDeviceErrorLatched(adapter)) {
                            enable &= ~AEROGPU_IRQ_ERROR;
                        }
                        adapter->IrqEnableMask = enable;
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                    }
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);

                    /* Enable vblank IRQ generation and wait for the status bit to latch. */
                    {
                        KIRQL oldIrql;
                        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                        ULONG enable = savedEnableMask | AEROGPU_IRQ_SCANOUT_VBLANK;
                        if (AeroGpuIsDeviceErrorLatched(adapter)) {
                            enable &= ~AEROGPU_IRQ_ERROR;
                        }
                        adapter->IrqEnableMask = enable;
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                    }

                    ULONG status = 0;
                    while (KeQueryInterruptTime() < irqDeadline) {
                        if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                            aborted = TRUE;
                            break;
                        }
                        status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
                        if ((status & AEROGPU_IRQ_SCANOUT_VBLANK) != 0) {
                            break;
                        }
                        LARGE_INTEGER interval;
                        interval.QuadPart = -10000; /* 1ms */
                        KeDelayExecutionThread(KernelMode, FALSE, &interval);
                    }

                    if ((status & AEROGPU_IRQ_SCANOUT_VBLANK) == 0) {
                        io->error_code = aborted ? AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE
                                                 : AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_LATCHED;
                    } else {
                        /*
                         * Disable the bit to avoid a new tick re-latching while we
                         * validate ACK clears the status.
                         */
                        {
                            KIRQL oldIrql;
                            KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                            ULONG enable = savedEnableMask & ~AEROGPU_IRQ_SCANOUT_VBLANK;
                            if (AeroGpuIsDeviceErrorLatched(adapter)) {
                                enable &= ~AEROGPU_IRQ_ERROR;
                            }
                            adapter->IrqEnableMask = enable;
                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                            KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                        }

                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
                        status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
                        while ((status & AEROGPU_IRQ_SCANOUT_VBLANK) != 0 && KeQueryInterruptTime() < irqDeadline) {
                            if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                                aborted = TRUE;
                                break;
                            }
                            LARGE_INTEGER interval;
                            interval.QuadPart = -10000; /* 1ms */
                            KeDelayExecutionThread(KernelMode, FALSE, &interval);
                            status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
                        }
                        if ((status & AEROGPU_IRQ_SCANOUT_VBLANK) != 0) {
                            io->error_code = aborted ? AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE
                                                     : AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_CLEARED;
                        } else {
                            ok = TRUE;
                        }
                    }

                    /* Restore IRQ enable mask to whatever dxgkrnl had configured. */
                    {
                        KIRQL oldIrql;
                        KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                        ULONG enable = savedEnableMask;
                        if (AeroGpuIsDeviceErrorLatched(adapter)) {
                            enable &= ~AEROGPU_IRQ_ERROR;
                        }
                        adapter->IrqEnableMask = enable;
                        if (AeroGpuMmioSafeNow(adapter)) {
                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                        }
                        KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                    }
                    if (AeroGpuMmioSafeNow(adapter)) {
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
                    }

                    if (osInterruptsDisabled && AeroGpuMmioSafeNow(adapter) && adapter->InterruptRegistered) {
                        adapter->DxgkInterface.DxgkCbEnableInterrupt(adapter->StartInfo.hDxgkHandle);
                    }

                    if (!ok) {
                        InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                        return STATUS_SUCCESS;
                    }
                }

            skip_vblank_irq_test:;

                /*
                 * IRQ delivery sanity: ensure the vblank interrupt reaches our ISR.
                 *
                 * This uses PerfIrqVblankDelivered which is incremented in the ISR only.
                 */
                if (!adapter->InterruptRegistered) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }

                if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }
                const LONGLONG delivered0 = InterlockedCompareExchange64(&adapter->PerfIrqVblankDelivered, 0, 0);
                const LONG dpc0 = InterlockedCompareExchange(&adapter->IrqDpcCount, 0, 0);
                BOOLEAN origVblankEnabled = FALSE;
                {
                    KIRQL oldIrql;
                    KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                    const ULONG cur = adapter->IrqEnableMask;
                    origVblankEnabled = ((cur & AEROGPU_IRQ_SCANOUT_VBLANK) != 0);
                    ULONG enable = cur | AEROGPU_IRQ_SCANOUT_VBLANK;
                    if (AeroGpuIsDeviceErrorLatched(adapter)) {
                        enable &= ~AEROGPU_IRQ_ERROR;
                    }
                    adapter->IrqEnableMask = enable;
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                    if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                        enable &= ~AEROGPU_IRQ_ERROR;
                        adapter->IrqEnableMask = enable;
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                    }
                    KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                }
                AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);

                ULONG deliveryWaitMs = periodMs * 2u;
                if (deliveryWaitMs < 10u) {
                    deliveryWaitMs = 10u;
                }
                if (deliveryWaitMs > 5000u) {
                    deliveryWaitMs = 5000u;
                }
                const ULONGLONG deliveryNow100ns = KeQueryInterruptTime();
                const ULONGLONG deliveryWait100ns = ((ULONGLONG)deliveryWaitMs * 10000ull);
                if (deliveryNow100ns >= deadline || (deadline - deliveryNow100ns) < deliveryWait100ns) {
                    io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_TIME_BUDGET_EXHAUSTED;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }
                const ULONGLONG deliveryDeadline = deliveryNow100ns + deliveryWait100ns;

                BOOLEAN delivered = FALSE;
                BOOLEAN deliveryInvalidState = FALSE;
                while (KeQueryInterruptTime() < deliveryDeadline) {
                    if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                        deliveryInvalidState = TRUE;
                        break;
                    }
                    const LONGLONG deliveredNow = InterlockedCompareExchange64(&adapter->PerfIrqVblankDelivered, 0, 0);
                    const LONG dpcNow = InterlockedCompareExchange(&adapter->IrqDpcCount, 0, 0);
                    if (deliveredNow != delivered0 && dpcNow != dpc0) {
                        delivered = TRUE;
                        break;
                    }
                    LARGE_INTEGER interval;
                    interval.QuadPart = -10000; /* 1ms */
                    KeDelayExecutionThread(KernelMode, FALSE, &interval);
                }

                {
                    KIRQL oldIrql;
                    KeAcquireSpinLock(&adapter->IrqEnableLock, &oldIrql);
                    ULONG enable = adapter->IrqEnableMask;
                    if (origVblankEnabled) {
                        enable |= AEROGPU_IRQ_SCANOUT_VBLANK;
                    } else {
                        enable &= ~AEROGPU_IRQ_SCANOUT_VBLANK;
                    }
                    if (AeroGpuIsDeviceErrorLatched(adapter)) {
                        enable &= ~AEROGPU_IRQ_ERROR;
                    }
                    adapter->IrqEnableMask = enable;
                    if (AeroGpuMmioSafeNow(adapter)) {
                        AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                        if ((enable & AEROGPU_IRQ_ERROR) != 0 && AeroGpuIsDeviceErrorLatched(adapter)) {
                            enable &= ~AEROGPU_IRQ_ERROR;
                            adapter->IrqEnableMask = enable;
                            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE, enable);
                        }
                    }
                    KeReleaseSpinLock(&adapter->IrqEnableLock, oldIrql);
                }
                if (AeroGpuMmioSafeNow(adapter)) {
                    AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ACK, AEROGPU_IRQ_SCANOUT_VBLANK);
                }

                if (!delivered) {
                    io->error_code = deliveryInvalidState ? AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE
                                                          : AEROGPU_DBGCTL_SELFTEST_ERR_VBLANK_IRQ_NOT_DELIVERED;
                    InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                    return STATUS_SUCCESS;
                }
            }
        }

        /* Cursor sanity (optional, gated by device feature bits). */
        if ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0) {
            if (!AeroGpuMmioSafeNow(adapter) || AeroGpuIsDeviceErrorLatched(adapter)) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_INVALID_STATE;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }
            const BOOLEAN haveCursorRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG));
            if (!haveCursorRegs) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_REGS_OUT_OF_RANGE;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }

            /* Save original MMIO cursor register state. */
            const ULONG origEnable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE);
            const ULONG origX = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X);
            const ULONG origY = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y);
            const ULONG origHotX = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X);
            const ULONG origHotY = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y);
            const ULONG origW = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH);
            const ULONG origH = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT);
            const ULONG origFmt = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT);
            const ULONG origPitch = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES);
            const ULONG origFbLo = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO);
            const ULONG origFbHi = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI);

            /*
             * Program a small cursor config and verify that the register writes stick.
             *
             * This intentionally does not rely on any backing store; fb_gpa is 0 and we
             * validate the config via readback only.
             */
            const ULONG testEnable = 1u;
            const ULONG testX = origX ^ 0x10u;
            const ULONG testY = origY ^ 0x20u;
            const ULONG testHotX = 0u;
            const ULONG testHotY = 0u;
            const ULONG testW = 16u;
            const ULONG testH = 16u;
            const ULONG testFmt = (ULONG)AEROGPU_FORMAT_B8G8R8A8_UNORM;
            const ULONG testPitch = testW * 4u;
            const ULONG testFbLo = 0u;
            const ULONG testFbHi = 0u;

            /* Disable while programming to avoid any transient DMA from a stale cursor GPA. */
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            KeMemoryBarrier();

            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, testX);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, testY);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X, testHotX);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y, testHotY);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, testW);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, testH);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, testFmt);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, testPitch);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, testFbLo);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, testFbHi);
            KeMemoryBarrier();
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, testEnable);
            KeMemoryBarrier();

            BOOLEAN ok = TRUE;
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE) != testEnable) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X) != testX) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y) != testY) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X) != testHotX) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y) != testHotY) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH) != testW) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT) != testH) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT) != testFmt) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES) != testPitch) {
                ok = FALSE;
            }
            if (AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO) != testFbLo ||
                AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI) != testFbHi) {
                ok = FALSE;
            }

            /* Restore original cursor register state regardless of the readback result. */
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, 0);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X, origX);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y, origY);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X, origHotX);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y, origHotY);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH, origW);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT, origH);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT, origFmt);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES, origPitch);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO, origFbLo);
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI, origFbHi);
            KeMemoryBarrier();
            AeroGpuWriteRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE, origEnable);

            if (!ok) {
                io->error_code = AEROGPU_DBGCTL_SELFTEST_ERR_CURSOR_RW_MISMATCH;
                InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
                return STATUS_SUCCESS;
            }
        }

        io->passed = 1;
        io->error_code = AEROGPU_DBGCTL_SELFTEST_OK;
        InterlockedExchange(&adapter->PerfSelftestLastErrorCode, (LONG)io->error_code);
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_VBLANK) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_vblank_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_query_vblank_out* out = (aerogpu_escape_query_vblank_out*)pEscape->pPrivateDriverData;

        /* Only scanout/source 0 is currently implemented. */
        if (out->vidpn_source_id != AEROGPU_VIDPN_SOURCE_ID) {
            return STATUS_NOT_SUPPORTED;
        }

        if (!adapter->SupportsVblank) {
            return STATUS_NOT_SUPPORTED;
        }

        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_VBLANK;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;

        out->flags = AEROGPU_DBGCTL_QUERY_VBLANK_FLAGS_VALID | AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_VBLANK_SUPPORTED;

        if (mmioSafe) {
            const BOOLEAN haveIrqRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_IRQ_ENABLE + sizeof(ULONG));
            if (haveIrqRegs) {
                out->irq_enable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_ENABLE);
                out->irq_status = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_IRQ_STATUS);
            } else {
                out->irq_enable = 0;
                out->irq_status = 0;
            }

            out->vblank_seq = AeroGpuReadRegU64HiLoHi(adapter,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
                                                      AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI);
            out->last_vblank_time_ns = AeroGpuReadRegU64HiLoHi(adapter,
                                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
                                                               AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI);
            out->vblank_period_ns = (uint32_t)AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS);
        } else {
            /* Avoid MMIO reads while the adapter is not in D0 (or not yet restored); return cached values. */
            out->irq_enable = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->IrqEnableMask);
            out->irq_status = 0;
            out->vblank_seq = AeroGpuAtomicReadU64(&adapter->LastVblankSeq);
            out->last_vblank_time_ns = AeroGpuAtomicReadU64(&adapter->LastVblankTimeNs);
            out->vblank_period_ns = (uint32_t)adapter->VblankPeriodNs;
        }

        out->vblank_interrupt_type = 0;
        if (adapter->VblankInterruptTypeValid) {
            KeMemoryBarrier();
            out->flags |= AEROGPU_DBGCTL_QUERY_VBLANK_FLAG_INTERRUPT_TYPE_VALID;
            out->vblank_interrupt_type = (uint32_t)adapter->VblankInterruptType;
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_SCANOUT) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_scanout_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_query_scanout_out* out = (aerogpu_escape_query_scanout_out*)pEscape->pPrivateDriverData;
        const BOOLEAN haveV2 = ((SIZE_T)pEscape->PrivateDriverDataSize >= sizeof(aerogpu_escape_query_scanout_out_v2)) ? TRUE : FALSE;
        aerogpu_escape_query_scanout_out_v2* out2 = haveV2 ? (aerogpu_escape_query_scanout_out_v2*)out : NULL;

        /* Only scanout/source 0 is currently implemented. */
        if (out->vidpn_source_id != AEROGPU_VIDPN_SOURCE_ID) {
            return STATUS_NOT_SUPPORTED;
        }

        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_SCANOUT;
        out->hdr.size = haveV2 ? (aerogpu_escape_u32)sizeof(*out2) : (aerogpu_escape_u32)sizeof(*out);
        out->hdr.reserved0 = 0;

        out->reserved0 = 0;
        if (haveV2) {
            uint32_t flags = AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID;
            if (adapter->PostDisplayOwnershipReleased) {
                flags |= AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED;
            }
            const uint64_t cachedFbGpa = (uint64_t)adapter->CurrentScanoutFbPa.QuadPart;
            out2->cached_fb_gpa = cachedFbGpa;
            if (cachedFbGpa != 0) {
                flags |= AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_CACHED_FB_GPA_VALID;
            }
            out->reserved0 = flags;
        }

        out->cached_enable = adapter->SourceVisible ? 1u : 0u;
        out->cached_width = adapter->CurrentWidth;
        out->cached_height = adapter->CurrentHeight;
        out->cached_format = adapter->CurrentFormat;
        out->cached_pitch_bytes = adapter->CurrentPitch;

        out->mmio_enable = 0;
        out->mmio_width = 0;
        out->mmio_height = 0;
        out->mmio_format = 0;
        out->mmio_pitch_bytes = 0;
        out->mmio_fb_gpa = 0;

        if (!mmioSafe) {
            /* Avoid MMIO reads while the adapter is not in D0 (or not yet restored); return cached values. */
            out->mmio_enable = out->cached_enable;
            out->mmio_width = out->cached_width;
            out->mmio_height = out->cached_height;
            out->mmio_format = out->cached_format;
            out->mmio_pitch_bytes = out->cached_pitch_bytes;
            out->mmio_fb_gpa = (uint64_t)adapter->CurrentScanoutFbPa.QuadPart;
        } else {
            if ((adapter->UsingNewAbi || adapter->AbiKind == AEROGPU_ABI_KIND_V1) &&
                adapter->Bar0Length >= (AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI + sizeof(ULONG))) {
                out->mmio_enable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_ENABLE);
                out->mmio_width = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_WIDTH);
                out->mmio_height = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_HEIGHT);
                out->mmio_format = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FORMAT);
                out->mmio_pitch_bytes = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES);
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI);
                out->mmio_fb_gpa = ((uint64_t)hi << 32) | (uint64_t)lo;
            } else if (adapter->Bar0Length >= (AEROGPU_LEGACY_REG_SCANOUT_FB_HI + sizeof(ULONG))) {
                out->mmio_enable = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_ENABLE);
                out->mmio_width = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_WIDTH);
                out->mmio_height = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_HEIGHT);
                out->mmio_format = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FORMAT);
                out->mmio_pitch_bytes = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_PITCH);
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_LEGACY_REG_SCANOUT_FB_HI);
                out->mmio_fb_gpa = ((uint64_t)hi << 32) | (uint64_t)lo;
            }
        }

        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_CURSOR) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_cursor_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_cursor_out* out = (aerogpu_escape_query_cursor_out*)pEscape->pPrivateDriverData;

        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_CURSOR;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;

        out->flags = AEROGPU_DBGCTL_QUERY_CURSOR_FLAGS_VALID;
        if (adapter->PostDisplayOwnershipReleased) {
            out->flags |= AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED;
        }
        out->reserved0 = 0;

        out->enable = 0;
        out->x = 0;
        out->y = 0;
        out->hot_x = 0;
        out->hot_y = 0;
        out->width = 0;
        out->height = 0;
        out->format = 0;
        out->fb_gpa = 0;
        out->pitch_bytes = 0;
        out->reserved1 = 0;

        BOOLEAN cursorSupported = FALSE;
        if (mmioSafe) {
            const BOOLEAN haveCursorRegs = adapter->Bar0Length >= (AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES + sizeof(ULONG));
            if (!haveCursorRegs) {
                return STATUS_SUCCESS;
            }

            cursorSupported = ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0) ? TRUE : FALSE;
            if (!cursorSupported) {
                return STATUS_SUCCESS;
            }

            out->flags |= AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED;

            out->enable = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_ENABLE);
            out->x = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_X);
            out->y = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_Y);
            out->hot_x = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_X);
            out->hot_y = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HOT_Y);
            out->width = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_WIDTH);
            out->height = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_HEIGHT);
            out->format = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FORMAT);

            {
                const ULONG lo = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO);
                const ULONG hi = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI);
                out->fb_gpa = ((uint64_t)hi << 32) | (uint64_t)lo;
            }

            out->pitch_bytes = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES);
            return STATUS_SUCCESS;
        }

        /* Avoid MMIO reads while the adapter is not in D0 (or not yet restored); return cached values. */
        cursorSupported = ((adapter->DeviceFeatures & (ULONGLONG)AEROGPU_FEATURE_CURSOR) != 0) ? TRUE : FALSE;
        if (!cursorSupported) {
            return STATUS_SUCCESS;
        }

        out->flags |= AEROGPU_DBGCTL_QUERY_CURSOR_FLAG_CURSOR_SUPPORTED;

        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);

            const BOOLEAN shapeValid = adapter->CursorShapeValid;
            const BOOLEAN visible = adapter->CursorVisible;
            const BOOLEAN shapeReady =
                shapeValid &&
                adapter->CursorFbPa.QuadPart != 0 &&
                adapter->CursorPitchBytes != 0 &&
                adapter->CursorWidth != 0 &&
                adapter->CursorHeight != 0;

            out->x = (uint32_t)adapter->CursorX;
            out->y = (uint32_t)adapter->CursorY;

            /*
             * Only return cursor shape-dependent fields when we have a valid shape
             * and backing store; otherwise keep values conservative.
             */
            if (shapeReady) {
                out->hot_x = (uint32_t)adapter->CursorHotX;
                out->hot_y = (uint32_t)adapter->CursorHotY;
                out->width = (uint32_t)adapter->CursorWidth;
                out->height = (uint32_t)adapter->CursorHeight;
                out->format = (uint32_t)adapter->CursorFormat;
                out->fb_gpa = (uint64_t)adapter->CursorFbPa.QuadPart;
                out->pitch_bytes = (uint32_t)adapter->CursorPitchBytes;
            }

            /*
             * If post-display ownership is currently released, the miniport must keep cursor DMA
             * disabled (even if the cached cursor state still indicates it should be visible).
             */
            out->enable = (visible && shapeReady && !adapter->PostDisplayOwnershipReleased) ? 1u : 0u;

            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_SET_CURSOR_POSITION) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_set_cursor_position_in)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const aerogpu_escape_set_cursor_position_in* in =
            (const aerogpu_escape_set_cursor_position_in*)pEscape->pPrivateDriverData;

        /* Preserve the current visibility bit; SetCursorPosition only updates coordinates. */
        BOOLEAN visible = FALSE;
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            visible = adapter->CursorVisible;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }

        DXGKARG_SETPOINTERPOSITION pos;
        RtlZeroMemory(&pos, sizeof(pos));
        pos.VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
        pos.Visible = visible;
        pos.X = (LONG)in->x;
        pos.Y = (LONG)in->y;

        return AeroGpuDdiSetPointerPosition(hAdapter, &pos);
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_SET_CURSOR_VISIBILITY) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_set_cursor_visibility_in)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const aerogpu_escape_set_cursor_visibility_in* in =
            (const aerogpu_escape_set_cursor_visibility_in*)pEscape->pPrivateDriverData;

        /* Preserve the current position; ShowCursor only toggles visibility. */
        LONG x = 0;
        LONG y = 0;
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);
            x = adapter->CursorX;
            y = adapter->CursorY;
            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }

        DXGKARG_SETPOINTERPOSITION pos;
        RtlZeroMemory(&pos, sizeof(pos));
        pos.VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
        pos.Visible = (in->visible != 0) ? TRUE : FALSE;
        pos.X = x;
        pos.Y = y;

        return AeroGpuDdiSetPointerPosition(hAdapter, &pos);
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_SET_CURSOR_SHAPE) {
        const SIZE_T headerBytes = (SIZE_T)offsetof(aerogpu_escape_set_cursor_shape_in, pixels);
        if (pEscape->PrivateDriverDataSize < headerBytes) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        const aerogpu_escape_set_cursor_shape_in* in =
            (const aerogpu_escape_set_cursor_shape_in*)pEscape->pPrivateDriverData;

        /* Validate that the buffer contains `pitch_bytes * height` pixel bytes. */
        const ULONGLONG pitch = (ULONGLONG)in->pitch_bytes;
        const ULONGLONG height = (ULONGLONG)in->height;
        if (pitch == 0 || height == 0) {
            return STATUS_INVALID_PARAMETER;
        }
        if (pitch > (~0ull / height)) {
            return STATUS_INVALID_PARAMETER;
        }
        const ULONGLONG pixelBytes = pitch * height;
        if (pixelBytes > (~0ull - (ULONGLONG)headerBytes)) {
            return STATUS_INVALID_PARAMETER;
        }
        if ((ULONGLONG)pEscape->PrivateDriverDataSize < (ULONGLONG)headerBytes + pixelBytes) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        DXGKARG_SETPOINTERSHAPE shape;
        RtlZeroMemory(&shape, sizeof(shape));
        shape.VidPnSourceId = AEROGPU_VIDPN_SOURCE_ID;
        shape.Width = (ULONG)in->width;
        shape.Height = (ULONG)in->height;
        shape.XHot = (ULONG)in->hot_x;
        shape.YHot = (ULONG)in->hot_y;
        shape.Pitch = (ULONG)in->pitch_bytes;
        shape.pPixels = (PVOID)in->pixels;
        shape.Flags.Value = 0;
        shape.Flags.Color = 1;

        return AeroGpuDdiSetPointerShape(hAdapter, &shape);
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_READ_GPA) {
        if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
            return STATUS_INVALID_DEVICE_STATE;
        }

        if (!AeroGpuDbgctlReadGpaRegistryEnabled(adapter)) {
            AEROGPU_LOG_RATELIMITED(g_AeroGpuBlockedReadGpaEscapeCount,
                                    4,
                                    "blocked dbgctl escape READ_GPA (EnableReadGpaEscape=0) pid=%p",
                                    PsGetCurrentProcessId());
            return STATUS_NOT_SUPPORTED;
        }

        if (!AeroGpuDbgctlCallerIsAdminOrSeDebug(ExGetPreviousMode())) {
            AEROGPU_LOG_RATELIMITED(g_AeroGpuBlockedReadGpaEscapeCount,
                                    4,
                                    "blocked dbgctl escape READ_GPA (caller not admin/SeDebug) pid=%p",
                                    PsGetCurrentProcessId());
            return STATUS_NOT_SUPPORTED;
        }

        aerogpu_escape_read_gpa_inout* io = (aerogpu_escape_read_gpa_inout*)pEscape->pPrivateDriverData;
        if (pEscape->PrivateDriverDataSize != sizeof(*io)) {
            return STATUS_INVALID_PARAMETER;
        }

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_READ_GPA;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;

        io->reserved0 = 0;
        io->status = (uint32_t)STATUS_INVALID_PARAMETER;
        io->bytes_copied = 0;
        RtlZeroMemory(io->data, sizeof(io->data));

        const ULONGLONG gpa = (ULONGLONG)io->gpa;
        const ULONG reqBytes = (ULONG)io->size_bytes;

        if (reqBytes == 0) {
            io->status = (uint32_t)STATUS_SUCCESS;
            return STATUS_SUCCESS;
        }
        if (reqBytes > AEROGPU_DBGCTL_READ_GPA_MAX_BYTES) {
            io->status = (uint32_t)STATUS_INVALID_PARAMETER;
            return STATUS_SUCCESS;
        }
        /* Validate `gpa .. gpa+reqBytes-1` does not overflow. */
        if (gpa > (~0ull - ((ULONGLONG)reqBytes - 1ull))) {
            io->status = (uint32_t)STATUS_INVALID_PARAMETER;
            return STATUS_SUCCESS;
        }

        /* Best-effort: if the address resolves to a driver-tracked buffer, copy from its kernel VA under lock. */
        {
            KIRQL pendingIrql;
            KeAcquireSpinLock(&adapter->PendingLock, &pendingIrql);

            BOOLEAN found = FALSE;
            NTSTATUS opSt = STATUS_SUCCESS;
            ULONG bytesToCopy = reqBytes;
            const PUCHAR out = (PUCHAR)io->data;

            if (!found) {
                found = AeroGpuTryCopyFromSubmissionList(&adapter->PendingSubmissions, gpa, reqBytes, out, &bytesToCopy, &opSt);
            }
            if (!found) {
                found = AeroGpuTryCopyFromSubmissionList(&adapter->RecentSubmissions, gpa, reqBytes, out, &bytesToCopy, &opSt);
            }

            if (!found) {
                for (PLIST_ENTRY entry = adapter->PendingInternalSubmissions.Flink;
                     entry != &adapter->PendingInternalSubmissions;
                     entry = entry->Flink) {
                    const AEROGPU_PENDING_INTERNAL_SUBMISSION* sub =
                        CONTAINING_RECORD(entry, AEROGPU_PENDING_INTERNAL_SUBMISSION, ListEntry);
                    if (!sub) {
                        continue;
                    }

                    struct range {
                        const void* Va;
                        SIZE_T Size;
                    } ranges[] = {
                        {sub->CmdVa, sub->CmdSizeBytes},
                        {sub->DescVa, sub->DescSizeBytes},
                    };
                    for (UINT i = 0; i < (UINT)(sizeof(ranges) / sizeof(ranges[0])); ++i) {
                        if (!ranges[i].Va || ranges[i].Size == 0) {
                            continue;
                        }

                        const ULONGLONG base = (ULONGLONG)MmGetPhysicalAddress((PVOID)ranges[i].Va).QuadPart;
                        const ULONGLONG size = (ULONGLONG)ranges[i].Size;
                        if (gpa < base) {
                            continue;
                        }
                        const ULONGLONG offset = gpa - base;
                        if (offset >= size) {
                            continue;
                        }
                        const ULONGLONG maxBytesU64 = size - offset;
                        bytesToCopy = (maxBytesU64 < (ULONGLONG)reqBytes) ? (ULONG)maxBytesU64 : reqBytes;
                        if (bytesToCopy != reqBytes) {
                            opSt = STATUS_PARTIAL_COPY;
                        }
                        RtlCopyMemory(out, (const PUCHAR)ranges[i].Va + (SIZE_T)offset, bytesToCopy);
                        found = TRUE;
                        break;
                    }
                    if (found) {
                        break;
                    }
                }
            }

            KeReleaseSpinLock(&adapter->PendingLock, pendingIrql);

            if (found) {
                io->status = (uint32_t)opSt;
                io->bytes_copied = (uint32_t)bytesToCopy;
                return STATUS_SUCCESS;
            }
        }

        /*
         * Driver-owned contiguous buffers with stable kernel VAs:
         * - ring buffer
         * - fence page
         *
         * Cursor framebuffer backing store is handled separately under CursorLock below.
         *
         * For scanout we fall back to physical translation via MmGetVirtualForPhysical below.
         */
        {
            /*
             * Ring and fence-page pointers can be detached/freed during teardown. Hold RingLock while
             * copying from these buffers so we never race AeroGpuRingCleanup.
             */
            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            struct range {
                ULONGLONG Base;
                ULONGLONG Size;
                const void* Va;
            } ranges[] = {
                {(ULONGLONG)adapter->RingPa.QuadPart, (ULONGLONG)adapter->RingSizeBytes, adapter->RingVa},
                {(ULONGLONG)adapter->FencePagePa.QuadPart, (ULONGLONG)PAGE_SIZE, adapter->FencePageVa},
            };

            for (UINT i = 0; i < (UINT)(sizeof(ranges) / sizeof(ranges[0])); ++i) {
                if (!ranges[i].Va || ranges[i].Size == 0) {
                    continue;
                }
                const ULONGLONG base = ranges[i].Base;
                const ULONGLONG size = ranges[i].Size;
                if (gpa < base) {
                    continue;
                }
                const ULONGLONG offset = gpa - base;
                if (offset >= size) {
                    continue;
                }

                const ULONGLONG maxBytesU64 = size - offset;
                const ULONG bytesToCopy = (maxBytesU64 < (ULONGLONG)reqBytes) ? (ULONG)maxBytesU64 : reqBytes;
                const NTSTATUS opSt = (bytesToCopy == reqBytes) ? STATUS_SUCCESS : STATUS_PARTIAL_COPY;

                RtlCopyMemory(io->data, (const PUCHAR)ranges[i].Va + (SIZE_T)offset, bytesToCopy);
                io->status = (uint32_t)opSt;
                io->bytes_copied = (uint32_t)bytesToCopy;
                KeReleaseSpinLock(&adapter->RingLock, ringIrql);
                return STATUS_SUCCESS;
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);
        }

        /* Cursor framebuffer backing store (protocol cursor regs). */
        {
            KIRQL cursorIrql;
            KeAcquireSpinLock(&adapter->CursorLock, &cursorIrql);

            if (adapter->CursorFbVa && adapter->CursorFbSizeBytes != 0) {
                const ULONGLONG base = (ULONGLONG)adapter->CursorFbPa.QuadPart;
                const ULONGLONG size = (ULONGLONG)adapter->CursorFbSizeBytes;
                const void* va = adapter->CursorFbVa;

                if (gpa >= base) {
                    const ULONGLONG offset = gpa - base;
                    if (offset < size) {
                        const ULONGLONG maxBytesU64 = size - offset;
                        const ULONG bytesToCopy =
                            (maxBytesU64 < (ULONGLONG)reqBytes) ? (ULONG)maxBytesU64 : reqBytes;
                        const NTSTATUS opSt = (bytesToCopy == reqBytes) ? STATUS_SUCCESS : STATUS_PARTIAL_COPY;

                        RtlCopyMemory(io->data, (const PUCHAR)va + (SIZE_T)offset, bytesToCopy);
                        io->status = (uint32_t)opSt;
                        io->bytes_copied = (uint32_t)bytesToCopy;
                        KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
                        return STATUS_SUCCESS;
                    }
                }
            }

            KeReleaseSpinLock(&adapter->CursorLock, cursorIrql);
        }

        /*
         * Recent ring descriptor references (AGPU): allow reads within cmd/alloc buffers referenced by the
         * most recent ring descriptors. This makes it easier to dump the most recent submission even on
         * fast devices where the pending submission list may already have been retired.
         */
        if (adapter->AbiKind == AEROGPU_ABI_KIND_V1) {
            ULONGLONG allowBase = 0;
            ULONGLONG allowSize = 0;
            BOOLEAN found = FALSE;

            KIRQL ringIrql;
            KeAcquireSpinLock(&adapter->RingLock, &ringIrql);

            /* Avoid racing teardown: re-check ring readiness under RingLock before dereferencing RingVa. */
            if (AeroGpuV1SubmitPathUsable(adapter)) {
                const struct aerogpu_ring_header* ringHeader = (const struct aerogpu_ring_header*)adapter->RingVa;
                ULONG tail = ringHeader->tail;
                ULONG window = AEROGPU_DBGCTL_MAX_RECENT_DESCRIPTORS;
                if (window > adapter->RingEntryCount) {
                    window = adapter->RingEntryCount;
                }
                if (tail < window) {
                    window = tail;
                }

                const struct aerogpu_submit_desc* ring =
                    (const struct aerogpu_submit_desc*)((PUCHAR)adapter->RingVa + sizeof(struct aerogpu_ring_header));

                for (ULONG i = 0; i < window && !found; ++i) {
                    const ULONG ringIndex = (tail - 1u - i) & (adapter->RingEntryCount - 1u);
                    const struct aerogpu_submit_desc entry = ring[ringIndex];

                    struct range {
                        ULONGLONG Base;
                        ULONGLONG Size;
                    } ranges[] = {
                        {(ULONGLONG)entry.cmd_gpa, (ULONGLONG)entry.cmd_size_bytes},
                        {(ULONGLONG)entry.alloc_table_gpa, (ULONGLONG)entry.alloc_table_size_bytes},
                    };

                    for (UINT j = 0; j < (UINT)(sizeof(ranges) / sizeof(ranges[0])); ++j) {
                        const ULONGLONG base = ranges[j].Base;
                        const ULONGLONG size = ranges[j].Size;
                        if (base == 0 || size == 0) {
                            continue;
                        }
                        if (gpa < base) {
                            continue;
                        }
                        const ULONGLONG off = gpa - base;
                        if (off >= size) {
                            continue;
                        }
                        allowBase = base;
                        allowSize = size;
                        found = TRUE;
                        break;
                    }
                }
            }

            KeReleaseSpinLock(&adapter->RingLock, ringIrql);

            if (found) {
                const ULONGLONG off = gpa - allowBase;
                const ULONGLONG maxBytesU64 = allowSize - off;
                ULONG bytesToCopy = (maxBytesU64 < (ULONGLONG)reqBytes) ? (ULONG)maxBytesU64 : reqBytes;
                NTSTATUS opSt = (bytesToCopy == reqBytes) ? STATUS_SUCCESS : STATUS_PARTIAL_COPY;

                if (!AeroGpuDbgctlValidateGpaRangeIsRam(gpa, bytesToCopy)) {
                    opSt = STATUS_INVALID_PARAMETER;
                    bytesToCopy = 0;
                } else {
                    const NTSTATUS readSt = AeroGpuDbgctlReadGpaBytes(gpa, bytesToCopy, (UCHAR*)io->data);
                    if (!NT_SUCCESS(readSt)) {
                        opSt = readSt;
                        bytesToCopy = 0;
                    }
                }

                io->status = (uint32_t)opSt;
                io->bytes_copied = (uint32_t)bytesToCopy;
                return STATUS_SUCCESS;
            }
        }

        /*
         * Scanout framebuffer (best-effort): allow reads within the cached scanout
         * region last programmed via SetVidPnSourceAddress.
         *
         * IMPORTANT: do not trust scanout MMIO registers as the source of truth
         * for authorizing READ_GPA. If the registers are corrupted or
         * misprogrammed, using them here would turn this escape into a generic
         * physical-memory read primitive.
         *
         * Also: when powered down (non-D0) or BAR0 is unmapped, do not attempt
         * scanout physical translation.
         */
        if (poweredOn) {
            const ULONGLONG fbGpa = (ULONGLONG)adapter->CurrentScanoutFbPa.QuadPart;
            const ULONG fbPitchBytes = adapter->CurrentPitch;
            const ULONG fbHeight = adapter->CurrentHeight;

            /* Derive a plausible bound for the scanout window (pitch * height). */
            ULONGLONG fbSizeBytes = 0;
            if (adapter->SourceVisible && fbGpa != 0 && fbPitchBytes != 0 && fbHeight != 0) {
                if ((ULONGLONG)fbPitchBytes <= (~0ull / (ULONGLONG)fbHeight)) {
                    fbSizeBytes = (ULONGLONG)fbPitchBytes * (ULONGLONG)fbHeight;
                }
            }

            /* Tight cap: never allow reads beyond the reported segment budget or 512 MiB. */
            const ULONGLONG segmentCap = adapter->NonLocalMemorySizeBytes;
            ULONGLONG maxAllowedBytes = (512ull * 1024ull * 1024ull);
            if (segmentCap != 0 && segmentCap < maxAllowedBytes) {
                maxAllowedBytes = segmentCap;
            }

            const BOOLEAN scanoutStateValid =
                (fbSizeBytes != 0) && (fbSizeBytes <= maxAllowedBytes) && (fbGpa <= (~0ull - fbSizeBytes));

            if (!scanoutStateValid) {
#if DBG
                static LONG s_ReadGpaScanoutInvalidStateCount = 0;
                AEROGPU_LOG_RATELIMITED(s_ReadGpaScanoutInvalidStateCount,
                                        8,
                                        "READ_GPA: scanout unavailable/invalid (visible=%u fb_gpa=0x%I64x pitch=%lu height=%lu size=%I64u max=%I64u)",
                                        (unsigned)adapter->SourceVisible,
                                        (ULONGLONG)fbGpa,
                                        (ULONG)fbPitchBytes,
                                        (ULONG)fbHeight,
                                        (ULONGLONG)fbSizeBytes,
                                        (ULONGLONG)maxAllowedBytes);
#endif
            } else if (gpa >= fbGpa && gpa < (fbGpa + fbSizeBytes)) {
                const ULONGLONG offset = gpa - fbGpa;
                const ULONGLONG maxBytesU64 = fbSizeBytes - offset;
                const ULONG bytesToCopy =
                    (maxBytesU64 < (ULONGLONG)reqBytes) ? (ULONG)maxBytesU64 : reqBytes;
                NTSTATUS opSt = (bytesToCopy == reqBytes) ? STATUS_SUCCESS : STATUS_PARTIAL_COPY;

                if (!AeroGpuDbgctlValidateGpaRangeIsRam(gpa, bytesToCopy)) {
                    io->status = (uint32_t)STATUS_INVALID_PARAMETER;
                    io->bytes_copied = 0;
                    return STATUS_SUCCESS;
                }

                ULONG copied = 0;
                ULONGLONG cur = gpa;
                while (copied < bytesToCopy) {
                    const ULONG remaining = bytesToCopy - copied;
                    const ULONG pageOff = (ULONG)(cur & (PAGE_SIZE - 1));
                    ULONG chunk = PAGE_SIZE - pageOff;
                    if (chunk > remaining) {
                        chunk = remaining;
                    }

                    PHYSICAL_ADDRESS pa;
                    pa.QuadPart = (LONGLONG)cur;
                    const PUCHAR src = (const PUCHAR)MmGetVirtualForPhysical(pa);
                    if (!src) {
                        opSt = (copied != 0) ? STATUS_PARTIAL_COPY : STATUS_UNSUCCESSFUL;
                        break;
                    }

                    __try {
                        RtlCopyMemory(io->data + copied, src, chunk);
                    } __except (EXCEPTION_EXECUTE_HANDLER) {
                        opSt = (copied != 0) ? STATUS_PARTIAL_COPY : STATUS_UNSUCCESSFUL;
                        break;
                    }

                    copied += chunk;
                    cur += (ULONGLONG)chunk;
                }

                io->status = (uint32_t)opSt;
                io->bytes_copied = (uint32_t)copied;
                return STATUS_SUCCESS;
            }
        }

        /* Not within any allowed/tracked device GPA region. */
        io->status = (uint32_t)STATUS_ACCESS_DENIED;
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_QUERY_ERROR) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_query_error_out)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_query_error_out* out = (aerogpu_escape_query_error_out*)pEscape->pPrivateDriverData;
        out->hdr.version = AEROGPU_ESCAPE_VERSION;
        out->hdr.op = AEROGPU_ESCAPE_OP_QUERY_ERROR;
        out->hdr.size = sizeof(*out);
        out->hdr.reserved0 = 0;
        out->flags = AEROGPU_DBGCTL_QUERY_ERROR_FLAGS_VALID;
        out->error_code = 0;
        out->error_fence = 0;
        out->error_count = 0;
        out->reserved0 = 0;

        /*
         * Always expose best-effort error state based on the KMD's IRQ_ERROR latch, even if the
         * device does not expose the optional MMIO error registers.
         *
         * If the MMIO error registers are present and the device is powered on, prefer those
         * for richer details.
         */
        out->flags |= AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_SUPPORTED;
        if (AeroGpuIsDeviceErrorLatched(adapter)) {
            out->flags |= AEROGPU_DBGCTL_QUERY_ERROR_FLAG_ERROR_LATCHED;
        }

        /* Avoid MMIO reads while powered down; return best-effort cached state. */
        out->error_fence = AeroGpuAtomicReadU64(&adapter->LastErrorFence);
        const ULONGLONG cachedFence = (ULONGLONG)out->error_fence;

        const ULONG cachedCode = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->LastErrorCode);
        if (cachedCode != 0) {
            out->error_code = cachedCode;
        } else if (AeroGpuIsDeviceErrorLatched(adapter)) {
            out->error_code = (ULONG)AEROGPU_ERROR_INTERNAL;
        }

        const ULONG cachedMmioCount = AeroGpuAtomicReadU32((volatile ULONG*)&adapter->LastErrorMmioCount);
        if (cachedMmioCount != 0) {
            out->error_count = cachedMmioCount;
        } else {
            const ULONGLONG errorCount = AeroGpuAtomicReadU64(&adapter->ErrorIrqCount);
            out->error_count = (errorCount > 0xFFFFFFFFull) ? 0xFFFFFFFFu : (ULONG)errorCount;
        }

        const ULONG abiMinor = (ULONG)(adapter->DeviceAbiVersion & 0xFFFFu);
        const BOOLEAN haveErrorRegs =
            (adapter->AbiKind == AEROGPU_ABI_KIND_V1) &&
            ((adapter->DeviceFeatures & AEROGPU_FEATURE_ERROR_INFO) != 0) &&
            (abiMinor >= 3) &&
            (adapter->Bar0Length >= (AEROGPU_MMIO_REG_ERROR_COUNT + sizeof(ULONG)));
        if (mmioSafe && haveErrorRegs) {
            /*
             * Prefer device-reported error payload when the adapter is in D0, but avoid wiping out
             * cached KMD telemetry with empty/invalid MMIO values (e.g. after a device reset).
             */
            const ULONG mmioCode = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_CODE);
            const ULONGLONG mmioFence = AeroGpuReadRegU64HiLoHi(adapter,
                                                               AEROGPU_MMIO_REG_ERROR_FENCE_LO,
                                                               AEROGPU_MMIO_REG_ERROR_FENCE_HI);
            const ULONG mmioCount = AeroGpuReadRegU32(adapter, AEROGPU_MMIO_REG_ERROR_COUNT);

            /*
             * Keep a best-effort cached copy of the most recent non-zero device-reported error payload.
             *
             * Normally this is captured in the IRQ_ERROR ISR path, but caching it here ensures dbgctl can
             * still report stable values after power down even if the error interrupt was masked/lost.
             *
             * Do not overwrite cached values when the device reports error_count==0 (no error
             * payload). Otherwise, keep the cached MMIO payload in sync with what we observe here
             * so powered-down QUERY_ERROR calls can still report the most recently observed error.
             */
            const BOOLEAN shouldRefreshCache =
                (mmioCount != 0) &&
                ((mmioCount != cachedMmioCount) || (mmioFence != 0 && mmioFence != cachedFence) ||
                 (mmioCode != 0 && mmioCode != cachedCode));
            if (shouldRefreshCache) {
                /*
                 * Avoid clobbering concurrent ISR/error cache updates:
                 * only refresh the cached payload if LastErrorMmioCount still matches the value we
                 * observed at the start of this escape.
                 */
                const LONG prevCount =
                    InterlockedCompareExchange((volatile LONG*)&adapter->LastErrorMmioCount, (LONG)mmioCount, (LONG)cachedMmioCount);
                if ((ULONG)prevCount == cachedMmioCount) {
                    AeroGpuAtomicWriteU64(&adapter->LastErrorTime100ns, KeQueryInterruptTime());

                    ULONG cacheCode = mmioCode;
                    if (cacheCode == 0) {
                        cacheCode = (ULONG)AEROGPU_ERROR_INTERNAL;
                    }
                    InterlockedExchange((volatile LONG*)&adapter->LastErrorCode, (LONG)cacheCode);

                    if (mmioFence != 0) {
                        AeroGpuAtomicWriteU64(&adapter->LastErrorFence, mmioFence);
                    } else if (mmioCount != cachedMmioCount) {
                        /*
                         * If this looks like a new device-reported error (ERROR_COUNT changed) but the
                         * device does not provide an associated fence (ERROR_FENCE==0), clear the cached
                         * fence so powered-down QUERY_ERROR calls do not report a stale fence from a
                         * prior error (for example if IRQ_ERROR was masked/lost).
                         *
                         * Note: when IRQ_ERROR is delivered normally, the ISR path records a best-effort
                         * LastErrorFence even without ERROR_FENCE, and also updates LastErrorMmioCount.
                         * In that common case, cachedMmioCount already matches and we do not clear it here.
                         */
                        /*
                         * Avoid clobbering a concurrent ISR update: only clear the fence if it still
                         * matches the value we observed at the start of QUERY_ERROR.
                         */
                        AeroGpuAtomicCompareExchangeU64(&adapter->LastErrorFence, 0, cachedFence);
                    }
                }
            }

            /*
             * Only trust device-provided error payload fields when error_count is non-zero.
             * This avoids reporting stale/invalid code/fence values after a device reset
             * that cleared the payload (count==0) but left other registers at arbitrary
             * values.
             */
            if (mmioCount != 0) {
                /*
                 * Prefer device-provided payload fields, but be defensive:
                 * - If ERROR_CODE is 0, preserve a previously cached non-zero code when we believe
                 *   we're observing the same payload (tolerate MMIO tearing).
                 * - If this appears to be a *new* payload (count/code/fence changed) but ERROR_CODE is
                 *   0, treat it as INTERNAL rather than reporting a stale prior code.
                 */
                if (mmioCode != 0) {
                    out->error_code = mmioCode;
                } else if (shouldRefreshCache) {
                    out->error_code = (ULONG)AEROGPU_ERROR_INTERNAL;
                } else if (out->error_code == 0) {
                    out->error_code = (ULONG)AEROGPU_ERROR_INTERNAL;
                }
                if (mmioFence != 0) {
                    out->error_fence = (uint64_t)mmioFence;
                } else if (mmioCount != cachedMmioCount) {
                    /*
                     * New error payload without an associated fence: avoid reporting a stale cached
                     * fence from a prior error.
                     */
                    const ULONGLONG currentFence = AeroGpuAtomicReadU64(&adapter->LastErrorFence);
                    out->error_fence = (currentFence != cachedFence) ? (uint64_t)currentFence : 0;
                }
                out->error_count = mmioCount;
            }
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION) {
        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_dump_createallocation_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }
        aerogpu_escape_dump_createallocation_inout* io =
            (aerogpu_escape_dump_createallocation_inout*)pEscape->pPrivateDriverData;

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;

        if (io->entry_capacity > AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS) {
            io->entry_capacity = AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS;
        }

        io->write_index = 0;
        io->entry_count = 0;
        io->reserved0 = 0;
        RtlZeroMemory(io->entries, sizeof(io->entries));

        /*
         * Avoid writing to the caller-provided output buffer while holding the
         * spin lock. While dxgkrnl typically marshals escape buffers into a
         * kernel mapping, keep the critical section minimal and copy out under
         * the lock, then format the response after releasing.
         */
        AEROGPU_CREATEALLOCATION_TRACE_ENTRY local[AEROGPU_DBGCTL_MAX_RECENT_ALLOCATIONS];
        RtlZeroMemory(local, sizeof(local));

        ULONG writeIndex = 0;
        ULONG outCount = 0;

        KIRQL oldIrql;
        KeAcquireSpinLock(&adapter->CreateAllocationTraceLock, &oldIrql);

        writeIndex = adapter->CreateAllocationTrace.WriteIndex;
        ULONG available = writeIndex;
        if (available > AEROGPU_CREATEALLOCATION_TRACE_SIZE) {
            available = AEROGPU_CREATEALLOCATION_TRACE_SIZE;
        }

        outCount = available;
        if (outCount > io->entry_capacity) {
            outCount = io->entry_capacity;
        }

        if (outCount != 0) {
            const ULONG startSeq = writeIndex - outCount;
            for (ULONG i = 0; i < outCount; ++i) {
                const ULONG seq = startSeq + i;
                const ULONG slot = seq % AEROGPU_CREATEALLOCATION_TRACE_SIZE;
                local[i] = adapter->CreateAllocationTrace.Entries[slot];
            }
        }

        KeReleaseSpinLock(&adapter->CreateAllocationTraceLock, oldIrql);

        io->write_index = writeIndex;
        io->entry_count = outCount;

        for (ULONG i = 0; i < outCount; ++i) {
            const AEROGPU_CREATEALLOCATION_TRACE_ENTRY* e = &local[i];
            aerogpu_dbgctl_createallocation_desc* out = &io->entries[i];
            out->seq = (uint32_t)e->Seq;
            out->call_seq = (uint32_t)e->CallSeq;
            out->alloc_index = (uint32_t)e->AllocIndex;
            out->num_allocations = (uint32_t)e->NumAllocations;
            out->create_flags = (uint32_t)e->CreateFlags;
            out->alloc_id = (uint32_t)e->AllocationId;
            out->priv_flags = (uint32_t)e->PrivFlags;
            out->pitch_bytes = (uint32_t)e->PitchBytes;
            out->share_token = (uint64_t)e->ShareToken;
            out->size_bytes = (uint64_t)e->SizeBytes;
            out->flags_in = (uint32_t)e->FlagsIn;
            out->flags_out = (uint32_t)e->FlagsOut;
        }
        return STATUS_SUCCESS;
    }

    if (hdr->op == AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE) {
        if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
            return STATUS_INVALID_DEVICE_STATE;
        }

        if (g_AeroGpuEnableMapSharedHandleEscape == 0) {
            AEROGPU_LOG_RATELIMITED(g_AeroGpuBlockedMapSharedHandleEscapeCount,
                                    4,
                                    "blocked dbgctl escape MAP_SHARED_HANDLE (EnableMapSharedHandleEscape=0) pid=%p",
                                    PsGetCurrentProcessId());
            return STATUS_NOT_SUPPORTED;
        }

        if (!AeroGpuDbgctlCallerIsAdminOrSeDebug(ExGetPreviousMode())) {
            AEROGPU_LOG_RATELIMITED(g_AeroGpuBlockedMapSharedHandleEscapeCount,
                                    4,
                                    "blocked dbgctl escape MAP_SHARED_HANDLE (caller not admin/SeDebug) pid=%p",
                                    PsGetCurrentProcessId());
            return STATUS_NOT_SUPPORTED;
        }

        if (pEscape->PrivateDriverDataSize < sizeof(aerogpu_escape_map_shared_handle_inout)) {
            return STATUS_BUFFER_TOO_SMALL;
        }

        aerogpu_escape_map_shared_handle_inout* io =
            (aerogpu_escape_map_shared_handle_inout*)pEscape->pPrivateDriverData;

        io->hdr.version = AEROGPU_ESCAPE_VERSION;
        io->hdr.op = AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE;
        io->hdr.size = sizeof(*io);
        io->hdr.reserved0 = 0;

        io->debug_token = 0;
        io->reserved0 = 0;

        HANDLE sharedHandle = (HANDLE)(ULONG_PTR)io->shared_handle;
        if (!sharedHandle) {
            return STATUS_INVALID_PARAMETER;
        }

        PVOID object = NULL;
        /*
         * D3D shared resource handles are expected to be section objects.
         * Restrict the referenced type so callers cannot pin unrelated kernel
         * objects via this debug escape.
         */
        NTSTATUS st = ObReferenceObjectByHandle(sharedHandle, 0, *MmSectionObjectType, UserMode, &object, NULL);
        if (!NT_SUCCESS(st)) {
            return st;
        }

        ULONG token = 0;
        BOOLEAN keepObjectRef = FALSE;
        AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* newNode = NULL;
        LIST_ENTRY evicted;
        InitializeListHead(&evicted);

        /*
         * Fast path: lookup without allocating. Keep hot entries near the tail
         * (LRU) so eviction preferentially drops cold objects.
         */
        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->SharedHandleTokenLock, &oldIrql);

            for (PLIST_ENTRY entry = adapter->SharedHandleTokens.Flink;
                 entry != &adapter->SharedHandleTokens;
                 entry = entry->Flink) {
                AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* node =
                    CONTAINING_RECORD(entry, AEROGPU_SHARED_HANDLE_TOKEN_ENTRY, ListEntry);
                if (node->Object == object) {
                    token = node->Token;

                    /* Refresh the entry's LRU position. */
                    RemoveEntryList(&node->ListEntry);
                    InsertTailList(&adapter->SharedHandleTokens, &node->ListEntry);
                    break;
                }
            }

            KeReleaseSpinLock(&adapter->SharedHandleTokenLock, oldIrql);
        }

        if (token != 0) {
            ObDereferenceObject(object);
            io->debug_token = (uint32_t)token;
            return STATUS_SUCCESS;
        }

        /* Allocate outside the spin lock to avoid DISPATCH_LEVEL pool allocs. */
        newNode = (AEROGPU_SHARED_HANDLE_TOKEN_ENTRY*)ExAllocatePoolWithTag(
            NonPagedPool, sizeof(*newNode), AEROGPU_POOL_TAG);
        if (!newNode) {
            ObDereferenceObject(object);
            return STATUS_INSUFFICIENT_RESOURCES;
        }
        RtlZeroMemory(newNode, sizeof(*newNode));

        {
            KIRQL oldIrql;
            KeAcquireSpinLock(&adapter->SharedHandleTokenLock, &oldIrql);

            /* Re-check after allocating: another thread may have inserted it. */
            for (PLIST_ENTRY entry = adapter->SharedHandleTokens.Flink;
                 entry != &adapter->SharedHandleTokens;
                 entry = entry->Flink) {
                AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* node =
                    CONTAINING_RECORD(entry, AEROGPU_SHARED_HANDLE_TOKEN_ENTRY, ListEntry);
                if (node->Object == object) {
                    token = node->Token;
                    RemoveEntryList(&node->ListEntry);
                    InsertTailList(&adapter->SharedHandleTokens, &node->ListEntry);
                    break;
                }
            }

            if (token == 0) {
                /*
                 * Enforce a hard cap to prevent unbounded kernel object pinning /
                 * NonPagedPool growth under hostile input.
                 */
                while (adapter->SharedHandleTokenCount >= AEROGPU_MAX_SHARED_HANDLE_TOKENS) {
                    if (IsListEmpty(&adapter->SharedHandleTokens)) {
                        adapter->SharedHandleTokenCount = 0;
                        break;
                    }

                    PLIST_ENTRY le = RemoveHeadList(&adapter->SharedHandleTokens);
                    AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* old =
                        CONTAINING_RECORD(le, AEROGPU_SHARED_HANDLE_TOKEN_ENTRY, ListEntry);
                    if (adapter->SharedHandleTokenCount != 0) {
                        adapter->SharedHandleTokenCount--;
                    }
                    InsertTailList(&evicted, &old->ListEntry);
                }

                if (adapter->SharedHandleTokenCount < AEROGPU_MAX_SHARED_HANDLE_TOKENS) {
                    token = ++adapter->NextSharedHandleToken;
                    if (token == 0) {
                        token = ++adapter->NextSharedHandleToken;
                    }

                    newNode->Object = object;
                    newNode->Token = token;
                    InsertTailList(&adapter->SharedHandleTokens, &newNode->ListEntry);
                    adapter->SharedHandleTokenCount++;
                    keepObjectRef = TRUE;
                } else {
                    token = 0;
                }
            }

            KeReleaseSpinLock(&adapter->SharedHandleTokenLock, oldIrql);
        }

        /* Release evicted entries outside the spin lock. */
        while (!IsListEmpty(&evicted)) {
            PLIST_ENTRY le = RemoveHeadList(&evicted);
            AEROGPU_SHARED_HANDLE_TOKEN_ENTRY* old =
                CONTAINING_RECORD(le, AEROGPU_SHARED_HANDLE_TOKEN_ENTRY, ListEntry);
            if (old->Object) {
                ObDereferenceObject(old->Object);
            }
            ExFreePoolWithTag(old, AEROGPU_POOL_TAG);
        }

        if (!keepObjectRef) {
            if (newNode) {
                ExFreePoolWithTag(newNode, AEROGPU_POOL_TAG);
            }
            ObDereferenceObject(object);
        }

        if (token == 0) {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        io->debug_token = (uint32_t)token;
        return STATUS_SUCCESS;
    }

    return STATUS_NOT_SUPPORTED;
}

/* ---- DriverEntry -------------------------------------------------------- */

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    AeroGpuLoadDisplayModeConfigFromRegistry(RegistryPath);
    AeroGpuLoadSubmitLimitsFromRegistry(RegistryPath);

    DXGK_INITIALIZATION_DATA init;
    RtlZeroMemory(&init, sizeof(init));
    init.Version = DXGKDDI_INTERFACE_VERSION_WDDM1_1;

    init.DxgkDdiAddDevice = AeroGpuDdiAddDevice;
    init.DxgkDdiStartDevice = AeroGpuDdiStartDevice;
    init.DxgkDdiStopDevice = AeroGpuDdiStopDevice;
    init.DxgkDdiStopDeviceAndReleasePostDisplayOwnership = AeroGpuDdiStopDeviceAndReleasePostDisplayOwnership;
    init.DxgkDdiSetPowerState = AeroGpuDdiSetPowerState;
    init.DxgkDdiRemoveDevice = AeroGpuDdiRemoveDevice;
    init.DxgkDdiUnload = AeroGpuDdiUnload;

    init.DxgkDdiAcquirePostDisplayOwnership = AeroGpuDdiAcquirePostDisplayOwnership;

    init.DxgkDdiQueryAdapterInfo = AeroGpuDdiQueryAdapterInfo;

    init.DxgkDdiQueryChildRelations = AeroGpuDdiQueryChildRelations;
    init.DxgkDdiQueryChildStatus = AeroGpuDdiQueryChildStatus;
    init.DxgkDdiQueryDeviceDescriptor = AeroGpuDdiQueryDeviceDescriptor;

    init.DxgkDdiIsSupportedVidPn = AeroGpuDdiIsSupportedVidPn;
    init.DxgkDdiRecommendFunctionalVidPn = AeroGpuDdiRecommendFunctionalVidPn;
    init.DxgkDdiEnumVidPnCofuncModality = AeroGpuDdiEnumVidPnCofuncModality;
    init.DxgkDdiCommitVidPn = AeroGpuDdiCommitVidPn;
    init.DxgkDdiUpdateActiveVidPnPresentPath = AeroGpuDdiUpdateActiveVidPnPresentPath;
    init.DxgkDdiQueryVidPnHardwareCapability = AeroGpuDdiQueryVidPnHardwareCapability;
    init.DxgkDdiRecommendMonitorModes = AeroGpuDdiRecommendMonitorModes;

    init.DxgkDdiSetVidPnSourceAddress = AeroGpuDdiSetVidPnSourceAddress;
    init.DxgkDdiSetVidPnSourceVisibility = AeroGpuDdiSetVidPnSourceVisibility;

    init.DxgkDdiCreateAllocation = AeroGpuDdiCreateAllocation;
    init.DxgkDdiDestroyAllocation = AeroGpuDdiDestroyAllocation;
    init.DxgkDdiDescribeAllocation = AeroGpuDdiDescribeAllocation;
    init.DxgkDdiGetStandardAllocationDriverData = AeroGpuDdiGetStandardAllocationDriverData;
    init.DxgkDdiOpenAllocation = AeroGpuDdiOpenAllocation;
    init.DxgkDdiCloseAllocation = AeroGpuDdiCloseAllocation;
    init.DxgkDdiLock = AeroGpuDdiLock;
    init.DxgkDdiUnlock = AeroGpuDdiUnlock;

    init.DxgkDdiCreateDevice = AeroGpuDdiCreateDevice;
    init.DxgkDdiDestroyDevice = AeroGpuDdiDestroyDevice;
    init.DxgkDdiCreateContext = AeroGpuDdiCreateContext;
    init.DxgkDdiDestroyContext = AeroGpuDdiDestroyContext;
    init.DxgkDdiRender = AeroGpuDdiRender;
    init.DxgkDdiPresent = AeroGpuDdiPresent;

    init.DxgkDdiBuildPagingBuffer = AeroGpuDdiBuildPagingBuffer;
    init.DxgkDdiSubmitCommand = AeroGpuDdiSubmitCommand;

    init.DxgkDdiInterruptRoutine = AeroGpuDdiInterruptRoutine;
    init.DxgkDdiControlInterrupt = AeroGpuDdiControlInterrupt;
    init.DxgkDdiDpcRoutine = AeroGpuDdiDpcRoutine;
    init.DxgkDdiGetScanLine = AeroGpuDdiGetScanLine;
    init.DxgkDdiResetFromTimeout = AeroGpuDdiResetFromTimeout;
    init.DxgkDdiRestartFromTimeout = AeroGpuDdiRestartFromTimeout;

    init.DxgkDdiSetPointerPosition = AeroGpuDdiSetPointerPosition;
    init.DxgkDdiSetPointerShape = AeroGpuDdiSetPointerShape;

    init.DxgkDdiEscape = AeroGpuDdiEscape;

    return DxgkInitialize(DriverObject, RegistryPath, &init);
}
