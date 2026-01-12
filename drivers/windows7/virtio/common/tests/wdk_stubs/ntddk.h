/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Minimal host-buildable ntddk.h stub for unit testing WDM helpers.
 *
 * This is NOT a complete WDK replacement. It only defines what
 * virtio_pci_intx_wdm.{h,c} requires.
 */

#pragma once

#include <stdint.h>
#include <stddef.h>
#include <string.h>

/* Basic WDK-like types */
typedef void VOID;
typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef int32_t LONG;
typedef UCHAR BOOLEAN;
typedef void* PVOID;

#ifndef TRUE
#define TRUE ((BOOLEAN)1u)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0u)
#endif

/* NTSTATUS */
typedef int32_t NTSTATUS;

#define STATUS_SUCCESS ((NTSTATUS)0x00000000)
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000Du)
#define STATUS_INVALID_DEVICE_STATE ((NTSTATUS)0xC0000184u)
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBu)
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009Au)

#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)

/* IRQL */
typedef uint8_t KIRQL;
typedef uintptr_t KAFFINITY;

#define PASSIVE_LEVEL ((KIRQL)0u)
#define DISPATCH_LEVEL ((KIRQL)2u)

/* Processor mode */
typedef enum _KPROCESSOR_MODE {
    KernelMode = 0,
    UserMode = 1,
} KPROCESSOR_MODE;

/* LARGE_INTEGER */
typedef struct _LARGE_INTEGER {
    int64_t QuadPart;
} LARGE_INTEGER, *PLARGE_INTEGER;

/* Device object (opaque in tests). */
typedef struct _DEVICE_OBJECT {
    int unused;
} DEVICE_OBJECT, *PDEVICE_OBJECT;

/* Interrupt descriptor bits */
#define CmResourceTypeInterrupt 2u
#define CM_RESOURCE_INTERRUPT_LATCHED 0x0001u
#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004u
#endif

#define IO_NO_INCREMENT 0

/* SAL annotations -> empty for host build */
#define _In_
#define _Inout_
#define _In_opt_
#define _Inout_opt_
#define _Out_
#define _Out_opt_
#define _IRQL_requires_max_(x)

/* Misc helpers/macros */
#define UNREFERENCED_PARAMETER(P) ((VOID)(P))

/* Always-on ASSERT for host tests (do not depend on NDEBUG). */
#define ASSERT(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            __builtin_trap();                                                                                             \
        }                                                                                                                \
    } while (0)

#define RtlZeroMemory(Destination, Length) (void)memset((Destination), 0, (Length))

/* __forceinline used by the production code for small helpers. */
#ifndef __forceinline
#if defined(__GNUC__) || defined(__clang__)
#define __forceinline __inline__ __attribute__((always_inline))
#else
#define __forceinline inline
#endif
#endif

/* Register access: virtio ISR status is read-to-clear (ACK). */
static __forceinline UCHAR READ_REGISTER_UCHAR(volatile UCHAR* Register)
{
    UCHAR v = *Register;
    *Register = 0;
    return v;
}

/* Interlocked primitives (single-process host tests). */
static __forceinline LONG InterlockedIncrement(volatile LONG* Addend)
{
    return __atomic_add_fetch((LONG*)Addend, 1, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedDecrement(volatile LONG* Addend)
{
    return __atomic_sub_fetch((LONG*)Addend, 1, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedExchange(volatile LONG* Target, LONG Value)
{
    return __atomic_exchange_n((LONG*)Target, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedOr(volatile LONG* Destination, LONG Value)
{
    return __atomic_fetch_or((LONG*)Destination, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedCompareExchange(volatile LONG* Destination, LONG Exchange, LONG Comperand)
{
    LONG expected = Comperand;
    (void)__atomic_compare_exchange_n((LONG*)Destination, &expected, Exchange, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return expected;
}

/* KINTERRUPT */
typedef struct _KINTERRUPT KINTERRUPT, *PKINTERRUPT;

typedef BOOLEAN (*PKSERVICE_ROUTINE)(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext);

/* KDPC */
typedef struct _KDPC KDPC, *PKDPC;

typedef VOID (*PKDEFERRED_ROUTINE)(_In_ PKDPC Dpc,
                                  _In_ PVOID DeferredContext,
                                  _In_opt_ PVOID SystemArgument1,
                                  _In_opt_ PVOID SystemArgument2);

struct _KDPC {
    PKDEFERRED_ROUTINE DeferredRoutine;
    PVOID DeferredContext;
    PVOID SystemArgument1;
    PVOID SystemArgument2;
    BOOLEAN Inserted;
};

struct _KINTERRUPT {
    PKSERVICE_ROUTINE ServiceRoutine;
    PVOID ServiceContext;
};

typedef enum _KINTERRUPT_MODE {
    LevelSensitive = 0,
    Latched = 1,
} KINTERRUPT_MODE;

/* CM_PARTIAL_RESOURCE_DESCRIPTOR (minimal interrupt subset). */
typedef struct _CM_PARTIAL_RESOURCE_DESCRIPTOR {
    UCHAR Type;
    UCHAR ShareDisposition;
    USHORT Flags;
    union {
        struct {
            ULONG Vector;
            ULONG Level;
            ULONG Affinity;
        } Interrupt;
    } u;
} CM_PARTIAL_RESOURCE_DESCRIPTOR, *PCM_PARTIAL_RESOURCE_DESCRIPTOR;

/* Stubbed WDK routines implemented in wdk_stubs.c */
NTSTATUS IoConnectInterrupt(_Out_ PKINTERRUPT* InterruptObject,
                            _In_ PKSERVICE_ROUTINE ServiceRoutine,
                            _In_ PVOID ServiceContext,
                            _In_opt_ PVOID SpinLock,
                            _In_ ULONG Vector,
                            _In_ KIRQL Irql,
                            _In_ KIRQL SynchronizeIrql,
                            _In_ KINTERRUPT_MODE InterruptMode,
                            _In_ BOOLEAN ShareVector,
                            _In_ KAFFINITY ProcessorEnableMask,
                            _In_ BOOLEAN FloatingSave);

VOID IoDisconnectInterrupt(_In_ PKINTERRUPT InterruptObject);

VOID KeInitializeDpc(_Out_ PKDPC Dpc, _In_ PKDEFERRED_ROUTINE DeferredRoutine, _In_opt_ PVOID DeferredContext);
BOOLEAN KeInsertQueueDpc(_Inout_ PKDPC Dpc, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);
BOOLEAN KeRemoveQueueDpc(_Inout_ PKDPC Dpc);

KIRQL KeGetCurrentIrql(VOID);

NTSTATUS KeDelayExecutionThread(_In_ KPROCESSOR_MODE WaitMode, _In_ BOOLEAN Alertable, _In_opt_ PLARGE_INTEGER Interval);

/*
 * Test-only helpers for driving the stubs deterministically.
 *
 * These are not part of the real WDK API, but are used by host tests to invoke
 * "hardware" events.
 */
BOOLEAN WdkTestTriggerInterrupt(_In_ PKINTERRUPT InterruptObject);
BOOLEAN WdkTestRunQueuedDpc(_Inout_ PKDPC Dpc);
