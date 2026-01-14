#pragma once

/*
 * Extremely small subset of the Windows KMDF `wdf.h` needed to compile and run
 * `drivers/windows/virtio/kmdf/virtio_pci_interrupts.c` as a host-side unit test
 * binary (Linux CI).
 *
 * This intentionally stubs only the types and APIs used by the interrupt helper.
 */

#include <stdlib.h>
#include <string.h>

#include <ntddk.h>

/* Generic WDF object handle type */
typedef void* WDFOBJECT;

/*
 * Host-test instrumentation
 *
 * Use a single monotonically increasing sequence so tests can reason about
 * relative ordering of Acquire/Release calls across different spinlocks.
 */
extern ULONGLONG WdfTestSpinLockSequence;

/* Forward declarations for typed handles (opaque in real KMDF). */
typedef struct WDFDEVICE__* WDFDEVICE;
typedef struct WDFINTERRUPT__* WDFINTERRUPT;
typedef struct WDFSPINLOCK__* WDFSPINLOCK;
typedef struct WDFMEMORY__* WDFMEMORY;

typedef struct _WDF_OBJECT_ATTRIBUTES {
    WDFOBJECT ParentObject;
    size_t ContextSize;
} WDF_OBJECT_ATTRIBUTES, *PWDF_OBJECT_ATTRIBUTES;

typedef struct _WDF_INTERRUPT_INFO {
    ULONG MessageNumber;
} WDF_INTERRUPT_INFO, *PWDF_INTERRUPT_INFO;

typedef BOOLEAN (*PFN_WDF_INTERRUPT_ISR)(_In_ WDFINTERRUPT Interrupt, _In_ ULONG MessageID);
typedef VOID (*PFN_WDF_INTERRUPT_DPC)(_In_ WDFINTERRUPT Interrupt, _In_ WDFOBJECT AssociatedObject);

typedef struct _WDF_INTERRUPT_CONFIG {
    PFN_WDF_INTERRUPT_ISR EvtInterruptIsr;
    PFN_WDF_INTERRUPT_DPC EvtInterruptDpc;

    /* Raw/translated descriptors (passed through, not interpreted by stubs). */
    struct _CM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptRaw;
    struct _CM_PARTIAL_RESOURCE_DESCRIPTOR* InterruptTranslated;

    BOOLEAN AutomaticSerialization;

    BOOLEAN MessageSignaled;
    ULONG MessageNumber;
} WDF_INTERRUPT_CONFIG, *PWDF_INTERRUPT_CONFIG;

/* CM_RESOURCE / PnP resource stubs */

typedef enum _CM_RESOURCE_TYPE {
    CmResourceTypeNull = 0,
    CmResourceTypePort = 1,
    CmResourceTypeInterrupt = 2,
} CM_RESOURCE_TYPE;

#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE ((USHORT)0x0004u)
#endif

typedef struct _CM_PARTIAL_RESOURCE_DESCRIPTOR {
    UCHAR Type;
    UCHAR ShareDisposition;
    USHORT Flags;
    union {
        struct {
            ULONG MessageCount;
        } MessageInterrupt;
    } u;
} CM_PARTIAL_RESOURCE_DESCRIPTOR, *PCM_PARTIAL_RESOURCE_DESCRIPTOR;

typedef struct _WDFCMRESLIST__ {
    ULONG Count;
    CM_PARTIAL_RESOURCE_DESCRIPTOR* Descriptors;
} WDFCMRESLIST__, *WDFCMRESLIST;

/* Object model stubs (enough for parent-child deletion + contexts). */

typedef enum _WDF_OBJECT_TYPE {
    WdfObjectTypeInvalid = 0,
    WdfObjectTypeDevice,
    WdfObjectTypeInterrupt,
    WdfObjectTypeSpinLock,
    WdfObjectTypeMemory,
} WDF_OBJECT_TYPE;

typedef struct _WDF_OBJECT_HEADER {
    WDF_OBJECT_TYPE Type;
    struct _WDF_OBJECT_HEADER* Parent;
    struct _WDF_OBJECT_HEADER* FirstChild;
    struct _WDF_OBJECT_HEADER* NextSibling;
    void* Context;
    size_t ContextSize;
} WDF_OBJECT_HEADER;

struct WDFDEVICE__ {
    WDF_OBJECT_HEADER Header;
};

struct WDFSPINLOCK__ {
    WDF_OBJECT_HEADER Header;
    ULONG AcquireCalls;
    ULONG ReleaseCalls;
    ULONGLONG LastAcquireSequence;
    ULONGLONG LastReleaseSequence;
    BOOLEAN Held;
};

struct WDFMEMORY__ {
    WDF_OBJECT_HEADER Header;
    void* Buffer;
    size_t Size;
};

struct WDFINTERRUPT__ {
    WDF_OBJECT_HEADER Header;

    WDFDEVICE Device;
    PFN_WDF_INTERRUPT_ISR Isr;
    PFN_WDF_INTERRUPT_DPC Dpc;

    BOOLEAN MessageSignaled;
    ULONG MessageNumber;

    BOOLEAN Enabled;
    ULONG DisableCalls;
    ULONG EnableCalls;

    /* Host test scheduling state. */
    BOOLEAN DpcQueued;
    ULONG DpcQueueCalls;
};

static __forceinline void WdfStubAttachChild(_Inout_ WDF_OBJECT_HEADER* Parent, _Inout_ WDF_OBJECT_HEADER* Child)
{
    Child->Parent = Parent;
    Child->NextSibling = Parent->FirstChild;
    Parent->FirstChild = Child;
}

static __forceinline void WdfStubDetachFromParent(_Inout_ WDF_OBJECT_HEADER* Obj)
{
    WDF_OBJECT_HEADER* parent = Obj->Parent;
    WDF_OBJECT_HEADER** it;

    if (parent == NULL) {
        return;
    }

    it = &parent->FirstChild;
    while (*it != NULL) {
        if (*it == Obj) {
            *it = Obj->NextSibling;
            break;
        }
        it = &(*it)->NextSibling;
    }

    Obj->Parent = NULL;
    Obj->NextSibling = NULL;
}

static __forceinline void* WdfStubAllocObject(_In_ WDF_OBJECT_TYPE Type, _In_ size_t ObjectSize, _In_opt_ const WDF_OBJECT_ATTRIBUTES* Attributes)
{
    WDF_OBJECT_HEADER* hdr = (WDF_OBJECT_HEADER*)calloc(1, ObjectSize);
    if (hdr == NULL) {
        return NULL;
    }

    hdr->Type = Type;

    if (Attributes != NULL) {
        hdr->ContextSize = Attributes->ContextSize;
        if (hdr->ContextSize != 0) {
            hdr->Context = calloc(1, hdr->ContextSize);
            if (hdr->Context == NULL) {
                free(hdr);
                return NULL;
            }
        }

        if (Attributes->ParentObject != NULL) {
            WdfStubAttachChild((WDF_OBJECT_HEADER*)Attributes->ParentObject, hdr);
        }
    }

    return hdr;
}

static inline void WdfObjectDelete(_In_opt_ WDFOBJECT Object)
{
    WDF_OBJECT_HEADER* hdr;

    if (Object == NULL) {
        return;
    }

    hdr = (WDF_OBJECT_HEADER*)Object;

    /* Delete children first (KMDF-style parent deletion). */
    while (hdr->FirstChild != NULL) {
        WdfObjectDelete((WDFOBJECT)hdr->FirstChild);
    }

    WdfStubDetachFromParent(hdr);

    if (hdr->Context != NULL) {
        free(hdr->Context);
        hdr->Context = NULL;
        hdr->ContextSize = 0;
    }

    if (hdr->Type == WdfObjectTypeMemory) {
        WDFMEMORY mem = (WDFMEMORY)Object;
        free(mem->Buffer);
        mem->Buffer = NULL;
        mem->Size = 0;
    }

    free(hdr);
}

static __forceinline WDFDEVICE WdfTestCreateDevice(void)
{
    return (WDFDEVICE)WdfStubAllocObject(WdfObjectTypeDevice, sizeof(struct WDFDEVICE__), NULL);
}

static __forceinline void WdfTestDestroyDevice(_In_opt_ WDFDEVICE Device)
{
    WdfObjectDelete((WDFOBJECT)Device);
}

/* Context helpers/macros */

static __forceinline void* WdfObjectGetContext(_In_ WDFOBJECT Object)
{
    WDF_OBJECT_HEADER* hdr = (WDF_OBJECT_HEADER*)Object;
    return hdr->Context;
}

#define WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(ContextType, FunctionName)                                                      \
    static __forceinline ContextType* FunctionName(_In_ WDFOBJECT Handle)                                                  \
    {                                                                                                                      \
        return (ContextType*)WdfObjectGetContext(Handle);                                                                  \
    }

/* Init macros */

#define WDF_OBJECT_ATTRIBUTES_INIT(Attributes) (VOID)memset((Attributes), 0, sizeof(*(Attributes)))

#define WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(Attributes, ContextType)                                                   \
    do {                                                                                                                   \
        WDF_OBJECT_ATTRIBUTES_INIT((Attributes));                                                                          \
        (Attributes)->ContextSize = sizeof(ContextType);                                                                   \
    } while (0)

#define WDF_INTERRUPT_CONFIG_INIT(Config, Isr, Dpc)                                                                        \
    do {                                                                                                                   \
        (VOID)memset((Config), 0, sizeof(*(Config)));                                                                      \
        (Config)->EvtInterruptIsr = (Isr);                                                                                 \
        (Config)->EvtInterruptDpc = (Dpc);                                                                                 \
    } while (0)

#define WDF_INTERRUPT_INFO_INIT(Info) (VOID)memset((Info), 0, sizeof(*(Info)))

/* Resource list accessors */

static __forceinline ULONG WdfCmResourceListGetCount(_In_opt_ WDFCMRESLIST List)
{
    return (List == NULL) ? 0 : List->Count;
}

static __forceinline PCM_PARTIAL_RESOURCE_DESCRIPTOR WdfCmResourceListGetDescriptor(_In_opt_ WDFCMRESLIST List, _In_ ULONG Index)
{
    if (List == NULL || List->Descriptors == NULL || Index >= List->Count) {
        return NULL;
    }
    return &List->Descriptors[Index];
}

/* Spinlock stubs */

static __forceinline NTSTATUS WdfSpinLockCreate(_In_opt_ const WDF_OBJECT_ATTRIBUTES* Attributes, _Out_ WDFSPINLOCK* SpinLock)
{
    WDFSPINLOCK lock;

    if (SpinLock == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    lock = (WDFSPINLOCK)WdfStubAllocObject(WdfObjectTypeSpinLock, sizeof(struct WDFSPINLOCK__), Attributes);
    if (lock == NULL) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    *SpinLock = lock;
    return STATUS_SUCCESS;
}

static __forceinline VOID WdfSpinLockAcquire(_In_opt_ WDFSPINLOCK SpinLock)
{
    if (SpinLock == NULL) {
        return;
    }

    SpinLock->AcquireCalls++;
    SpinLock->Held = TRUE;
    SpinLock->LastAcquireSequence = ++WdfTestSpinLockSequence;
}

static __forceinline VOID WdfSpinLockRelease(_In_opt_ WDFSPINLOCK SpinLock)
{
    if (SpinLock == NULL) {
        return;
    }

    SpinLock->ReleaseCalls++;
    SpinLock->Held = FALSE;
    SpinLock->LastReleaseSequence = ++WdfTestSpinLockSequence;
}

/* Memory stubs */

static __forceinline NTSTATUS WdfMemoryCreate(
    _In_opt_ const WDF_OBJECT_ATTRIBUTES* Attributes,
    _In_ POOL_TYPE PoolType,
    _In_ ULONG PoolTag,
    _In_ size_t BufferSize,
    _Out_ WDFMEMORY* Memory,
    _Out_opt_ PVOID* Buffer)
{
    WDFMEMORY mem;

    UNREFERENCED_PARAMETER(PoolType);
    UNREFERENCED_PARAMETER(PoolTag);

    if (Memory == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    mem = (WDFMEMORY)WdfStubAllocObject(WdfObjectTypeMemory, sizeof(struct WDFMEMORY__), Attributes);
    if (mem == NULL) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    mem->Buffer = calloc(1, BufferSize);
    if (mem->Buffer == NULL) {
        WdfObjectDelete(mem);
        return STATUS_DEVICE_HARDWARE_ERROR;
    }
    mem->Size = BufferSize;

    *Memory = mem;
    if (Buffer != NULL) {
        *Buffer = mem->Buffer;
    }

    return STATUS_SUCCESS;
}

/* Interrupt stubs */

static __forceinline NTSTATUS WdfInterruptCreate(
    _In_ WDFDEVICE Device,
    _In_ const WDF_INTERRUPT_CONFIG* Config,
    _In_opt_ const WDF_OBJECT_ATTRIBUTES* Attributes,
    _Out_ WDFINTERRUPT* Interrupt)
{
    WDFINTERRUPT intr;

    if (Interrupt == NULL || Config == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    intr = (WDFINTERRUPT)WdfStubAllocObject(WdfObjectTypeInterrupt, sizeof(struct WDFINTERRUPT__), Attributes);
    if (intr == NULL) {
        return STATUS_DEVICE_HARDWARE_ERROR;
    }

    intr->Device = Device;
    intr->Isr = Config->EvtInterruptIsr;
    intr->Dpc = Config->EvtInterruptDpc;
    intr->MessageSignaled = Config->MessageSignaled;
    intr->MessageNumber = Config->MessageNumber;
    intr->Enabled = TRUE;
    intr->DpcQueued = FALSE;
    intr->DpcQueueCalls = 0;

    *Interrupt = intr;
    return STATUS_SUCCESS;
}

static __forceinline NTSTATUS WdfInterruptDisable(_In_ WDFINTERRUPT Interrupt)
{
    if (Interrupt == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    Interrupt->DisableCalls++;
    Interrupt->Enabled = FALSE;
    return STATUS_SUCCESS;
}

static __forceinline NTSTATUS WdfInterruptEnable(_In_ WDFINTERRUPT Interrupt)
{
    if (Interrupt == NULL) {
        return STATUS_INVALID_PARAMETER;
    }
    Interrupt->EnableCalls++;
    Interrupt->Enabled = TRUE;
    return STATUS_SUCCESS;
}

static __forceinline VOID WdfInterruptQueueDpcForIsr(_In_ WDFINTERRUPT Interrupt)
{
    if (Interrupt == NULL) {
        return;
    }
    Interrupt->DpcQueueCalls++;
    Interrupt->DpcQueued = TRUE;
}

static __forceinline VOID WdfInterruptGetInfo(_In_ WDFINTERRUPT Interrupt, _Inout_ WDF_INTERRUPT_INFO* Info)
{
    if (Interrupt == NULL || Info == NULL) {
        return;
    }

    Info->MessageNumber = Interrupt->MessageNumber;
}

/*
 * Host-test helper: run the queued DPC synchronously.
 *
 * KMDF normally schedules DPCs asynchronously; the host tests model scheduling
 * by letting the test explicitly flush the pending work.
 */
static __forceinline VOID WdfTestInterruptRunDpc(_In_ WDFINTERRUPT Interrupt)
{
    if (Interrupt == NULL) {
        return;
    }
    if (!Interrupt->DpcQueued) {
        return;
    }
    Interrupt->DpcQueued = FALSE;
    if (Interrupt->Dpc != NULL) {
        Interrupt->Dpc(Interrupt, (WDFOBJECT)Interrupt->Device);
    }
}
