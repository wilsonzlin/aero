#include <ntddk.h>

#include "portcls_compat.h"
#include "topology.h"
#include "trace.h"
#include "virtiosnd.h"

typedef struct _VIRTIOSND_TOPOLOGY_MINIPORT {
    IMiniportTopology Interface;
    LONG RefCount;
} VIRTIOSND_TOPOLOGY_MINIPORT, *PVIRTIOSND_TOPOLOGY_MINIPORT;

static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_AddRef(_In_ IMiniportTopology *This);
static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_Release(_In_ IMiniportTopology *This);

static PVIRTIOSND_TOPOLOGY_MINIPORT
VirtIoSndTopologyMiniportFromInterface(_In_ IMiniportTopology *Interface)
{
    return CONTAINING_RECORD(Interface, VIRTIOSND_TOPOLOGY_MINIPORT, Interface);
}

static NTSTATUS STDMETHODCALLTYPE
VirtIoSndTopologyMiniport_QueryInterface(
    _In_ IMiniportTopology *This,
    _In_ REFIID Riid,
    _Outptr_ PVOID *Object
    )
{
    if (Object == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Object = NULL;

    if (IsEqualGUID(Riid, &IID_IUnknown) ||
        IsEqualGUID(Riid, &IID_IMiniport) ||
        IsEqualGUID(Riid, &IID_IMiniportTopology)) {
        *Object = This;
        (VOID)VirtIoSndTopologyMiniport_AddRef(This);
        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_AddRef(_In_ IMiniportTopology *This)
{
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport = VirtIoSndTopologyMiniportFromInterface(This);
    return (ULONG)InterlockedIncrement(&miniport->RefCount);
}

static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_Release(_In_ IMiniportTopology *This)
{
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport = VirtIoSndTopologyMiniportFromInterface(This);
    LONG ref = InterlockedDecrement(&miniport->RefCount);
    if (ref == 0) {
        ExFreePoolWithTag(miniport, VIRTIOSND_POOL_TAG);
        return 0;
    }
    return (ULONG)ref;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndTopologyMiniport_Init(
    _In_ IMiniportTopology *This,
    _In_opt_ PUNKNOWN UnknownAdapter,
    _In_ PRESOURCELIST ResourceList,
    _In_ PPORTTOPOLOGY Port,
    _Outptr_opt_result_maybenull_ PSERVICEGROUP *ServiceGroup
    )
{
    UNREFERENCED_PARAMETER(This);
    UNREFERENCED_PARAMETER(UnknownAdapter);
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);

    if (ServiceGroup != NULL) {
        *ServiceGroup = NULL;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE VirtIoSndTopologyMiniport_GetDescription(
    _In_ IMiniportTopology *This,
    _Outptr_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor
    );

static NTSTATUS STDMETHODCALLTYPE VirtIoSndTopologyMiniport_DataRangeIntersection(
    _In_ IMiniportTopology *This,
    _In_ ULONG PinId,
    _In_ PKSDATARANGE DataRange,
    _In_ PKSDATARANGE MatchingDataRange,
    _In_ ULONG OutputBufferLength,
    _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength) PVOID ResultantFormat,
    _Out_ PULONG ResultantFormatLength
    );

static const GUID* g_VirtIoSndTopoCategories[] = {
    &KSCATEGORY_AUDIO,
    &KSCATEGORY_TOPOLOGY,
};

static NTSTATUS
VirtIoSndProperty_ChannelConfig(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_GET) {
        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            PropertyRequest->ValueSize = sizeof(ULONG);
            return STATUS_BUFFER_TOO_SMALL;
        }

        *(PULONG)PropertyRequest->Value = KSAUDIO_SPEAKER_STEREO;
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG mask;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        mask = *(const ULONG *)PropertyRequest->Value;
        if (mask != KSAUDIO_SPEAKER_STEREO) {
            return STATUS_INVALID_PARAMETER;
        }

        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static NTSTATUS
VirtIoSndProperty_JackDescription(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG required;
    KSMULTIPLE_ITEM *item;
    KSJACK_DESCRIPTION *jack;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!(PropertyRequest->Verb & KSPROPERTY_TYPE_GET)) {
        return STATUS_INVALID_PARAMETER;
    }

    required = sizeof(KSMULTIPLE_ITEM) + sizeof(KSJACK_DESCRIPTION);
    if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
        PropertyRequest->ValueSize = required;
        return STATUS_BUFFER_TOO_SMALL;
    }

    item = (KSMULTIPLE_ITEM *)PropertyRequest->Value;
    item->Size = required;
    item->Count = 1;

    jack = (KSJACK_DESCRIPTION *)(item + 1);
    RtlZeroMemory(jack, sizeof(*jack));
    jack->ChannelMapping = KSAUDIO_SPEAKER_STEREO;
    jack->IsConnected = TRUE;

    PropertyRequest->ValueSize = required;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndProperty_JackDescription2(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG required;
    KSMULTIPLE_ITEM *item;
    KSJACK_DESCRIPTION2 *jack;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!(PropertyRequest->Verb & KSPROPERTY_TYPE_GET)) {
        return STATUS_INVALID_PARAMETER;
    }

    required = sizeof(KSMULTIPLE_ITEM) + sizeof(KSJACK_DESCRIPTION2);
    if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
        PropertyRequest->ValueSize = required;
        return STATUS_BUFFER_TOO_SMALL;
    }

    item = (KSMULTIPLE_ITEM *)PropertyRequest->Value;
    item->Size = required;
    item->Count = 1;

    jack = (KSJACK_DESCRIPTION2 *)(item + 1);
    RtlZeroMemory(jack, sizeof(*jack));

    PropertyRequest->ValueSize = required;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndProperty_JackContainerId(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    static const GUID kContainerId = {
        0x7d8c3f44, 0x0f6e, 0x4d3f, {0x9f, 0x2c, 0x35, 0x6d, 0x5c, 0x63, 0x33, 0x41}
    };

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (!(PropertyRequest->Verb & KSPROPERTY_TYPE_GET)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(GUID)) {
        PropertyRequest->ValueSize = sizeof(GUID);
        return STATUS_BUFFER_TOO_SMALL;
    }

    *(GUID *)PropertyRequest->Value = kContainerId;
    PropertyRequest->ValueSize = sizeof(GUID);
    return STATUS_SUCCESS;
}

static const PCPROPERTY_ITEM g_VirtIoSndTopoAudioProperties[] = {
    {KSPROPERTY_AUDIO_CHANNEL_CONFIG, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET, VirtIoSndProperty_ChannelConfig},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoJackProperties[] = {
    {KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET, VirtIoSndProperty_JackDescription},
    {KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET, VirtIoSndProperty_JackDescription2},
    {KSPROPERTY_JACK_CONTAINERID, KSPROPERTY_TYPE_GET, VirtIoSndProperty_JackContainerId},
};

static const PCPROPERTY_SET g_VirtIoSndTopoPropertySets[] = {
    {&KSPROPSETID_Audio, RTL_NUMBER_OF(g_VirtIoSndTopoAudioProperties), g_VirtIoSndTopoAudioProperties},
    {&KSPROPSETID_Jack, RTL_NUMBER_OF(g_VirtIoSndTopoJackProperties), g_VirtIoSndTopoJackProperties},
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoAutomation = {
    RTL_NUMBER_OF(g_VirtIoSndTopoPropertySets),
    g_VirtIoSndTopoPropertySets,
    0,
    NULL,
    0,
    NULL,
};

static const KSPIN_DESCRIPTOR g_VirtIoSndTopoPinDescriptors[] = {
    // VIRTIOSND_TOPO_PIN_BRIDGE
    {0, NULL, 0, NULL, 0, NULL, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_BRIDGE, &KSNODETYPE_WAVE_OUT, &KSPINNAME_WAVE_OUT},
    // VIRTIOSND_TOPO_PIN_SPEAKER
    {0, NULL, 0, NULL, 0, NULL, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_NONE, &KSNODETYPE_SPEAKER, &KSPINNAME_SPEAKER},
};

static const PCPIN_DESCRIPTOR g_VirtIoSndTopoPins[] = {
    {1, 1, 0, NULL, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_BRIDGE]},
    {1, 1, 0, &g_VirtIoSndTopoAutomation, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_SPEAKER]},
};

static const PCNODE_DESCRIPTOR g_VirtIoSndTopoNodes[] = {
    // Node 0: speaker endpoint.
    {0, &g_VirtIoSndTopoAutomation, &KSNODETYPE_SPEAKER, NULL},
};

static const PCCONNECTION_DESCRIPTOR g_VirtIoSndTopoConnections[] = {
    {KSFILTER_NODE, VIRTIOSND_TOPO_PIN_BRIDGE, 0, 0},
    {0, 0, KSFILTER_NODE, VIRTIOSND_TOPO_PIN_SPEAKER},
};

static const PCFILTER_DESCRIPTOR g_VirtIoSndTopoFilterDescriptor = {
    1,
    &g_VirtIoSndTopoAutomation,
    sizeof(PCPIN_DESCRIPTOR),
    RTL_NUMBER_OF(g_VirtIoSndTopoPins),
    g_VirtIoSndTopoPins,
    sizeof(PCNODE_DESCRIPTOR),
    RTL_NUMBER_OF(g_VirtIoSndTopoNodes),
    g_VirtIoSndTopoNodes,
    sizeof(PCCONNECTION_DESCRIPTOR),
    RTL_NUMBER_OF(g_VirtIoSndTopoConnections),
    g_VirtIoSndTopoConnections,
    RTL_NUMBER_OF(g_VirtIoSndTopoCategories),
    g_VirtIoSndTopoCategories,
};

static NTSTATUS STDMETHODCALLTYPE VirtIoSndTopologyMiniport_GetDescription(
    _In_ IMiniportTopology *This,
    _Outptr_ PPCFILTER_DESCRIPTOR *OutFilterDescriptor
    )
{
    UNREFERENCED_PARAMETER(This);

    if (OutFilterDescriptor == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutFilterDescriptor = (PPCFILTER_DESCRIPTOR)&g_VirtIoSndTopoFilterDescriptor;
    return STATUS_SUCCESS;
}

static NTSTATUS STDMETHODCALLTYPE
VirtIoSndTopologyMiniport_DataRangeIntersection(
    _In_ IMiniportTopology *This,
    _In_ ULONG PinId,
    _In_ PKSDATARANGE DataRange,
    _In_ PKSDATARANGE MatchingDataRange,
    _In_ ULONG OutputBufferLength,
    _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength) PVOID ResultantFormat,
    _Out_ PULONG ResultantFormatLength
    )
{
    UNREFERENCED_PARAMETER(This);
    UNREFERENCED_PARAMETER(PinId);
    UNREFERENCED_PARAMETER(DataRange);
    UNREFERENCED_PARAMETER(MatchingDataRange);
    UNREFERENCED_PARAMETER(OutputBufferLength);
    UNREFERENCED_PARAMETER(ResultantFormat);
    UNREFERENCED_PARAMETER(ResultantFormatLength);

    return STATUS_NOT_SUPPORTED;
}

static const IMiniportTopologyVtbl g_VirtIoSndTopologyMiniportVtbl = {
    VirtIoSndTopologyMiniport_QueryInterface,
    VirtIoSndTopologyMiniport_AddRef,
    VirtIoSndTopologyMiniport_Release,
    VirtIoSndTopologyMiniport_Init,
    VirtIoSndTopologyMiniport_GetDescription,
    VirtIoSndTopologyMiniport_DataRangeIntersection,
};

NTSTATUS
VirtIoSndMiniportTopology_Create(_Outptr_result_maybenull_ PUNKNOWN *OutUnknown)
{
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;

    if (OutUnknown == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutUnknown = NULL;

    miniport = (PVIRTIOSND_TOPOLOGY_MINIPORT)ExAllocatePoolWithTag(NonPagedPool, sizeof(*miniport), VIRTIOSND_POOL_TAG);
    if (miniport == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(miniport, sizeof(*miniport));
    miniport->Interface.lpVtbl = &g_VirtIoSndTopologyMiniportVtbl;
    miniport->RefCount = 1;

    *OutUnknown = (PUNKNOWN)&miniport->Interface;
    return STATUS_SUCCESS;
}
