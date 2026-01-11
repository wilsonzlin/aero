/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"
#include "topology.h"
#include "trace.h"
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
#include "aero_virtio_snd_ioport.h"
#else
#include "virtiosnd.h"
#endif

#ifndef KSAUDIO_SPEAKER_MONO
// Some WDK environments may not define KSAUDIO_SPEAKER_MONO; it maps to FRONT_CENTER.
#define KSAUDIO_SPEAKER_MONO SPEAKER_FRONT_CENTER
#endif

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

#define VIRTIOSND_TOPO_NODE_VOLUME 0
#define VIRTIOSND_TOPO_NODE_MUTE 1
#define VIRTIOSND_TOPO_NODE_SPEAKER 2
#define VIRTIOSND_TOPO_NODE_MICROPHONE 3

/*
 * Minimal software-backed endpoint volume/mute state.
 *
 * These values are not applied to the WaveRT render stream yet; they exist so
 * common Win7 audio stack components (WDMAudio/KSProxy/MMDevice UI) can discover
 * a reasonable topology and round-trip volume/mute properties without error.
 */
static volatile LONG g_VirtIoSndTopoVolumeDb[2] = {0, 0}; // per-channel dB value (driver-defined units)
static volatile LONG g_VirtIoSndTopoMute[2] = {0, 0};     // per-channel mute flag (0/1)

static BOOLEAN
VirtIoSndTopoTryGetChannel(_In_ PPCPROPERTY_REQUEST PropertyRequest, _Out_ ULONG* OutChannel)
{
    const KSNODEPROPERTY_AUDIO_CHANNEL *inst;

    if (OutChannel == NULL) {
        return FALSE;
    }

    *OutChannel = 0;

    if (PropertyRequest == NULL || PropertyRequest->Instance == NULL) {
        return FALSE;
    }

    /*
     * Volume/mute properties are typically node-targeted and pass a
     * KSNODEPROPERTY_AUDIO_CHANNEL instance with a Channel field. If the
     * caller provides some other instance format, treat the request as master
     * volume/mute (channel 0).
     */
    if (PropertyRequest->InstanceSize < sizeof(*inst)) {
        return FALSE;
    }

    inst = (const KSNODEPROPERTY_AUDIO_CHANNEL *)PropertyRequest->Instance;
    *OutChannel = inst->Channel;
    return TRUE;
}

static NTSTATUS
VirtIoSndProperty_ChannelConfig(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        ULONG required = sizeof(*desc);
        ULONG access = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = access;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
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
VirtIoSndProperty_ChannelConfigMono(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        ULONG required = sizeof(*desc);
        ULONG access = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = access;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_GET) {
        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            PropertyRequest->ValueSize = sizeof(ULONG);
            return STATUS_BUFFER_TOO_SMALL;
        }

        *(PULONG)PropertyRequest->Value = KSAUDIO_SPEAKER_MONO;
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG mask;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        mask = *(const ULONG *)PropertyRequest->Value;
        if (mask != KSAUDIO_SPEAKER_MONO) {
            return STATUS_INVALID_PARAMETER;
        }

        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static NTSTATUS
VirtIoSndProperty_VolumeLevel(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG channel;
    LONG level;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        ULONG required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_GET) {
        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(LONG)) {
            PropertyRequest->ValueSize = sizeof(LONG);
            return STATUS_BUFFER_TOO_SMALL;
        }

        channel = 0;
        if (VirtIoSndTopoTryGetChannel(PropertyRequest, &channel) && channel < RTL_NUMBER_OF(g_VirtIoSndTopoVolumeDb)) {
            level = InterlockedCompareExchange(&g_VirtIoSndTopoVolumeDb[channel], 0, 0);
        } else {
            level = InterlockedCompareExchange(&g_VirtIoSndTopoVolumeDb[0], 0, 0);
        }

        *(PLONG)PropertyRequest->Value = level;
        PropertyRequest->ValueSize = sizeof(LONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG i;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(LONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        level = *(const LONG *)PropertyRequest->Value;

        channel = 0;
        if (VirtIoSndTopoTryGetChannel(PropertyRequest, &channel) && channel < RTL_NUMBER_OF(g_VirtIoSndTopoVolumeDb)) {
            (VOID)InterlockedExchange(&g_VirtIoSndTopoVolumeDb[channel], level);
        } else {
            for (i = 0; i < RTL_NUMBER_OF(g_VirtIoSndTopoVolumeDb); ++i) {
                (VOID)InterlockedExchange(&g_VirtIoSndTopoVolumeDb[i], level);
            }
        }

        return STATUS_SUCCESS;
    }

    return STATUS_INVALID_PARAMETER;
}

static NTSTATUS
VirtIoSndProperty_Mute(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG channel;
    ULONG mute;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        ULONG required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_GET) {
        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            PropertyRequest->ValueSize = sizeof(ULONG);
            return STATUS_BUFFER_TOO_SMALL;
        }

        channel = 0;
        if (VirtIoSndTopoTryGetChannel(PropertyRequest, &channel) && channel < RTL_NUMBER_OF(g_VirtIoSndTopoMute)) {
            mute = (ULONG)InterlockedCompareExchange(&g_VirtIoSndTopoMute[channel], 0, 0);
        } else {
            mute = (ULONG)InterlockedCompareExchange(&g_VirtIoSndTopoMute[0], 0, 0);
        }

        *(PULONG)PropertyRequest->Value = (mute != 0) ? 1u : 0u;
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG i;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        mute = (*(const ULONG *)PropertyRequest->Value != 0) ? 1u : 0u;

        channel = 0;
        if (VirtIoSndTopoTryGetChannel(PropertyRequest, &channel) && channel < RTL_NUMBER_OF(g_VirtIoSndTopoMute)) {
            (VOID)InterlockedExchange(&g_VirtIoSndTopoMute[channel], (LONG)mute);
        } else {
            for (i = 0; i < RTL_NUMBER_OF(g_VirtIoSndTopoMute); ++i) {
                (VOID)InterlockedExchange(&g_VirtIoSndTopoMute[i], (LONG)mute);
            }
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

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
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
VirtIoSndProperty_JackDescriptionMono(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG required;
    KSMULTIPLE_ITEM *item;
    KSJACK_DESCRIPTION *jack;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
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
    jack->ChannelMapping = KSAUDIO_SPEAKER_MONO;
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

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
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

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_BASICSUPPORT) {
        KSPROPERTY_DESCRIPTION *desc;
        ULONG required = sizeof(*desc);

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < required) {
            PropertyRequest->ValueSize = required;
            return STATUS_BUFFER_TOO_SMALL;
        }

        desc = (KSPROPERTY_DESCRIPTION *)PropertyRequest->Value;
        RtlZeroMemory(desc, sizeof(*desc));
        desc->AccessFlags = KSPROPERTY_TYPE_GET;
        desc->DescriptionSize = required;
        PropertyRequest->ValueSize = required;
        return STATUS_SUCCESS;
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
    {KSPROPERTY_AUDIO_CHANNEL_CONFIG, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_ChannelConfig},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoAudioPropertiesMic[] = {
    {KSPROPERTY_AUDIO_CHANNEL_CONFIG, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_ChannelConfigMono},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoVolumeProperties[] = {
    {KSPROPERTY_AUDIO_VOLUMELEVEL, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_VolumeLevel},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoMuteProperties[] = {
    {KSPROPERTY_AUDIO_MUTE, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_Mute},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoJackProperties[] = {
    {KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescription},
    {KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescription2},
    {KSPROPERTY_JACK_CONTAINERID, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackContainerId},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoJackPropertiesMic[] = {
    {KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescriptionMono},
    {KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescription2},
    {KSPROPERTY_JACK_CONTAINERID, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackContainerId},
};

static const PCPROPERTY_SET g_VirtIoSndTopoPropertySets[] = {
    {&KSPROPSETID_Audio, RTL_NUMBER_OF(g_VirtIoSndTopoAudioProperties), g_VirtIoSndTopoAudioProperties},
    {&KSPROPSETID_Jack, RTL_NUMBER_OF(g_VirtIoSndTopoJackProperties), g_VirtIoSndTopoJackProperties},
};

static const PCPROPERTY_SET g_VirtIoSndTopoPropertySetsMic[] = {
    {&KSPROPSETID_Audio, RTL_NUMBER_OF(g_VirtIoSndTopoAudioPropertiesMic), g_VirtIoSndTopoAudioPropertiesMic},
    {&KSPROPSETID_Jack, RTL_NUMBER_OF(g_VirtIoSndTopoJackPropertiesMic), g_VirtIoSndTopoJackPropertiesMic},
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoAutomation = {
    RTL_NUMBER_OF(g_VirtIoSndTopoPropertySets),
    g_VirtIoSndTopoPropertySets,
    0,
    NULL,
    0,
    NULL,
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoAutomationMic = {
    RTL_NUMBER_OF(g_VirtIoSndTopoPropertySetsMic),
    g_VirtIoSndTopoPropertySetsMic,
    0,
    NULL,
    0,
    NULL,
};

static const PCPROPERTY_SET g_VirtIoSndTopoVolumePropertySets[] = {
    {&KSPROPSETID_Audio, RTL_NUMBER_OF(g_VirtIoSndTopoVolumeProperties), g_VirtIoSndTopoVolumeProperties},
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoVolumeAutomation = {
    RTL_NUMBER_OF(g_VirtIoSndTopoVolumePropertySets),
    g_VirtIoSndTopoVolumePropertySets,
    0,
    NULL,
    0,
    NULL,
};

static const PCPROPERTY_SET g_VirtIoSndTopoMutePropertySets[] = {
    {&KSPROPSETID_Audio, RTL_NUMBER_OF(g_VirtIoSndTopoMuteProperties), g_VirtIoSndTopoMuteProperties},
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoMuteAutomation = {
    RTL_NUMBER_OF(g_VirtIoSndTopoMutePropertySets),
    g_VirtIoSndTopoMutePropertySets,
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
    // VIRTIOSND_TOPO_PIN_BRIDGE_CAPTURE
    {0, NULL, 0, NULL, 0, NULL, KSPIN_DATAFLOW_OUT, KSPIN_COMMUNICATION_BRIDGE, &KSNODETYPE_WAVE_IN, &KSPINNAME_WAVE_IN},
    // VIRTIOSND_TOPO_PIN_MICROPHONE
    {0, NULL, 0, NULL, 0, NULL, KSPIN_DATAFLOW_IN, KSPIN_COMMUNICATION_NONE, &KSNODETYPE_MICROPHONE, &KSPINNAME_MICROPHONE},
};

static const PCPIN_DESCRIPTOR g_VirtIoSndTopoPins[] = {
    {1, 1, 0, NULL, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_BRIDGE]},
    {1, 1, 0, &g_VirtIoSndTopoAutomation, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_SPEAKER]},
    {1, 1, 0, NULL, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_BRIDGE_CAPTURE]},
    {1, 1, 0, &g_VirtIoSndTopoAutomationMic, g_VirtIoSndTopoPinDescriptors[VIRTIOSND_TOPO_PIN_MICROPHONE]},
};

static const PCNODE_DESCRIPTOR g_VirtIoSndTopoNodes[] = {
    // Node 0: volume (software-backed).
    {0, &g_VirtIoSndTopoVolumeAutomation, &KSNODETYPE_VOLUME, NULL},
    // Node 1: mute (software-backed).
    {0, &g_VirtIoSndTopoMuteAutomation, &KSNODETYPE_MUTE, NULL},
    // Node 2: speaker connector.
    {0, &g_VirtIoSndTopoAutomation, &KSNODETYPE_SPEAKER, NULL},
    // Node 3: microphone connector.
    {0, &g_VirtIoSndTopoAutomationMic, &KSNODETYPE_MICROPHONE, NULL},
};

static const PCCONNECTION_DESCRIPTOR g_VirtIoSndTopoConnections[] = {
    {KSFILTER_NODE, VIRTIOSND_TOPO_PIN_BRIDGE, VIRTIOSND_TOPO_NODE_VOLUME, 0},
    {VIRTIOSND_TOPO_NODE_VOLUME, 1, VIRTIOSND_TOPO_NODE_MUTE, 0},
    {VIRTIOSND_TOPO_NODE_MUTE, 1, VIRTIOSND_TOPO_NODE_SPEAKER, 0},
    {VIRTIOSND_TOPO_NODE_SPEAKER, 0, KSFILTER_NODE, VIRTIOSND_TOPO_PIN_SPEAKER},
    {KSFILTER_NODE, VIRTIOSND_TOPO_PIN_MICROPHONE, VIRTIOSND_TOPO_NODE_MICROPHONE, 0},
    {VIRTIOSND_TOPO_NODE_MICROPHONE, 0, KSFILTER_NODE, VIRTIOSND_TOPO_PIN_BRIDGE_CAPTURE},
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
