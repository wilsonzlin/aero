#include "virtio_input.h"
#include "log.h"

DRIVER_INITIALIZE DriverEntry;

static EVT_WDF_OBJECT_CONTEXT_CLEANUP VirtioInputEvtDriverContextCleanup;

// Default is strict (Aero contract v1). Compat can be enabled via build macro or registry.
BOOLEAN g_VioInputCompatIdName = (AERO_VIOINPUT_COMPAT_ID_NAME != 0) ? TRUE : FALSE;

static VOID VirtioInputEvtDriverContextCleanup(_In_ WDFOBJECT DriverObject)
{
    UNREFERENCED_PARAMETER(DriverObject);
    VioInputLogShutdown();
}

static VOID VirtioInputReadDriverParameters(_In_ PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;
    ULONG compat = g_VioInputCompatIdName ? 1u : 0u;
    RTL_QUERY_REGISTRY_TABLE table[2];

    if (RegistryPath == NULL || RegistryPath->Buffer == NULL) {
        return;
    }

    RtlZeroMemory(table, sizeof(table));

    table[0].Flags = RTL_QUERY_REGISTRY_DIRECT;
    table[0].Name = VIOINPUT_REG_COMPAT_ID_NAME;
    table[0].EntryContext = &compat;
    table[0].DefaultType = REG_DWORD;
    table[0].DefaultData = &compat;
    table[0].DefaultLength = sizeof(compat);

    status = RtlQueryRegistryValues(RTL_REGISTRY_ABSOLUTE, RegistryPath->Buffer, table, NULL, NULL);
    if (NT_SUCCESS(status)) {
        g_VioInputCompatIdName = (compat != 0) ? TRUE : FALSE;
    }

    // Emit a one-time message even in non-diagnostics builds.
    if (g_VioInputCompatIdName) {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_INFO_LEVEL,
            "[vioinput] CompatIdName=1 (query status=%!STATUS!)\n",
            status);
    }
}

NTSTATUS DriverEntry(_In_ PDRIVER_OBJECT DriverObject, _In_ PUNICODE_STRING RegistryPath)
{
    WDF_DRIVER_CONFIG config;
    WDF_OBJECT_ATTRIBUTES attributes;
    HID_MINIDRIVER_REGISTRATION hidRegistration;
    NTSTATUS status;

    VioInputLogInitialize(RegistryPath);
    VirtioInputReadDriverParameters(RegistryPath);

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
