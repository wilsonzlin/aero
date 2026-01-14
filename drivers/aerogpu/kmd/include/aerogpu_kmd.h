#pragma once

#include <ntddk.h>
#include <d3dkmddi.h>

#include "aerogpu_log.h"
#include "aerogpu_pci.h"
#include "aerogpu_ring.h"
#include "aerogpu_wddm_alloc.h"
#include "aerogpu_legacy_abi.h"

/* Compatibility alias used by some KMD code paths. */
#define AEROGPU_PCI_MMIO_MAGIC AEROGPU_MMIO_MAGIC /* "AGPU" */

/* Driver pool tag: 'A','G','P','U' */
#define AEROGPU_POOL_TAG 'UPGA'

#define AEROGPU_CHILD_UID 1
#define AEROGPU_VIDPN_SOURCE_ID 0
#define AEROGPU_VIDPN_TARGET_ID 0
#define AEROGPU_NODE_ORDINAL 0
#define AEROGPU_ENGINE_ORDINAL 0

#define AEROGPU_SEGMENT_ID_SYSTEM 1

#define AEROGPU_RING_ENTRY_COUNT_DEFAULT 256u

#define AEROGPU_SUBMISSION_LOG_SIZE 64u
#define AEROGPU_CREATEALLOCATION_TRACE_SIZE 64u

/*
 * Maximum allowed DMA buffer size for a single submission (bytes).
 *
 * The KMD copies each incoming command buffer into physically-contiguous
 * non-cached memory. Extremely large contiguous allocations can fragment or
 * exhaust contiguous memory and destabilize the guest, so we cap the effective
 * DMA size (after any header-based shrink).
 *
 * Optional registry override (REG_DWORD, bytes):
 *   HKR\\Parameters\\MaxDmaBufferBytes
 */
#if defined(_WIN64)
#define AEROGPU_KMD_MAX_DMA_BUFFER_BYTES (32u * 1024u * 1024u) /* 32 MiB */
#else
#define AEROGPU_KMD_MAX_DMA_BUFFER_BYTES (16u * 1024u * 1024u) /* 16 MiB */
#endif
#define AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MIN (256u * 1024u)        /* 256 KiB */
#define AEROGPU_KMD_MAX_DMA_BUFFER_BYTES_MAX (256u * 1024u * 1024u) /* 256 MiB */

/*
 * Contiguous non-cached buffer pool.
 *
 * The submission hot path allocates/frees physically-contiguous buffers at a
 * high frequency (DMA copy buffers, per-submit allocation tables, legacy
 * descriptors). Repeated calls into MmAllocateContiguousMemorySpecifyCache can
 * cause contention and contiguous-memory fragmentation.
 *
 * We pool freed buffers in size classes of whole pages (1..256 pages == 4KiB..1MiB)
 * and cap the total number of bytes retained so we never pin too much contiguous
 * memory long-term.
 */
#define AEROGPU_CONTIG_POOL_MAX_PAGES 256u /* 1 MiB / 4 KiB pages */

typedef struct _AEROGPU_CONTIG_POOL {
    KSPIN_LOCK Lock;
    LIST_ENTRY FreeLists[AEROGPU_CONTIG_POOL_MAX_PAGES]; /* index 0 == 1 page */
    SIZE_T BytesRetained;

#if DBG
    /* DBG-only observability counters. */
    DECLSPEC_ALIGN(8) volatile LONGLONG Hits;
    DECLSPEC_ALIGN(8) volatile LONGLONG Misses;
    DECLSPEC_ALIGN(8) volatile LONGLONG FreesToPool;
    DECLSPEC_ALIGN(8) volatile LONGLONG FreesToOs;
    DECLSPEC_ALIGN(8) volatile LONGLONG OsAllocs;
    DECLSPEC_ALIGN(8) volatile LONGLONG OsAllocBytes;
    DECLSPEC_ALIGN(8) volatile LONGLONG OsFrees;
    DECLSPEC_ALIGN(8) volatile LONGLONG OsFreeBytes;
    DECLSPEC_ALIGN(8) volatile LONGLONG HighWatermarkBytes;
#endif
} AEROGPU_CONTIG_POOL;

/*
 * Driver-private submission types.
 *
 * Legacy ABI: encoded into `aerogpu_legacy_submission_desc_header::type`.
 * New ABI: used to derive `AEROGPU_SUBMIT_FLAG_PRESENT` via DMA private data.
 */
#define AEROGPU_SUBMIT_RENDER 1u
#define AEROGPU_SUBMIT_PRESENT 2u
#define AEROGPU_SUBMIT_PAGING 3u

/*
 * Hard cap for AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE cached mappings.
 *
 * This escape is debug/bring-up tooling only; keep the cache bounded so hostile
 * user-mode callers cannot pin arbitrary kernel objects or exhaust NonPagedPool
 * via unbounded growth.
 */
#define AEROGPU_MAX_SHARED_HANDLE_TOKENS 1024u

typedef enum _AEROGPU_ABI_KIND {
    AEROGPU_ABI_KIND_UNKNOWN = 0,
    AEROGPU_ABI_KIND_LEGACY = 1,
    AEROGPU_ABI_KIND_V1 = 2,
} AEROGPU_ABI_KIND;

typedef struct _AEROGPU_SUBMISSION_LOG_ENTRY {
    ULONGLONG Fence;
    ULONG Type;
    ULONG DmaSize;
    LARGE_INTEGER Qpc;
} AEROGPU_SUBMISSION_LOG_ENTRY;

typedef struct _AEROGPU_SUBMISSION_LOG {
    ULONG WriteIndex;
    AEROGPU_SUBMISSION_LOG_ENTRY Entries[AEROGPU_SUBMISSION_LOG_SIZE];
} AEROGPU_SUBMISSION_LOG;

/*
 * Scratch buffers used by AeroGpuBuildAllocTable.
 *
 * Building the per-submit alloc table can be a hot path under real D3D workloads.
 * Cache the temporary hash/entry buffers to avoid NonPagedPool allocation churn
 * and fragmentation on every submission.
 *
 * Protected by `Mutex`; callers must be at PASSIVE_LEVEL (BuildAllocTable takes
 * allocation CpuMapMutexes which are FAST_MUTEXes).
 */
typedef struct _AEROGPU_ALLOC_TABLE_SCRATCH {
    FAST_MUTEX Mutex;

    PVOID Block;
    SIZE_T BlockBytes;

    UINT TmpEntriesCapacity; /* number of aerogpu_alloc_entry slots */
    UINT HashCapacity;       /* number of slots in Seen* arrays; power-of-two */

    struct aerogpu_alloc_entry* TmpEntries;
    uint32_t* Seen;
    /*
     * [epoch|index] packed metadata for the Seen hash table:
     * - high 16 bits: epoch tag
     * - low  16 bits: tmpEntries index
     */
    uint32_t* SeenMeta;
    uint16_t Epoch;

#if DBG
    volatile LONG HitCount;
    volatile LONG GrowCount;
#endif
} AEROGPU_ALLOC_TABLE_SCRATCH;

typedef struct _AEROGPU_CREATEALLOCATION_TRACE_ENTRY {
    ULONG Seq;
    ULONG CallSeq;
    ULONG AllocIndex;
    ULONG NumAllocations;
    ULONG CreateFlags;
    ULONG AllocationId;
    ULONG PrivFlags;
    ULONG PitchBytes;
    ULONGLONG ShareToken;
    ULONGLONG SizeBytes;
    ULONG FlagsIn;
    ULONG FlagsOut;
} AEROGPU_CREATEALLOCATION_TRACE_ENTRY;

typedef struct _AEROGPU_CREATEALLOCATION_TRACE {
    ULONG WriteIndex;
    AEROGPU_CREATEALLOCATION_TRACE_ENTRY Entries[AEROGPU_CREATEALLOCATION_TRACE_SIZE];
} AEROGPU_CREATEALLOCATION_TRACE;

typedef struct _AEROGPU_SUBMISSION_META {
    PVOID AllocTableVa;
    PHYSICAL_ADDRESS AllocTablePa;
    UINT AllocTableSizeBytes;

    UINT AllocationCount;
    aerogpu_legacy_submission_desc_allocation Allocations[1]; /* variable length */
} AEROGPU_SUBMISSION_META;

/*
 * KMD-internal mapping from a WOW64-safe opaque ID (stored in AEROGPU_DMA_PRIV)
 * to a kernel pointer (AEROGPU_SUBMISSION_META*).
 */
typedef struct _AEROGPU_META_HANDLE_ENTRY {
    LIST_ENTRY ListEntry;
    ULONGLONG Handle;
    AEROGPU_SUBMISSION_META* Meta;
} AEROGPU_META_HANDLE_ENTRY;

/*
 * Cache entry for AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE.
 *
 * The entry holds a referenced kernel object pointer and a stable debug token.
 */
typedef struct _AEROGPU_SHARED_HANDLE_TOKEN_ENTRY {
    LIST_ENTRY ListEntry;
    PVOID Object;
    ULONG Token;
} AEROGPU_SHARED_HANDLE_TOKEN_ENTRY;

typedef struct _AEROGPU_SUBMISSION {
    LIST_ENTRY ListEntry;
    ULONGLONG Fence;

    PVOID DmaCopyVa;
    SIZE_T DmaCopySize;
    PHYSICAL_ADDRESS DmaCopyPa;

    PVOID DescVa;
    SIZE_T DescSize;
    PHYSICAL_ADDRESS DescPa;

    PVOID AllocTableVa;
    PHYSICAL_ADDRESS AllocTablePa;
    UINT AllocTableSizeBytes;
} AEROGPU_SUBMISSION;

typedef struct _AEROGPU_ALLOCATION {
    LIST_ENTRY ListEntry;
    ULONG AllocationId; /* aerogpu_wddm_alloc_priv.alloc_id */
    ULONGLONG ShareToken; /* protocol share_token (aerogpu_wddm_alloc_priv.share_token; 0 for non-shared allocations) */
    SIZE_T SizeBytes;
    ULONG Flags; /* aerogpu_wddm_alloc_priv.flags + internal KMD flags */

    /* Optional copy of UMD-provided metadata (aerogpu_wddm_alloc_priv_v2). */
    ULONG Kind; /* enum aerogpu_wddm_alloc_kind */
    ULONG Width;
    ULONG Height;
    ULONG Format;        /* DXGI_FORMAT numeric value */
    ULONG RowPitchBytes; /* bytes; 0 if unknown */

    PHYSICAL_ADDRESS LastKnownPa; /* updated from allocation lists */
    ULONG PitchBytes; /* Optional row pitch (bytes) for linear surface allocations; 0 if not applicable/unknown. */

    /*
     * CPU mapping state for DxgkDdiLock / DxgkDdiUnlock.
     *
     * Win7 D3D10/11 staging readback relies on D3DKMTLock/Unlock (invoked via
     * the runtime's LockCb/UnlockCb) returning a valid CPU VA. We implement a
     * minimal map/unmap path by temporarily mapping the allocation's backing
     * pages into the calling process.
     */
    FAST_MUTEX CpuMapMutex;
    LONG CpuMapRefCount;
    PVOID CpuMapUserVa;   /* base VA returned by MmMapLockedPagesSpecifyCache */
    PVOID CpuMapKernelVa; /* VA returned by MmMapIoSpace */
    PMDL CpuMapMdl;
    SIZE_T CpuMapSize;       /* bytes mapped (page-aligned) */
    SIZE_T CpuMapPageOffset; /* byte offset into first page */
    BOOLEAN CpuMapWritePending;

    /*
     * Allocation teardown can occur at IRQL > PASSIVE_LEVEL (e.g. via CloseAllocation/DestroyAllocation),
     * but CPU mapping resources must be released at PASSIVE_LEVEL. When required, we defer unmap/free to
     * a work item that runs in PASSIVE context.
     */
    WORK_QUEUE_ITEM DeferredFreeWorkItem;
    volatile LONG DeferredFreeQueued;
} AEROGPU_ALLOCATION;

typedef struct _AEROGPU_SHARE_TOKEN_REF {
    LIST_ENTRY ListEntry;
    ULONGLONG ShareToken;
    ULONG OpenCount;
} AEROGPU_SHARE_TOKEN_REF;

typedef struct _AEROGPU_DEVICE {
    struct _AEROGPU_ADAPTER* Adapter;
} AEROGPU_DEVICE;

typedef struct _AEROGPU_CONTEXT {
    AEROGPU_DEVICE* Device;
    /*
     * Opaque per-context ID that is forwarded into `aerogpu_submit_desc.context_id`.
     *
     * The emulator uses this to isolate per-context rendering state (D3D9 state caching assumes
     * device contexts do not interleave state). Resources remain keyed by protocol handles.
     */
    ULONG ContextId;
} AEROGPU_CONTEXT;

typedef struct _AEROGPU_ADAPTER {
    PDEVICE_OBJECT PhysicalDeviceObject;

    ULONGLONG NonLocalMemorySizeBytes;

    DXGK_START_INFO StartInfo;
    DXGKRNL_INTERFACE DxgkInterface;
    BOOLEAN InterruptRegistered; /* True once DxgkCbRegisterInterrupt succeeds. */

    /*
     * WDDM power state tracking.
     *
     * Dxgkrnl may transition the adapter between D0 and non-D0 states without
     * tearing down the device (StopDevice/StartDevice), e.g. during guest
     * sleep/hibernate or PnP disable/enable.
     *
     * Use a simple atomic "ready" gate for submission paths so we never touch
     * the ring/MMIO while powered down.
     */
    volatile LONG DevicePowerState;      /* DXGK_DEVICE_POWER_STATE */
    volatile LONG AcceptingSubmissions;  /* 1 when D0 + ring is initialized */

    PUCHAR Bar0;
    ULONG Bar0Length;

    /* Cached MMIO discovery fields (see aerogpu_pci.h). */
    ULONG DeviceMmioMagic;
    ULONG DeviceAbiVersion;

    /* Device feature bits (AEROGPU_FEATURE_* from aerogpu_pci.h). */
    ULONGLONG DeviceFeatures;
    BOOLEAN SupportsVblank;
    DXGK_INTERRUPT_TYPE VblankInterruptType;
    BOOLEAN VblankInterruptTypeValid;

    PVOID RingVa;
    PHYSICAL_ADDRESS RingPa;
    ULONG RingSizeBytes;
    ULONG RingEntryCount;
    ULONG RingTail;
    /*
     * Legacy ABI ring head/tail registers are masked indices (wrap at RingEntryCount).
     * Track monotonic head/tail sequence numbers so internal submissions (dbgctl
     * selftest) can be retired safely without relying on modulo arithmetic.
     *
     * Only meaningful when AbiKind == AEROGPU_ABI_KIND_LEGACY.
     */
    ULONG LegacyRingHeadIndex;
    ULONG LegacyRingHeadSeq;
    ULONG LegacyRingTailSeq;
    struct aerogpu_ring_header* RingHeader; /* Only when AbiKind == AEROGPU_ABI_KIND_V1 */
    struct aerogpu_fence_page* FencePageVa; /* Only when AbiKind == AEROGPU_ABI_KIND_V1 */
    PHYSICAL_ADDRESS FencePagePa;
    KSPIN_LOCK RingLock;

    KSPIN_LOCK IrqEnableLock;
    ULONG IrqEnableMask; /* Cached AEROGPU_MMIO_REG_IRQ_ENABLE value (when present). */
    volatile LONG DeviceErrorLatched; /* Set when the device signals AEROGPU_IRQ_ERROR. Cleared on StartDevice/RestartFromTimeout. */

    /*
     * Interrupt observability counters (debug/selftest).
     *
     * These are intentionally simple monotonically increasing counters (no wrap
     * handling) used by dbgctl selftest to sanity-check IRQ delivery without a
     * kernel debugger.
     */
    volatile LONG IrqIsrCount;
    volatile LONG IrqDpcCount;
    volatile LONG IrqIsrFenceCount;
    volatile LONG IrqIsrVblankCount;

    LIST_ENTRY PendingSubmissions;
    KSPIN_LOCK PendingLock;
    LIST_ENTRY PendingInternalSubmissions;
    NPAGED_LOOKASIDE_LIST PendingInternalSubmissionLookaside;

    /* Pooled contiguous buffers used by the submission hot path. */
    AEROGPU_CONTIG_POOL ContigPool;

    /*
     * Recently retired submissions kept around for dbgctl READ_GPA / post-mortem
     * dump tooling. These are driver-owned contiguous buffers (cmd stream + optional
     * alloc table) so retaining them is safe, but must be bounded to avoid
     * exhausting contiguous memory.
     *
     * Protected by PendingLock.
     */
    LIST_ENTRY RecentSubmissions;
    ULONG RecentSubmissionCount;
    DECLSPEC_ALIGN(8) ULONGLONG RecentSubmissionBytes;
    /*
     * Fence tracking.
     *
     * Note: This driver is built for both x86 and x64. On x86, plain 64-bit
     * loads/stores are not atomic and can tear (leading to bogus fence
     * comparisons/clamping and flaky dbgctl output).
     *
     * These fields are accessed from ISR/DPC paths without taking PendingLock,
     * so callers must use Interlocked*64 operations (e.g.
     * AeroGpuAtomicReadU64/AeroGpuAtomicWriteU64) even if they already hold
     * PendingLock.
     *
     * Interlocked*64 requires 8-byte alignment, so these fields are explicitly
     * aligned.
    */
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastSubmittedFence;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastCompletedFence;

    /*
     * v1 fence extension state (Win7/WDDM 1.1 32-bit SubmissionFenceId -> AeroGPU v1 64-bit fences).
     *
     * Protected by PendingLock.
     */
    ULONG V1FenceEpoch;
    ULONG V1LastFence32;

    /* ---- dbgctl performance/health counters ------------------------------ */

    /* Monotonic counters updated via interlocked operations. */
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfTotalSubmissions;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfTotalPresents;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfTotalRenderSubmits;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfTotalInternalSubmits;

    DECLSPEC_ALIGN(8) volatile LONGLONG PerfIrqFenceDelivered;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfIrqVblankDelivered;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfIrqSpurious;

    /* DBG-only: GetScanLine telemetry (cache hits vs MMIO polling). */
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfGetScanLineCacheHits;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfGetScanLineMmioPolls;

    DECLSPEC_ALIGN(8) volatile LONGLONG PerfResetFromTimeoutCount;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfLastResetTime100ns;

    DECLSPEC_ALIGN(8) volatile LONGLONG PerfRingPushFailures;

    /* Submit-path contiguous allocation pool counters. */
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfContigPoolHit;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfContigPoolMiss;
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfContigPoolBytesSaved;

    /* dbgctl selftest statistics (AEROGPU_ESCAPE_OP_SELFTEST). */
    DECLSPEC_ALIGN(8) volatile LONGLONG PerfSelftestCount;
    volatile LONG PerfSelftestLastErrorCode; /* enum aerogpu_dbgctl_selftest_error */

    /*
     * Sticky error IRQ tracking.
     *
     * `AEROGPU_IRQ_ERROR` indicates the device/backend rejected or failed a submission. We
     * surface this to dxgkrnl as a DMA fault and also keep lightweight counters so repeated
     * failures are visible via dbgctl escapes without requiring kernel debugging.
     *
     * Counters are best-effort and monotonic for the lifetime of the adapter.
     *
     * Note: These are 64-bit fields accessed cross-thread (ISR/DPC + dbgctl). On
     * x86, plain 64-bit loads/stores can tear; use interlocked operations.
     */
    DECLSPEC_ALIGN(8) volatile ULONGLONG ErrorIrqCount;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastErrorFence;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastNotifiedErrorFence;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastErrorTime100ns; /* KeQueryInterruptTime() at last IRQ_ERROR */
    volatile ULONG LastErrorCode;      /* enum aerogpu_error_code (0 if unknown / not supported). */
    volatile ULONG LastErrorMmioCount; /* Cached AEROGPU_MMIO_REG_ERROR_COUNT (0 if unknown / not supported). */

    LIST_ENTRY PendingMetaHandles;
    /*
     * Bookkeeping for PendingMetaHandles.
     *
     * Pending meta handles are produced by DxgkDdiRender/DxgkDdiPresent and
     * consumed by DxgkDdiSubmitCommand. Under pathological call patterns (or if
     * submits fail to arrive), this list can otherwise grow without bound and
     * exhaust nonpaged resources. The KMD enforces hard caps (count + bytes) to
     * keep this bounded.
     *
     * Protected by MetaHandleLock.
     */
    ULONG PendingMetaHandleCount;
    DECLSPEC_ALIGN(8) ULONGLONG PendingMetaHandleBytes;
    KSPIN_LOCK MetaHandleLock;
    ULONGLONG NextMetaHandle;

    LIST_ENTRY Allocations;
    KSPIN_LOCK AllocationsLock;
    LIST_ENTRY ShareTokenRefs;
    /*
     * Atomic alloc_id generator for non-AeroGPU (kernel/runtime) allocations
     * that do not carry an AeroGPU private-data blob.
     *
     * The counter is initialised so that the first generated ID is
     * AEROGPU_WDDM_ALLOC_ID_KMD_MIN, keeping the namespace split described in
    * `aerogpu_wddm_alloc.h`.
     */
    volatile LONG NextKmdAllocId;
    DECLSPEC_ALIGN(8) volatile LONGLONG NextShareToken;

    /*
     * Monotonic generator for `AEROGPU_CONTEXT::ContextId` values.
     *
     * Starts at 0 so the first allocated ID is 1 (0 is reserved to mean "unknown/default" on the
     * host side).
     */
    volatile LONG NextContextId;

    AEROGPU_ABI_KIND AbiKind;

    LIST_ENTRY SharedHandleTokens;
    KSPIN_LOCK SharedHandleTokenLock;
    ULONG NextSharedHandleToken;
    ULONG SharedHandleTokenCount;

    /* Current mode (programmed via CommitVidPn / SetVidPnSourceAddress). */
    ULONG CurrentWidth;
    ULONG CurrentHeight;
    ULONG CurrentPitch;
    ULONG CurrentFormat; /* enum aerogpu_format */
    PHYSICAL_ADDRESS CurrentScanoutFbPa;
    BOOLEAN SourceVisible;
    BOOLEAN UsingNewAbi;

    /* ---- Hardware cursor (protocol cursor regs) ------------------------- */
    /*
     * Cursor backing store in guest physical memory.
     *
     * This is driver-managed system memory (non-paged, physically contiguous) and is
     * programmed into AEROGPU_MMIO_REG_CURSOR_FB_GPA_{LO,HI} so the emulator can
     * DMA it during scanout composition.
     */
    KSPIN_LOCK CursorLock;
    PVOID CursorFbVa;
    PHYSICAL_ADDRESS CursorFbPa;
    SIZE_T CursorFbSizeBytes;
    ULONG CursorWidth;
    ULONG CursorHeight;
    ULONG CursorPitchBytes;
    ULONG CursorFormat; /* enum aerogpu_format */
    ULONG CursorHotX;
    ULONG CursorHotY;
    LONG CursorX;
    LONG CursorY;
    BOOLEAN CursorVisible;
    BOOLEAN CursorShapeValid;

    /*
     * Post-display ownership state (WDDM 1.1 Acquire/Release callbacks).
     *
     * When dxgkrnl asks us to release post-display ownership, we disable scanout
     * and vblank IRQ delivery. When ownership is reacquired, we restore scanout
     * register programming and re-enable vblank IRQs if they were previously
     * enabled by dxgkrnl.
     */
    BOOLEAN PostDisplayOwnershipReleased;
    BOOLEAN PostDisplayVblankWasEnabled;

    /* VBlank / scanline estimation state (see DxgkDdiGetScanLine). */
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastVblankSeq;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastVblankTimeNs;
    DECLSPEC_ALIGN(8) volatile ULONGLONG LastVblankInterruptTime100ns;
    ULONG VblankPeriodNs;

    /*
     * Recent CreateAllocation trace.
     *
     * Used by the dbgctl escape `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` to expose
     * the incoming/outgoing `DXGK_ALLOCATIONINFO::Flags.Value` bits without
     * requiring kernel debugging.
     */
    KSPIN_LOCK CreateAllocationTraceLock;
    volatile LONG CreateAllocationCallSeq;
    AEROGPU_CREATEALLOCATION_TRACE CreateAllocationTrace;

    AEROGPU_SUBMISSION_LOG SubmissionLog;

    AEROGPU_ALLOC_TABLE_SCRATCH AllocTableScratch;
} AEROGPU_ADAPTER;

/*
 * Interlocked*64 requires 8-byte aligned targets on x86. These static asserts
 * protect against future struct layout changes that could break atomic fence
 * state reads/writes.
 */
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, LastSubmittedFence) & 7u) == 0);
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, LastCompletedFence) & 7u) == 0);
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, ErrorIrqCount) & 7u) == 0);
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, LastErrorFence) & 7u) == 0);
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, LastNotifiedErrorFence) & 7u) == 0);
C_ASSERT((FIELD_OFFSET(AEROGPU_ADAPTER, LastErrorTime100ns) & 7u) == 0);

static __forceinline ULONG AeroGpuReadRegU32(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG Offset)
{
    return READ_REGISTER_ULONG((volatile ULONG*)(Adapter->Bar0 + Offset));
}

static __forceinline VOID AeroGpuWriteRegU32(_In_ const AEROGPU_ADAPTER* Adapter, _In_ ULONG Offset, _In_ ULONG Value)
{
    WRITE_REGISTER_ULONG((volatile ULONG*)(Adapter->Bar0 + Offset), Value);
}
