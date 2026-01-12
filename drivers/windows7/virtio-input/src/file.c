#include "virtio_input.h"

static BOOLEAN VirtioInputAsciiEqualsInsensitive(_In_reads_bytes_(Len) const CHAR *A, _In_ size_t Len, _In_z_ const CHAR *B)
{
    size_t i;
    for (i = 0; i < Len; i++) {
        CHAR ca = A[i];
        CHAR cb = B[i];

        if (cb == '\0') {
            return FALSE;
        }

        if (ca >= 'A' && ca <= 'Z') {
            ca = (CHAR)(ca - 'A' + 'a');
        }

        if (cb >= 'A' && cb <= 'Z') {
            cb = (CHAR)(cb - 'A' + 'a');
        }

        if (ca != cb) {
            return FALSE;
        }
    }

    return B[Len] == '\0';
}

static ULONG VirtioInputGetCollectionNumberFromCreateRequest(_In_ WDFREQUEST Request)
{
    PIRP irp = WdfRequestWdmGetIrp(Request);
    if (irp == NULL) {
        return 0;
    }
    PIO_STACK_LOCATION irpSp = IoGetCurrentIrpStackLocation(irp);

    if (irpSp == NULL) {
        return 0;
    }

    PUCHAR eaBuffer = (PUCHAR)irpSp->Parameters.Create.EaBuffer;
    ULONG eaLength = irpSp->Parameters.Create.EaLength;

    if (eaBuffer == NULL || eaLength < FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName)) {
        return 0;
    }

    // We only expect a small EA list (HidCollection), so bound how much we inspect.
    SIZE_T parseLen = eaLength;
    if (parseLen > 4096u) {
        parseLen = 4096u;
    }

    PUCHAR cursor;
    PUCHAR end;

    PMDL eaMdl = NULL;
    PUCHAR eaSystem = eaBuffer;
    ULONG collection = 0;

    if (WdfRequestGetRequestorMode(Request) == UserMode) {
        NTSTATUS status = VioInputMapUserAddress(eaBuffer, parseLen, IoReadAccess, &eaMdl, (PVOID *)&eaSystem);
        if (!NT_SUCCESS(status)) {
            return 0;
        }
    }

    cursor = eaSystem;
    end = eaSystem + parseLen;

    while (cursor + FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) <= end) {
        PFILE_FULL_EA_INFORMATION entry = (PFILE_FULL_EA_INFORMATION)cursor;

        SIZE_T remaining = (SIZE_T)(end - cursor);
        ULONG entrySize = entry->NextEntryOffset;
        if (entrySize == 0) {
            entrySize = (ULONG)remaining;
        }

        if (entrySize < FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) || entrySize > remaining) {
            break;
        }

        SIZE_T required =
            FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) + (SIZE_T)entry->EaNameLength + 1 + (SIZE_T)entry->EaValueLength;
        if (required > entrySize) {
            break;
        }

        const CHAR *eaName = (const CHAR *)entry->EaName;
        const UCHAR *eaValue = (const UCHAR *)(entry->EaName + entry->EaNameLength + 1);

        if (VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HidCollection") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HID_COLLECTION") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HidCollectionNumber") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HID_COLLECTION_NUMBER")) {

            if (entry->EaValueLength >= sizeof(ULONG)) {
                collection = *(UNALIGNED const ULONG *)eaValue;
                break;
            }
            if (entry->EaValueLength >= sizeof(USHORT)) {
                collection = *(UNALIGNED const USHORT *)eaValue;
                break;
            }
            if (entry->EaValueLength >= sizeof(UCHAR)) {
                collection = *(UNALIGNED const UCHAR *)eaValue;
                break;
            }

            collection = 0;
            break;
        }

        if (entry->NextEntryOffset == 0) {
            break;
        }

        cursor += entry->NextEntryOffset;
    }

    VioInputMdlFree(&eaMdl);

    return collection;
}

static VOID VirtioInputEvtDeviceFileCreate(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request, _In_ WDFFILEOBJECT FileObject)
{
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(Device);
    PVIRTIO_INPUT_FILE_CONTEXT fileCtx = VirtioInputGetFileContext(FileObject);

    PIRP irp = WdfRequestWdmGetIrp(Request);
    PIO_STACK_LOCATION irpSp = (irp == NULL) ? NULL : IoGetCurrentIrpStackLocation(irp);
    fileCtx->HasCollectionEa = (irpSp != NULL && irpSp->Parameters.Create.EaBuffer != NULL && irpSp->Parameters.Create.EaLength != 0);

    fileCtx->CollectionNumber = VirtioInputGetCollectionNumberFromCreateRequest(Request);

    switch (fileCtx->CollectionNumber) {
    case 1:
        if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_MOUSE;
        } else if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_KEYBOARD;
        } else {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_ANY;
        }
        break;
    default:
        fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_ANY;
        break;
    }

    WdfRequestComplete(Request, STATUS_SUCCESS);
}

static VOID VirtioInputEvtFileClose(_In_ WDFFILEOBJECT FileObject)
{
    UNREFERENCED_PARAMETER(FileObject);
}

static VOID VirtioInputEvtFileCleanup(_In_ WDFFILEOBJECT FileObject)
{
    WDFDEVICE device = WdfFileObjectGetDevice(FileObject);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    UCHAR i;
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        if (devCtx->ReadReportQueue[i] == NULL) {
            continue;
        }

        for (;;) {
            WDFREQUEST found = NULL;
            NTSTATUS status = WdfIoQueueFindRequest(devCtx->ReadReportQueue[i], NULL, FileObject, NULL, &found);
            if (!NT_SUCCESS(status)) {
                break;
            }

            WDFREQUEST request = NULL;
            status = WdfIoQueueRetrieveFoundRequest(devCtx->ReadReportQueue[i], found, &request);
            WdfObjectDereference(found);

            if (!NT_SUCCESS(status)) {
                break;
            }

            VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
            VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT cancelled (file cleanup): pending=%ld\n",
                devCtx->Counters.ReadReportQueueDepth);

            WdfRequestComplete(request, STATUS_CANCELLED);
        }
    }
}

NTSTATUS VirtioInputFileConfigure(_Inout_ WDFDEVICE_INIT *DeviceInit)
{
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_OBJECT_ATTRIBUTES fileAttributes;

    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, VirtioInputEvtDeviceFileCreate, VirtioInputEvtFileClose, VirtioInputEvtFileCleanup);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&fileAttributes, VIRTIO_INPUT_FILE_CONTEXT);

    WdfDeviceInitSetFileObjectConfig(DeviceInit, &fileConfig, &fileAttributes);

    return STATUS_SUCCESS;
}
