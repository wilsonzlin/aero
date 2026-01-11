/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "adapter_context.h"

#include "trace.h"

typedef struct _VIRTIOSND_ADAPTER_CONTEXT_ENTRY {
    LIST_ENTRY ListEntry;
    PUNKNOWN UnknownAdapter;
    PVIRTIOSND_DEVICE_EXTENSION Dx;
} VIRTIOSND_ADAPTER_CONTEXT_ENTRY, *PVIRTIOSND_ADAPTER_CONTEXT_ENTRY;

static LIST_ENTRY g_VirtIoSndAdapterContextList = {&g_VirtIoSndAdapterContextList, &g_VirtIoSndAdapterContextList};
static KSPIN_LOCK g_VirtIoSndAdapterContextLock = 0;

static _Ret_maybenull_ PVIRTIOSND_ADAPTER_CONTEXT_ENTRY
VirtIoSndAdapterContext_FindLocked(_In_ PUNKNOWN UnknownAdapter)
{
    PLIST_ENTRY link;

    for (link = g_VirtIoSndAdapterContextList.Flink; link != &g_VirtIoSndAdapterContextList; link = link->Flink) {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;

        entry = CONTAINING_RECORD(link, VIRTIOSND_ADAPTER_CONTEXT_ENTRY, ListEntry);
        if (entry->UnknownAdapter == UnknownAdapter) {
            return entry;
        }
    }

    return NULL;
}

_Use_decl_annotations_
VOID
VirtIoSndAdapterContext_Initialize(VOID)
{
    /*
     * NOTE: KSPIN_LOCK is semantically initialized by KeInitializeSpinLock.
     * While the loader zeros BSS (and 0 is the unlocked state today), calling
     * KeInitializeSpinLock keeps the intent explicit and avoids relying on
     * undocumented initialization behavior.
     */
    KeInitializeSpinLock(&g_VirtIoSndAdapterContextLock);
}

_Use_decl_annotations_
NTSTATUS VirtIoSndAdapterContext_Register(PUNKNOWN UnknownAdapter, PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY newEntry;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL || Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = STATUS_SUCCESS;
    newEntry = NULL;

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY existing;

        existing = VirtIoSndAdapterContext_FindLocked(UnknownAdapter);
        if (existing != NULL) {
            existing->Dx = Dx;
            KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);
            return STATUS_SUCCESS;
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    newEntry = (PVIRTIOSND_ADAPTER_CONTEXT_ENTRY)ExAllocatePoolWithTag(NonPagedPool, sizeof(*newEntry), VIRTIOSND_POOL_TAG);
    if (newEntry == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(newEntry, sizeof(*newEntry));
    newEntry->UnknownAdapter = UnknownAdapter;
    newEntry->Dx = Dx;

    /*
     * Hold a reference so the mapping can survive after VirtIoSndStartDevice drops
     * its local PcGetAdapterCommon reference.
     */
    IUnknown_AddRef(UnknownAdapter);

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY existing;

        existing = VirtIoSndAdapterContext_FindLocked(UnknownAdapter);
        if (existing != NULL) {
            existing->Dx = Dx;
            status = STATUS_SUCCESS;
        } else {
            InsertTailList(&g_VirtIoSndAdapterContextList, &newEntry->ListEntry);
            newEntry = NULL; /* consumed */
            status = STATUS_SUCCESS;
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (newEntry != NULL) {
        IUnknown_Release(newEntry->UnknownAdapter);
        ExFreePoolWithTag(newEntry, VIRTIOSND_POOL_TAG);
    }

    return status;
}

_Use_decl_annotations_
VOID VirtIoSndAdapterContext_Unregister(PUNKNOWN UnknownAdapter)
{
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL) {
        return;
    }

    entry = NULL;

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        entry = VirtIoSndAdapterContext_FindLocked(UnknownAdapter);
        if (entry != NULL) {
            RemoveEntryList(&entry->ListEntry);
            InitializeListHead(&entry->ListEntry);
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (entry != NULL) {
        IUnknown_Release(entry->UnknownAdapter);
        ExFreePoolWithTag(entry, VIRTIOSND_POOL_TAG);
    }
}

_Use_decl_annotations_
PVIRTIOSND_DEVICE_EXTENSION VirtIoSndAdapterContext_Lookup(PUNKNOWN UnknownAdapter)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL) {
        return NULL;
    }

    dx = NULL;

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;

        entry = VirtIoSndAdapterContext_FindLocked(UnknownAdapter);
        if (entry != NULL) {
            dx = entry->Dx;
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    return dx;
}

_Use_decl_annotations_
VOID VirtIoSndAdapterContext_UnregisterAndStop(PUNKNOWN UnknownAdapter, BOOLEAN MarkRemoved)
{
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL) {
        return;
    }

    entry = NULL;

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        entry = VirtIoSndAdapterContext_FindLocked(UnknownAdapter);
        if (entry != NULL) {
            RemoveEntryList(&entry->ListEntry);
            InitializeListHead(&entry->ListEntry);
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (entry == NULL) {
        return;
    }

    if (entry->Dx != NULL) {
        if (MarkRemoved) {
            entry->Dx->Removed = TRUE;
        }

        VirtIoSndStopHardware(entry->Dx);
    }

    IUnknown_Release(entry->UnknownAdapter);
    ExFreePoolWithTag(entry, VIRTIOSND_POOL_TAG);
}
