#include <ntddk.h>
#include <wdf.h>

#include "virtio_pci_modern.h"

#define VIRTIO_TEST_POOL_TAG 'TioV'

typedef struct _DEVICE_CONTEXT {
    VIRTIO_PCI_MODERN_DEVICE Vdev;
    BOOLEAN VdevInitialized;
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, VirtioTestGetContext);

DRIVER_INITIALIZE DriverEntry;
EVT_WDF_DRIVER_DEVICE_ADD VirtioTestEvtDeviceAdd;
EVT_WDF_DEVICE_PREPARE_HARDWARE VirtioTestEvtDevicePrepareHardware;
EVT_WDF_DEVICE_RELEASE_HARDWARE VirtioTestEvtDeviceReleaseHardware;
EVT_WDF_DEVICE_D0_EXIT VirtioTestEvtDeviceD0Exit;

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    NTSTATUS status;

    WDF_DRIVER_CONFIG_INIT(&config, VirtioTestEvtDeviceAdd);
    config.DriverPoolTag = VIRTIO_TEST_POOL_TAG;

    status = WdfDriverCreate(
        DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);

    if (!NT_SUCCESS(status)) {
        DbgPrint("virtio-transport-test: WdfDriverCreate failed 0x%08X\n", status);
    }

    return status;
}

NTSTATUS VirtioTestEvtDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnpCallbacks;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDFDEVICE device;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Driver);

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpCallbacks);
    pnpCallbacks.EvtDevicePrepareHardware = VirtioTestEvtDevicePrepareHardware;
    pnpCallbacks.EvtDeviceReleaseHardware = VirtioTestEvtDeviceReleaseHardware;
    pnpCallbacks.EvtDeviceD0Exit = VirtioTestEvtDeviceD0Exit;

    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpCallbacks);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);
    attributes.EvtCleanupCallback = NULL;

    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        DbgPrint("virtio-transport-test: WdfDeviceCreate failed 0x%08X\n", status);
        return status;
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioTestEvtDevicePrepareHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    PDEVICE_CONTEXT ctx;
    NTSTATUS status;
    UINT64 negotiated;
    ULONG i;

    ctx = VirtioTestGetContext(Device);

    DbgPrint("virtio-transport-test: EvtDevicePrepareHardware\n");

    status = VirtioPciModernInit(Device, &ctx->Vdev);
    if (!NT_SUCCESS(status)) {
        DbgPrint("virtio-transport-test: VirtioPciModernInit failed 0x%08X\n", status);
        return status;
    }

    status = VirtioPciModernMapBars(&ctx->Vdev, ResourcesRaw, ResourcesTranslated);
    if (!NT_SUCCESS(status)) {
        DbgPrint("virtio-transport-test: VirtioPciModernMapBars failed 0x%08X\n", status);
        VirtioPciModernUninit(&ctx->Vdev);
        return status;
    }

    ctx->VdevInitialized = TRUE;

    DbgPrint("virtio-transport-test: virtio caps (all=%lu)\n", ctx->Vdev.Caps.AllCount);
    for (i = 0; i < ctx->Vdev.Caps.AllCount; i++) {
        const VIRTIO_PCI_CAP_INFO *c = &ctx->Vdev.Caps.All[i];
        DbgPrint("virtio-transport-test:  cap[%lu] present=%u cfg_type=%u bar=%u off=0x%lX len=0x%lX cap_off=0x%lX cap_len=%u\n",
                 i,
                 (UINT)c->Present,
                 (UINT)c->CfgType,
                 (UINT)c->Bar,
                 c->Offset,
                 c->Length,
                 c->CapOffset,
                 (UINT)c->CapLen);
    }

    DbgPrint("virtio-transport-test: selected caps:\n");
    DbgPrint("virtio-transport-test:  COMMON bar=%u off=0x%lX len=0x%lX va=%p\n",
             (UINT)ctx->Vdev.Caps.CommonCfg.Bar,
             ctx->Vdev.Caps.CommonCfg.Offset,
             ctx->Vdev.Caps.CommonCfg.Length,
             ctx->Vdev.CommonCfg);
    DbgPrint("virtio-transport-test:  NOTIFY bar=%u off=0x%lX len=0x%lX va=%p mult=0x%lX\n",
             (UINT)ctx->Vdev.Caps.NotifyCfg.Bar,
             ctx->Vdev.Caps.NotifyCfg.Offset,
             ctx->Vdev.Caps.NotifyCfg.Length,
             ctx->Vdev.NotifyBase,
             ctx->Vdev.NotifyOffMultiplier);
    DbgPrint("virtio-transport-test:  ISR    bar=%u off=0x%lX len=0x%lX va=%p\n",
             (UINT)ctx->Vdev.Caps.IsrCfg.Bar,
             ctx->Vdev.Caps.IsrCfg.Offset,
             ctx->Vdev.Caps.IsrCfg.Length,
             ctx->Vdev.IsrStatus);
    DbgPrint("virtio-transport-test:  DEVICE bar=%u off=0x%lX len=0x%lX va=%p\n",
             (UINT)ctx->Vdev.Caps.DeviceCfg.Bar,
             ctx->Vdev.Caps.DeviceCfg.Offset,
             ctx->Vdev.Caps.DeviceCfg.Length,
             ctx->Vdev.DeviceCfg);

    for (i = 0; i < VIRTIO_PCI_MAX_BARS; i++) {
        const VIRTIO_PCI_BAR *b = &ctx->Vdev.Bars[i];
        DbgPrint(
            "virtio-transport-test: BAR%lu present=%u mem=%u 64=%u upper=%u base=0x%I64X raw=0x%I64X trans=0x%I64X len=0x%Ix va=%p\n",
            i,
            (UINT)b->Present,
            (UINT)b->IsMemory,
            (UINT)b->Is64Bit,
            (UINT)b->IsUpperHalf,
            b->Base,
            (ULONGLONG)b->RawStart.QuadPart,
            (ULONGLONG)b->TranslatedStart.QuadPart,
            b->Length,
            b->Va);
    }

    negotiated = 0;
    status = VirtioPciNegotiateFeatures(&ctx->Vdev, VIRTIO_F_VERSION_1, VIRTIO_F_VERSION_1, &negotiated);
    if (!NT_SUCCESS(status)) {
        DbgPrint("virtio-transport-test: VirtioPciNegotiateFeatures failed 0x%08X\n", status);
        VirtioPciModernResetDevice(&ctx->Vdev);
        VirtioPciModernUninit(&ctx->Vdev);
        ctx->VdevInitialized = FALSE;
        return status;
    }

    DbgPrint("virtio-transport-test: negotiated features 0x%I64X\n", negotiated);

    return STATUS_SUCCESS;
}

NTSTATUS VirtioTestEvtDeviceD0Exit(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    PDEVICE_CONTEXT ctx;

    UNREFERENCED_PARAMETER(TargetState);

    ctx = VirtioTestGetContext(Device);
    if (ctx->VdevInitialized) {
        DbgPrint("virtio-transport-test: EvtDeviceD0Exit -> reset\n");
        VirtioPciModernResetDevice(&ctx->Vdev);
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioTestEvtDeviceReleaseHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    PDEVICE_CONTEXT ctx;

    UNREFERENCED_PARAMETER(ResourcesTranslated);

    ctx = VirtioTestGetContext(Device);

    DbgPrint("virtio-transport-test: EvtDeviceReleaseHardware\n");

    if (ctx->VdevInitialized) {
        VirtioPciModernResetDevice(&ctx->Vdev);
        VirtioPciModernUninit(&ctx->Vdev);
        ctx->VdevInitialized = FALSE;
    }

    return STATUS_SUCCESS;
}
