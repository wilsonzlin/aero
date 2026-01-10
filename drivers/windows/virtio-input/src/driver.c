#include "virtio_input.h"
#include "log.h"

DRIVER_INITIALIZE DriverEntry;

static EVT_WDF_OBJECT_CONTEXT_CLEANUP VirtioInputEvtDriverContextCleanup;

static VOID VirtioInputEvtDriverContextCleanup(_In_ WDFOBJECT DriverObject)
{
    UNREFERENCED_PARAMETER(DriverObject);
    VioInputLogShutdown();
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_OBJECT_ATTRIBUTES attributes;
    HID_MINIDRIVER_REGISTRATION hidRegistration;
    NTSTATUS status;

    VioInputLogInitialize(RegistryPath);

    WDF_DRIVER_CONFIG_INIT(&config, VirtioInputEvtDriverDeviceAdd);
    config.DriverPoolTag = VIRTIOINPUT_POOL_TAG;

    WDF_OBJECT_ATTRIBUTES_INIT(&attributes);
    attributes.EvtCleanupCallback = VirtioInputEvtDriverContextCleanup;

    status =
        WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    RtlZeroMemory(&hidRegistration, sizeof(hidRegistration));
    hidRegistration.Revision = HID_REVISION;
    hidRegistration.DriverObject = DriverObject;
    hidRegistration.RegistryPath = RegistryPath;
    hidRegistration.DeviceExtensionSize = 0;
    hidRegistration.DevicesArePolled = FALSE;

    return HidRegisterMinidriver(DriverObject, RegistryPath, &hidRegistration);
}
