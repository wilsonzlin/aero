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
        sizeof(VIRTIOSND_DEVICE_EXTENSION)
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
    PIO_STACK_LOCATION stack;
    PVIRTIOSND_DEVICE_EXTENSION dx;

    stack = IoGetCurrentIrpStackLocation(Irp);
    dx = VIRTIOSND_GET_DX(DeviceObject);

    if (dx != NULL && dx->Signature == VIRTIOSND_DX_SIGNATURE && dx->Self == DeviceObject) {
        switch (stack->MinorFunction) {
        case IRP_MN_STOP_DEVICE:
            VirtIoSndStopHardware(dx);
            if (dx->LowerDeviceObject != NULL) {
                ObDereferenceObject(dx->LowerDeviceObject);
                dx->LowerDeviceObject = NULL;
            }
            if (dx->Pdo != NULL) {
                ObDereferenceObject(dx->Pdo);
                dx->Pdo = NULL;
            }
            break;

        case IRP_MN_SURPRISE_REMOVAL:
        case IRP_MN_REMOVE_DEVICE:
            dx->Removed = TRUE;
            VirtIoSndStopHardware(dx);
            if (dx->LowerDeviceObject != NULL) {
                ObDereferenceObject(dx->LowerDeviceObject);
                dx->LowerDeviceObject = NULL;
            }
            if (dx->Pdo != NULL) {
                ObDereferenceObject(dx->Pdo);
                dx->Pdo = NULL;
            }
            break;

        default:
            break;
        }
    }

    return PcDispatchIrp(DeviceObject, Irp);
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndStartDevice(PDEVICE_OBJECT DeviceObject, PIRP Irp, PRESOURCELIST ResourceList)
{
    NTSTATUS status;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    PIO_STACK_LOCATION stack;
    PCM_RESOURCE_LIST rawResources;
    PCM_RESOURCE_LIST translatedResources;
    PUNKNOWN unknownAdapter = NULL;
    PUNKNOWN unknownWave = NULL;
    PUNKNOWN unknownWavePort = NULL;
    PPORTWAVERT portWaveRt = NULL;
    PUNKNOWN unknownTopo = NULL;
    PUNKNOWN unknownTopoPort = NULL;
    PPORTTOPOLOGY portTopology = NULL;

    VIRTIOSND_TRACE("StartDevice\n");

    dx = VIRTIOSND_GET_DX(DeviceObject);
    if (dx != NULL) {
        // IoCreateDevice() zero-initializes the extension; only initialize once.
        if (dx->Signature != VIRTIOSND_DX_SIGNATURE) {
            dx->Signature = VIRTIOSND_DX_SIGNATURE;
            dx->Self = DeviceObject;
            VirtIoSndIntxInitialize(dx);
        }

        // Refresh device stack pointers for this start.
        if (dx->LowerDeviceObject != NULL) {
            ObDereferenceObject(dx->LowerDeviceObject);
            dx->LowerDeviceObject = NULL;
        }
        if (dx->Pdo != NULL) {
            ObDereferenceObject(dx->Pdo);
            dx->Pdo = NULL;
        }

        dx->LowerDeviceObject = IoGetLowerDeviceObject(DeviceObject);
        dx->Pdo = IoGetDeviceAttachmentBaseRef(DeviceObject);

        stack = IoGetCurrentIrpStackLocation(Irp);
        rawResources = stack->Parameters.StartDevice.AllocatedResources;
        translatedResources = stack->Parameters.StartDevice.AllocatedResourcesTranslated;

        status = VirtIoSndStartHardware(dx, rawResources, translatedResources);
        if (!NT_SUCCESS(status)) {
            // For bring-up/debugging, allow PortCls to enumerate an endpoint even
            // if the virtio transport cannot be started yet. In that case, the
            // WaveRT miniport will fall back to the Null backend.
            VIRTIOSND_TRACE_ERROR("VirtIoSndStartHardware failed: 0x%08X (falling back to null backend)\n", (UINT)status);
            status = STATUS_SUCCESS;
        }
    }

    status = PcGetAdapterCommon(DeviceObject, &unknownAdapter);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcGetAdapterCommon failed: 0x%08X\n", status);
        return status;
    }

    status = PcRegisterAdapterPowerManagement(unknownAdapter, DeviceObject);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterAdapterPowerManagement failed: 0x%08X\n", status);
        goto Exit;
    }

    status = VirtIoSndMiniportTopology_Create(&unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create topology miniport failed: 0x%08X\n", status);
        goto Exit;
    }

    status = PcNewPort(&unknownTopoPort, CLSID_PortTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(Topology) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownTopoPort, &IID_IPortTopology, (PVOID *)&portTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortTopology) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = IPortTopology_Init(portTopology, DeviceObject, Irp, unknownTopo, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortTopology::Init failed: 0x%08X\n", status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY, unknownTopoPort, unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(topology) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = VirtIoSndMiniportWaveRT_Create(dx, &unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create waveRT miniport failed: 0x%08X\n", status);
        goto Exit;
    }

    status = PcNewPort(&unknownWavePort, CLSID_PortWaveRT);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(WaveRT) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownWavePort, &IID_IPortWaveRT, (PVOID *)&portWaveRt);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortWaveRT) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = IPortWaveRT_Init(portWaveRt, DeviceObject, Irp, unknownWave, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortWaveRT::Init failed: 0x%08X\n", status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE, unknownWavePort, unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(wave) failed: 0x%08X\n", status);
        goto Exit;
    }

    status = PcRegisterPhysicalConnection(
        DeviceObject,
        VIRTIOSND_SUBDEVICE_TOPOLOGY,
        VIRTIOSND_TOPO_PIN_BRIDGE,
        VIRTIOSND_SUBDEVICE_WAVE,
        VIRTIOSND_WAVE_PIN_BRIDGE);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterPhysicalConnection failed: 0x%08X\n", status);
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
    return status;
}
