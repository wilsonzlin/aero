/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"

#include "topology.h"
#include "trace.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"
#include "wavert.h"

DRIVER_INITIALIZE DriverEntry;

static DRIVER_ADD_DEVICE VirtIoSndAddDevice;
static DRIVER_DISPATCH VirtIoSndDispatchPnp;
static NTSTATUS VirtIoSndStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PRESOURCELIST ResourceList);

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

    status = PcInitializeAdapterDriver(DriverObject, RegistryPath, VirtIoSndAddDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Wrap PortCls PnP handling so we can stop virtio transport cleanly on
    // STOP/REMOVE. All other PnP IRPs are forwarded to PcDispatchIrp.
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

static NTSTATUS
VirtIoSndDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    PIO_STACK_LOCATION stack = IoGetCurrentIrpStackLocation(Irp);
    PVIRTIOSND_ADAPTER_EXTENSION ext = (PVIRTIOSND_ADAPTER_EXTENSION)DeviceObject->DeviceExtension;
    PVIRTIOSND_DEVICE_EXTENSION dx = (ext != NULL) ? &ext->Dx : NULL;
    NTSTATUS status;

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
        VirtIoSndStopHardware(dx);
        break;

    default:
        break;
    }

    return PcDispatchIrp(DeviceObject, Irp);
}

static NTSTATUS
VirtIoSndCapturePdoAndLower(_In_ PDEVICE_OBJECT DeviceObject, _Out_ PDEVICE_OBJECT *PdoOut, _Out_ PDEVICE_OBJECT *LowerOut)
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

    ObDereferenceObject(base);
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndStartDevice(PDEVICE_OBJECT DeviceObject, PIRP Irp, PRESOURCELIST ResourceList)
{
    NTSTATUS status;
    PVIRTIOSND_ADAPTER_EXTENSION ext = (PVIRTIOSND_ADAPTER_EXTENSION)DeviceObject->DeviceExtension;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    BOOLEAN hwStarted = FALSE;
    PUNKNOWN unknownAdapter = NULL;
    PUNKNOWN unknownWave = NULL;
    PUNKNOWN unknownWavePort = NULL;
    PPORTWAVERT portWaveRt = NULL;
    PUNKNOWN unknownTopo = NULL;
    PUNKNOWN unknownTopoPort = NULL;
    PPORTTOPOLOGY portTopology = NULL;

    VIRTIOSND_TRACE("StartDevice\n");

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
        PIO_STACK_LOCATION stack = IoGetCurrentIrpStackLocation(Irp);
        PCM_RESOURCE_LIST raw = stack->Parameters.StartDevice.AllocatedResources;
        PCM_RESOURCE_LIST translated = stack->Parameters.StartDevice.AllocatedResourcesTranslated;
        status = VirtIoSndStartHardware(dx, raw, translated);
    }
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndStartHardware failed: 0x%08X\n", status);
        VirtIoSndStopHardware(dx); // best-effort cleanup of partial allocations
        goto Exit;
    }
    hwStarted = TRUE;

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

    status = IUnknown_QueryInterface(unknownTopoPort, &IID_IPortTopology, (PVOID *)&portTopology);
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

    status = VirtIoSndMiniportWaveRT_Create(dx, &unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create waveRT miniport failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcNewPort(&unknownWavePort, CLSID_PortWaveRT);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(WaveRT) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownWavePort, &IID_IPortWaveRT, (PVOID *)&portWaveRt);
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
