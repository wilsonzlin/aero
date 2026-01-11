/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"

#include "adapter_context.h"
#include "topology.h"
#include "trace.h"
#include "virtio_pci_contract.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"
#include "wavert.h"

DRIVER_INITIALIZE DriverEntry;

static DRIVER_ADD_DEVICE VirtIoSndAddDevice;
static DRIVER_DISPATCH VirtIoSndDispatchPnp;
static NTSTATUS VirtIoSndStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PRESOURCELIST ResourceList);

/*
 * PortCls allocates the adapter FDO device extension. Store the virtio-snd WDM
 * per-device state at the front so legacy helper macros (VIRTIOSND_GET_DX) still
 * work, while keeping additional PortCls bookkeeping alongside it.
 */
typedef struct _VIRTIOSND_ADAPTER_EXTENSION {
    VIRTIOSND_DEVICE_EXTENSION Dx;
    BOOLEAN TopologyRegistered;
    BOOLEAN WaveRegistered;
} VIRTIOSND_ADAPTER_EXTENSION, *PVIRTIOSND_ADAPTER_EXTENSION;

_Use_decl_annotations_
NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;

    VIRTIOSND_TRACE("DriverEntry\n");

    VirtIoSndAdapterContext_Initialize();

    status = PcInitializeAdapterDriver(DriverObject, RegistryPath, VirtIoSndAddDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Wrap PortCls PnP handling so we can stop/unregister virtio transport cleanly
    // on STOP/REMOVE. All other PnP IRPs are forwarded to PcDispatchIrp.
    DriverObject->MajorFunction[IRP_MJ_PNP] = VirtIoSndDispatchPnp;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndAddDevice(PDRIVER_OBJECT DriverObject, PDEVICE_OBJECT PhysicalDeviceObject)
{
    VIRTIOSND_TRACE("AddDevice\n");

    return PcAddAdapterDevice(
        DriverObject,
        PhysicalDeviceObject,
        VirtIoSndStartDevice,
        2, // max miniports/subdevices
        sizeof(VIRTIOSND_ADAPTER_EXTENSION) // device extension size
    );
}

static VOID VirtIoSndSafeRelease(_In_opt_ PUNKNOWN Unknown)
{
    if (Unknown != NULL) {
        IUnknown_Release(Unknown);
    }
}

static BOOLEAN VirtIoSndReadForceNullBackend(_In_ PDEVICE_OBJECT DeviceObject)
{
    HANDLE key;
    UNICODE_STRING valueName;
    UCHAR buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
    PKEY_VALUE_PARTIAL_INFORMATION info;
    ULONG resultLen;
    BOOLEAN forceNullBackend;

    forceNullBackend = FALSE;
    key = NULL;

    if (DeviceObject == NULL) {
        return FALSE;
    }

    if (!NT_SUCCESS(IoOpenDeviceRegistryKey(DeviceObject, PLUGPLAY_REGKEY_DEVICE, KEY_READ, &key)) || key == NULL) {
        return FALSE;
    }

    RtlInitUnicodeString(&valueName, L"ForceNullBackend");
    info = (PKEY_VALUE_PARTIAL_INFORMATION)buf;
    RtlZeroMemory(buf, sizeof(buf));
    resultLen = 0;

    if (NT_SUCCESS(ZwQueryValueKey(key, &valueName, KeyValuePartialInformation, info, sizeof(buf), &resultLen)) &&
        info->Type == REG_DWORD && info->DataLength >= sizeof(ULONG)) {
        forceNullBackend = (*(UNALIGNED const ULONG*)info->Data) ? TRUE : FALSE;
    }

    ZwClose(key);
    return forceNullBackend;
}

static NTSTATUS
VirtIoSndDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    PIO_STACK_LOCATION stack;
    PVIRTIOSND_ADAPTER_EXTENSION ext;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;
    PUNKNOWN unknownAdapter;

    stack = IoGetCurrentIrpStackLocation(Irp);
    ext = (PVIRTIOSND_ADAPTER_EXTENSION)DeviceObject->DeviceExtension;
    dx = (ext != NULL) ? &ext->Dx : NULL;
    unknownAdapter = NULL;

    if (ext == NULL || dx == NULL || dx->Signature != VIRTIOSND_DX_SIGNATURE || dx->Self != DeviceObject) {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    switch (stack->MinorFunction) {
    case IRP_MN_STOP_DEVICE:
        /*
         * Let PortCls quiesce/close pins first so the WaveRT period timer is
         * stopped before we tear down the virtio transport.
         */
        status = PcDispatchIrp(DeviceObject, Irp);

        if (ext->WaveRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
            ext->WaveRegistered = FALSE;
        }
        if (ext->TopologyRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);
            ext->TopologyRegistered = FALSE;
        }

        if (NT_SUCCESS(PcGetAdapterCommon(DeviceObject, &unknownAdapter))) {
            VirtIoSndAdapterContext_Unregister(unknownAdapter);
            VirtIoSndSafeRelease(unknownAdapter);
        }

        VirtIoSndStopHardware(dx);
        return status;

    case IRP_MN_SURPRISE_REMOVAL:
    case IRP_MN_REMOVE_DEVICE:
        dx->Removed = TRUE;

        if (ext->WaveRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
            ext->WaveRegistered = FALSE;
        }
        if (ext->TopologyRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);
            ext->TopologyRegistered = FALSE;
        }

        if (NT_SUCCESS(PcGetAdapterCommon(DeviceObject, &unknownAdapter))) {
            VirtIoSndAdapterContext_Unregister(unknownAdapter);
            VirtIoSndSafeRelease(unknownAdapter);
        }

        VirtIoSndStopHardware(dx);
        break;

    default:
        break;
    }

    return PcDispatchIrp(DeviceObject, Irp);
}

static NTSTATUS
VirtIoSndCapturePdoAndLower(_In_ PDEVICE_OBJECT DeviceObject, _Out_ PDEVICE_OBJECT* PdoOut, _Out_ PDEVICE_OBJECT* LowerOut)
{
    PDEVICE_OBJECT base;
    PDEVICE_OBJECT prev;
    PDEVICE_OBJECT cur;

    if (PdoOut == NULL || LowerOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *PdoOut = NULL;
    *LowerOut = NULL;

    base = IoGetDeviceAttachmentBaseRef(DeviceObject);
    if (base == NULL) {
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    prev = base;
    cur = base;
    while (cur != NULL && cur != DeviceObject) {
        prev = cur;
        cur = cur->AttachedDevice;
    }

    if (cur != DeviceObject) {
        ObDereferenceObject(base);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    *PdoOut = base;
    *LowerOut = prev;

    /*
     * We only need stable pointers to the PDO/lower object. The objects are
     * owned by the device stack; avoid leaking references in the PortCls path.
     */
    ObDereferenceObject(base);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndStartDevice(PDEVICE_OBJECT DeviceObject, PIRP Irp, PRESOURCELIST ResourceList)
{
    NTSTATUS status;
    PVIRTIOSND_ADAPTER_EXTENSION ext;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    BOOLEAN hwStarted;
    BOOLEAN adapterContextRegistered;
    PUNKNOWN unknownAdapter;
    PUNKNOWN unknownWave;
    PUNKNOWN unknownWavePort;
    PPORTWAVERT portWaveRt;
    PUNKNOWN unknownTopo;
    PUNKNOWN unknownTopoPort;
    PPORTTOPOLOGY portTopology;
    PCM_RESOURCE_LIST raw;
    PCM_RESOURCE_LIST translated;
    PIO_STACK_LOCATION stack;

    VIRTIOSND_TRACE("StartDevice\n");

    ext = (PVIRTIOSND_ADAPTER_EXTENSION)DeviceObject->DeviceExtension;
    hwStarted = FALSE;
    adapterContextRegistered = FALSE;
    unknownAdapter = NULL;
    unknownWave = NULL;
    unknownWavePort = NULL;
    portWaveRt = NULL;
    unknownTopo = NULL;
    unknownTopoPort = NULL;
    portTopology = NULL;
    raw = NULL;
    translated = NULL;
    stack = NULL;

    status = PcGetAdapterCommon(DeviceObject, &unknownAdapter);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcGetAdapterCommon failed: 0x%08X\n", (UINT)status);
        return status;
    }

    status = PcRegisterAdapterPowerManagement(unknownAdapter, DeviceObject);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterAdapterPowerManagement failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    if (ext == NULL) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Exit;
    }

    /*
     * Best-effort restart safety: if the extension was previously initialized,
     * stop any in-flight virtio transport before zeroing the bookkeeping fields.
     */
    dx = &ext->Dx;
    if (dx->Signature == VIRTIOSND_DX_SIGNATURE && dx->Self == DeviceObject) {
        VirtIoSndStopHardware(dx);
    }

    RtlZeroMemory(ext, sizeof(*ext));
    dx = &ext->Dx;

    dx->Signature = VIRTIOSND_DX_SIGNATURE;
    dx->Self = DeviceObject;
    dx->Removed = FALSE;

    status = VirtIoSndCapturePdoAndLower(DeviceObject, &dx->Pdo, &dx->LowerDeviceObject);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("failed to capture PDO/lower device object: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    VirtIoSndIntxInitialize(dx);

    {
        static const USHORT allowedIds[] = {0x1059u};
        status = AeroVirtioPciValidateContractV1Pdo(dx->Pdo, allowedIds, RTL_NUMBER_OF(allowedIds));
        if (!NT_SUCCESS(status)) {
            VIRTIOSND_TRACE_ERROR("AERO-W7-VIRTIO contract identity check failed: 0x%08X\n", (UINT)status);
            goto Exit;
        }
    }

    if (Irp == NULL) {
        status = STATUS_INVALID_PARAMETER;
        goto Exit;
    }

    stack = IoGetCurrentIrpStackLocation(Irp);
    if (stack != NULL) {
        raw = stack->Parameters.StartDevice.AllocatedResources;
        translated = stack->Parameters.StartDevice.AllocatedResourcesTranslated;
    }
    if (raw == NULL || translated == NULL) {
        VIRTIOSND_TRACE_ERROR("StartDevice missing CM resources (raw=%p translated=%p)\n", raw, translated);
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Exit;
    }

    /*
     * Policy: fail StartDevice if the virtio-snd transport cannot be brought up.
     * This surfaces as a Code 10 in Device Manager rather than enumerating a
     * "null backend" audio endpoint.
     */
    status = VirtIoSndStartHardware(dx, raw, translated);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndStartHardware failed: 0x%08X\n", (UINT)status);
        VirtIoSndStopHardware(dx); // best-effort cleanup of partial allocations
        goto Exit;
    }
    hwStarted = TRUE;

    status = VirtIoSndAdapterContext_Register(unknownAdapter, dx, VirtIoSndReadForceNullBackend(DeviceObject));
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndAdapterContext_Register failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    adapterContextRegistered = TRUE;

    status = VirtIoSndMiniportTopology_Create(&unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create topology miniport failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcNewPort(&unknownTopoPort, CLSID_PortTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(Topology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownTopoPort, &IID_IPortTopology, (PVOID*)&portTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortTopology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IPortTopology_Init(portTopology, DeviceObject, Irp, unknownTopo, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortTopology::Init failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY, unknownTopoPort, unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(topology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    ext->TopologyRegistered = TRUE;

    status = VirtIoSndMiniportWaveRT_Create(&unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create waveRT miniport failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcNewPort(&unknownWavePort, CLSID_PortWaveRT);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(WaveRT) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownWavePort, &IID_IPortWaveRT, (PVOID*)&portWaveRt);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortWaveRT) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IPortWaveRT_Init(portWaveRt, DeviceObject, Irp, unknownWave, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortWaveRT::Init failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE, unknownWavePort, unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(wave) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    ext->WaveRegistered = TRUE;

    status = PcRegisterPhysicalConnection(
        DeviceObject,
        VIRTIOSND_SUBDEVICE_TOPOLOGY,
        VIRTIOSND_TOPO_PIN_BRIDGE,
        VIRTIOSND_SUBDEVICE_WAVE,
        VIRTIOSND_WAVE_PIN_BRIDGE);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterPhysicalConnection failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

Exit:
    if (!NT_SUCCESS(status) && adapterContextRegistered) {
        VirtIoSndAdapterContext_Unregister(unknownAdapter);
        adapterContextRegistered = FALSE;
    }

    VirtIoSndSafeRelease((PUNKNOWN)portWaveRt);
    VirtIoSndSafeRelease(unknownWavePort);
    VirtIoSndSafeRelease(unknownWave);

    VirtIoSndSafeRelease((PUNKNOWN)portTopology);
    VirtIoSndSafeRelease(unknownTopoPort);
    VirtIoSndSafeRelease(unknownTopo);

    VirtIoSndSafeRelease(unknownAdapter);

    if (!NT_SUCCESS(status) && hwStarted) {
        if (ext != NULL) {
            if (ext->WaveRegistered) {
                (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
                ext->WaveRegistered = FALSE;
            }
            if (ext->TopologyRegistered) {
                (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);
                ext->TopologyRegistered = FALSE;
            }
        }
        VirtIoSndStopHardware(dx);
    }
    return status;
}
