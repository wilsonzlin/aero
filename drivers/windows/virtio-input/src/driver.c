#include "virtio_input.h"

DRIVER_INITIALIZE DriverEntry;

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    HID_MINIDRIVER_REGISTRATION hidRegistration;
    NTSTATUS status;

    WDF_DRIVER_CONFIG_INIT(&config, VirtioInputEvtDriverDeviceAdd);
    config.DriverPoolTag = VIRTIOINPUT_POOL_TAG;

    status =
        WdfDriverCreate(DriverObject, RegistryPath, WDF_NO_OBJECT_ATTRIBUTES, &config, WDF_NO_HANDLE);
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

