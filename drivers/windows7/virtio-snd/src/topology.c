/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"
#include "adapter_context.h"
#include "topology.h"
#include "trace.h"
#include "virtiosnd_jack.h"
#if defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
#include "aero_virtio_snd_ioport.h"
#else
#include "virtiosnd.h"
#endif

#define VIRTIOSND_TOPOLOGY_SIGNATURE 'poTV' /* 'VT' + 'op' (endianness depends on debugger display) */

#ifndef KSAUDIO_SPEAKER_MONO
// Some WDK environments may not define KSAUDIO_SPEAKER_MONO; it maps to FRONT_CENTER.
#define KSAUDIO_SPEAKER_MONO SPEAKER_FRONT_CENTER
#endif

/*
 * PortCls event verbs are defined by portcls.h, but older header environments may
 * omit them. Provide conservative fallbacks so the driver continues to build.
 */
#ifndef PCEVENT_VERB_ADD
#define PCEVENT_VERB_ADD 0x00000001u
#endif
#ifndef PCEVENT_VERB_REMOVE
#define PCEVENT_VERB_REMOVE 0x00000002u
#endif
#ifndef PCEVENT_VERB_SUPPORT
#define PCEVENT_VERB_SUPPORT 0x00000004u
#endif

typedef struct _VIRTIOSND_TOPOLOGY_MINIPORT {
    IMiniportTopology Interface;
    ULONG Signature;
    LONG RefCount;
    VIRTIOSND_PORTCLS_DX Dx;

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    ULONG RenderChannelMask;
    ULONG CaptureChannelMask;
    UCHAR RenderChannelsMin;
    UCHAR RenderChannelsMax;
    UCHAR CaptureChannelsMin;
    UCHAR CaptureChannelsMax;
#endif
} VIRTIOSND_TOPOLOGY_MINIPORT, *PVIRTIOSND_TOPOLOGY_MINIPORT;

static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_AddRef(_In_ IMiniportTopology *This);
static ULONG STDMETHODCALLTYPE VirtIoSndTopologyMiniport_Release(_In_ IMiniportTopology *This);

static PVIRTIOSND_TOPOLOGY_MINIPORT VirtIoSndTopoMiniportFromPropertyRequest(_In_opt_ PPCPROPERTY_REQUEST PropertyRequest);
static BOOLEAN VirtIoSndTopoChannelMaskForChannels(_In_ USHORT Channels, _Out_ PULONG OutChannelMask);
static BOOLEAN VirtIoSndTopoChannelsForChannelMask(_In_ ULONG ChannelMask, _Out_ PUSHORT OutChannels);

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
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;
    UNREFERENCED_PARAMETER(ResourceList);
    UNREFERENCED_PARAMETER(Port);

    if (ServiceGroup != NULL) {
        *ServiceGroup = NULL;
    }

    miniport = VirtIoSndTopologyMiniportFromInterface(This);
    if (miniport != NULL) {
        BOOLEAN forceNullBackend;
        VIRTIOSND_PORTCLS_DX dx;

        /*
         * Cache the device extension pointer so jack properties can reflect
         * virtio-snd eventq JACK notifications.
         */
        forceNullBackend = FALSE;
        dx = VirtIoSndAdapterContext_Lookup(UnknownAdapter, &forceNullBackend);
        miniport->Dx = dx;

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        {
            ULONG mask;
            USHORT channels;

            miniport->RenderChannelsMin = VIRTIOSND_CHANNELS;
            miniport->RenderChannelsMax = VIRTIOSND_CHANNELS;
            miniport->CaptureChannelsMin = VIRTIOSND_CAPTURE_CHANNELS;
            miniport->CaptureChannelsMax = VIRTIOSND_CAPTURE_CHANNELS;
            miniport->RenderChannelMask = KSAUDIO_SPEAKER_STEREO;
            miniport->CaptureChannelMask = KSAUDIO_SPEAKER_MONO;

            if (!forceNullBackend && dx != NULL && InterlockedCompareExchange(&dx->Control.CapsValid, 0, 0) != 0) {
                miniport->RenderChannelsMin = dx->Control.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMin;
                miniport->RenderChannelsMax = dx->Control.Caps[VIRTIO_SND_PLAYBACK_STREAM_ID].ChannelsMax;
                miniport->CaptureChannelsMin = dx->Control.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMin;
                miniport->CaptureChannelsMax = dx->Control.Caps[VIRTIO_SND_CAPTURE_STREAM_ID].ChannelsMax;

                channels = 2;
                if (channels < miniport->RenderChannelsMin || channels > miniport->RenderChannelsMax) {
                    channels = (USHORT)miniport->RenderChannelsMax;
                    if (channels > 8) {
                        channels = 8;
                    }
                    if (channels < miniport->RenderChannelsMin) {
                        channels = miniport->RenderChannelsMin;
                    }
                }

                mask = 0;
                if (VirtIoSndTopoChannelMaskForChannels(channels, &mask)) {
                    miniport->RenderChannelMask = mask;
                }

                channels = 1;
                if (channels < miniport->CaptureChannelsMin || channels > miniport->CaptureChannelsMax) {
                    channels = (USHORT)miniport->CaptureChannelsMax;
                    if (channels > 8) {
                        channels = 8;
                    }
                    if (channels < miniport->CaptureChannelsMin) {
                        channels = miniport->CaptureChannelsMin;
                    }
                }

                mask = 0;
                if (VirtIoSndTopoChannelMaskForChannels(channels, &mask)) {
                    miniport->CaptureChannelMask = mask;
                }
            }
        }
#endif
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

/*
 * Jack connection state model.
 *
 * Exposed via KSPROPERTY_JACK_DESCRIPTION / KSPROPERTY_JACK_DESCRIPTION2.
 *
 * Default to "connected" to preserve existing behavior when the device does not
 * emit virtio-snd jack events.
 */
static volatile LONG g_VirtIoSndTopoJackConnected[VIRTIOSND_JACK_ID_COUNT] = {1, 1};

/*
 * Jack state change notification (KSEVENTSETID_Jack / KSEVENT_JACK_INFO_CHANGE).
 *
 * Windows components (e.g. MMDevAPI/UI) typically register for this event to
 * refresh jack connection state without polling. If no clients register, the
 * event list remains empty and state changes are still reflected via property
 * reads.
 */
static LIST_ENTRY g_VirtIoSndTopoJackInfoChangeEventList[VIRTIOSND_JACK_ID_COUNT] = {
    {&g_VirtIoSndTopoJackInfoChangeEventList[0], &g_VirtIoSndTopoJackInfoChangeEventList[0]},
    {&g_VirtIoSndTopoJackInfoChangeEventList[1], &g_VirtIoSndTopoJackInfoChangeEventList[1]},
};

static KSPIN_LOCK g_VirtIoSndTopoJackEventListLock = 0;

_Use_decl_annotations_
VOID VirtIoSndTopology_Initialize(VOID)
{
    /*
     * PortCls/KS can dispatch property/event requests concurrently on multiple
     * threads/IRQLs. Keep global helper state explicitly initialized rather than
     * relying on BSS zero-initialization semantics.
     */
    KeInitializeSpinLock(&g_VirtIoSndTopoJackEventListLock);

    /*
     * Reset the default jack state at driver load so early property queries
     * (before any eventq notifications arrive) report a connected endpoint.
     */
    VirtIoSndTopology_ResetJackState();
}

static VOID VirtIoSndTopoNotifyJackInfoChange(_In_ ULONG JackId)
{
    KIRQL oldIrql;

    if (JackId >= RTL_NUMBER_OF(g_VirtIoSndTopoJackInfoChangeEventList)) {
        return;
    }

    /*
     * PortCls helper: generate notifications for all registered listeners.
     *
     * IRQL: <= DISPATCH_LEVEL (called from the virtio INTx DPC and from the
     * WaveRT period DPC in polling-only mode).
     */
    KeAcquireSpinLock(&g_VirtIoSndTopoJackEventListLock, &oldIrql);
    PcGenerateEventList(&g_VirtIoSndTopoJackInfoChangeEventList[JackId]);
    KeReleaseSpinLock(&g_VirtIoSndTopoJackEventListLock, oldIrql);
}

_Use_decl_annotations_
VOID VirtIoSndTopology_ResetJackState(VOID)
{
    ULONG i;
    for (i = 0; i < RTL_NUMBER_OF(g_VirtIoSndTopoJackConnected); ++i) {
        (VOID)InterlockedExchange(&g_VirtIoSndTopoJackConnected[i], 1);
    }
}

_Use_decl_annotations_
VOID VirtIoSndTopology_UpdateJackState(ULONG JackId, BOOLEAN IsConnected)
{
    VirtIoSndTopology_UpdateJackStateEx(JackId, IsConnected, FALSE);
}

_Use_decl_annotations_
VOID VirtIoSndTopology_UpdateJackStateEx(ULONG JackId, BOOLEAN IsConnected, BOOLEAN NotifyEvenIfUnchanged)
{
    LONG v;
    LONG old;

    if (JackId >= RTL_NUMBER_OF(g_VirtIoSndTopoJackConnected)) {
        return;
    }

    v = IsConnected ? 1 : 0;
    old = InterlockedExchange(&g_VirtIoSndTopoJackConnected[JackId], v);
    if (old != v || NotifyEvenIfUnchanged) {
        VirtIoSndTopoNotifyJackInfoChange(JackId);
    }
}

_Use_decl_annotations_
BOOLEAN VirtIoSndTopology_IsJackConnected(ULONG JackId)
{
    if (JackId >= RTL_NUMBER_OF(g_VirtIoSndTopoJackConnected)) {
        /*
         * Unknown jack IDs are treated as "connected" so that a device that
         * never sends jack events (or sends events for jacks we do not expose)
         * does not accidentally make the endpoint appear disconnected.
         */
        return TRUE;
    }

    return (InterlockedCompareExchange(&g_VirtIoSndTopoJackConnected[JackId], 0, 0) != 0) ? TRUE : FALSE;
}

static PVIRTIOSND_TOPOLOGY_MINIPORT
VirtIoSndTopoMiniportFromPropertyRequest(_In_opt_ PPCPROPERTY_REQUEST PropertyRequest)
{
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;

    if (PropertyRequest == NULL) {
        return NULL;
    }

    miniport = (PVIRTIOSND_TOPOLOGY_MINIPORT)PropertyRequest->MajorTarget;
    if (miniport != NULL && miniport->Signature == VIRTIOSND_TOPOLOGY_SIGNATURE) {
        return miniport;
    }

    miniport = (PVIRTIOSND_TOPOLOGY_MINIPORT)PropertyRequest->MinorTarget;
    if (miniport != NULL && miniport->Signature == VIRTIOSND_TOPOLOGY_SIGNATURE) {
        return miniport;
    }

    return NULL;
}

static BOOLEAN
VirtIoSndTopoChannelMaskForChannels(_In_ USHORT Channels, _Out_ PULONG OutChannelMask)
{
    if (OutChannelMask == NULL) {
        return FALSE;
    }

    switch (Channels) {
    case 1:
        *OutChannelMask = KSAUDIO_SPEAKER_MONO;
        return TRUE;
    case 2:
        *OutChannelMask = KSAUDIO_SPEAKER_STEREO;
        return TRUE;
    case 3:
        *OutChannelMask = KSAUDIO_SPEAKER_STEREO | SPEAKER_FRONT_CENTER;
        return TRUE;
    case 4:
        *OutChannelMask = KSAUDIO_SPEAKER_QUAD;
        return TRUE;
    case 5:
        *OutChannelMask = KSAUDIO_SPEAKER_QUAD | SPEAKER_FRONT_CENTER;
        return TRUE;
    case 6:
        *OutChannelMask = KSAUDIO_SPEAKER_5POINT1;
        return TRUE;
    case 7:
        *OutChannelMask = KSAUDIO_SPEAKER_5POINT1 | SPEAKER_BACK_CENTER;
        return TRUE;
    case 8:
        *OutChannelMask = KSAUDIO_SPEAKER_7POINT1;
        return TRUE;
    default:
        *OutChannelMask = 0;
        return FALSE;
    }
}

static BOOLEAN
VirtIoSndTopoChannelsForChannelMask(_In_ ULONG ChannelMask, _Out_ PUSHORT OutChannels)
{
    if (OutChannels == NULL) {
        return FALSE;
    }

    switch (ChannelMask) {
    case KSAUDIO_SPEAKER_MONO:
        *OutChannels = 1;
        return TRUE;
    case KSAUDIO_SPEAKER_STEREO:
        *OutChannels = 2;
        return TRUE;
    case KSAUDIO_SPEAKER_STEREO | SPEAKER_FRONT_CENTER:
        *OutChannels = 3;
        return TRUE;
    case KSAUDIO_SPEAKER_QUAD:
        *OutChannels = 4;
        return TRUE;
    case KSAUDIO_SPEAKER_QUAD | SPEAKER_FRONT_CENTER:
        *OutChannels = 5;
        return TRUE;
    case KSAUDIO_SPEAKER_5POINT1:
        *OutChannels = 6;
        return TRUE;
    case KSAUDIO_SPEAKER_5POINT1 | SPEAKER_BACK_CENTER:
        *OutChannels = 7;
        return TRUE;
    case KSAUDIO_SPEAKER_7POINT1:
        *OutChannels = 8;
        return TRUE;
    default:
        *OutChannels = 0;
        return FALSE;
    }
}

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
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    miniport = VirtIoSndTopoMiniportFromPropertyRequest(PropertyRequest);

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

        *(PULONG)PropertyRequest->Value =
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
            (miniport != NULL && miniport->RenderChannelMask != 0) ? miniport->RenderChannelMask :
#endif
            KSAUDIO_SPEAKER_STEREO;
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG mask;
        USHORT channels;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        mask = *(const ULONG *)PropertyRequest->Value;

        channels = 0;
        if (!VirtIoSndTopoChannelsForChannelMask(mask, &channels) || channels == 0) {
            return STATUS_INVALID_PARAMETER;
        }

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        if (miniport != NULL) {
            if ((UCHAR)channels < miniport->RenderChannelsMin || (UCHAR)channels > miniport->RenderChannelsMax) {
                return STATUS_INVALID_PARAMETER;
            }

            miniport->RenderChannelMask = mask;
            return STATUS_SUCCESS;
        }
#endif

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
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    miniport = VirtIoSndTopoMiniportFromPropertyRequest(PropertyRequest);

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

        *(PULONG)PropertyRequest->Value =
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
            (miniport != NULL && miniport->CaptureChannelMask != 0) ? miniport->CaptureChannelMask :
#endif
            KSAUDIO_SPEAKER_MONO;
        PropertyRequest->ValueSize = sizeof(ULONG);
        return STATUS_SUCCESS;
    }

    if (PropertyRequest->Verb & KSPROPERTY_TYPE_SET) {
        ULONG mask;
        USHORT channels;

        if (PropertyRequest->Value == NULL || PropertyRequest->ValueSize < sizeof(ULONG)) {
            return STATUS_INVALID_PARAMETER;
        }

        mask = *(const ULONG *)PropertyRequest->Value;

        channels = 0;
        if (!VirtIoSndTopoChannelsForChannelMask(mask, &channels) || channels == 0) {
            return STATUS_INVALID_PARAMETER;
        }

#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        if (miniport != NULL) {
            if ((UCHAR)channels < miniport->CaptureChannelsMin || (UCHAR)channels > miniport->CaptureChannelsMax) {
                return STATUS_INVALID_PARAMETER;
            }

            miniport->CaptureChannelMask = mask;
            return STATUS_SUCCESS;
        }
#endif

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
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;
    BOOLEAN connected;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    miniport = VirtIoSndTopoMiniportFromPropertyRequest(PropertyRequest);

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
    jack->ChannelMapping =
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        (miniport != NULL && miniport->RenderChannelMask != 0) ? miniport->RenderChannelMask :
#endif
        KSAUDIO_SPEAKER_STEREO;
    connected = VirtIoSndTopology_IsJackConnected(VIRTIOSND_JACK_ID_SPEAKER);
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    if (miniport != NULL && miniport->Dx != NULL) {
        connected = VirtIoSndJackStateIsConnected(&miniport->Dx->JackState, VIRTIOSND_JACK_ID_SPEAKER);
    }
#endif
    jack->IsConnected = connected ? TRUE : FALSE;

    PropertyRequest->ValueSize = required;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndProperty_JackDescriptionMono(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    ULONG required;
    KSMULTIPLE_ITEM *item;
    KSJACK_DESCRIPTION *jack;
    PVIRTIOSND_TOPOLOGY_MINIPORT miniport;
    BOOLEAN connected;

    if (PropertyRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    miniport = VirtIoSndTopoMiniportFromPropertyRequest(PropertyRequest);

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
    jack->ChannelMapping =
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
        (miniport != NULL && miniport->CaptureChannelMask != 0) ? miniport->CaptureChannelMask :
#endif
        KSAUDIO_SPEAKER_MONO;
    connected = VirtIoSndTopology_IsJackConnected(VIRTIOSND_JACK_ID_MICROPHONE);
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    if (miniport != NULL && miniport->Dx != NULL) {
        connected = VirtIoSndJackStateIsConnected(&miniport->Dx->JackState, VIRTIOSND_JACK_ID_MICROPHONE);
    }
#endif
    jack->IsConnected = connected ? TRUE : FALSE;

    PropertyRequest->ValueSize = required;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndProperty_JackDescription2Common(_In_ PPCPROPERTY_REQUEST PropertyRequest, _In_ ULONG JackId)
{
    ULONG required;
    KSMULTIPLE_ITEM *item;
    KSJACK_DESCRIPTION2 *jack;
    BOOLEAN connected;

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
    connected = VirtIoSndTopology_IsJackConnected(JackId);
#if !defined(AERO_VIRTIO_SND_IOPORT_LEGACY)
    {
        PVIRTIOSND_TOPOLOGY_MINIPORT miniport = VirtIoSndTopoMiniportFromPropertyRequest(PropertyRequest);
        if (miniport != NULL && miniport->Dx != NULL) {
            connected = VirtIoSndJackStateIsConnected(&miniport->Dx->JackState, JackId);
        }
    }
#endif

    /*
     * KSJACK_DESCRIPTION2 exposes connection state via DeviceStateInfo.
     * Prefer the symbolic constant when available, but fall back to bit 0
     * (the documented "connected" bit) for older WDK environments.
     */
#ifndef KSJACK_DEVICE_STATE_CONNECTED
#define KSJACK_DEVICE_STATE_CONNECTED 0x00000001u
#endif
    jack->DeviceStateInfo = connected ? KSJACK_DEVICE_STATE_CONNECTED : 0u;
#ifdef KSJACK_CAPABILITY_PRESENCE_DETECT
    jack->JackCapabilities = KSJACK_CAPABILITY_PRESENCE_DETECT;
#endif

    PropertyRequest->ValueSize = required;
    return STATUS_SUCCESS;
}

static NTSTATUS
VirtIoSndProperty_JackDescription2Speaker(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    return VirtIoSndProperty_JackDescription2Common(PropertyRequest, VIRTIOSND_JACK_ID_SPEAKER);
}

static NTSTATUS
VirtIoSndProperty_JackDescription2Mic(_In_ PPCPROPERTY_REQUEST PropertyRequest)
{
    return VirtIoSndProperty_JackDescription2Common(PropertyRequest, VIRTIOSND_JACK_ID_MICROPHONE);
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

static NTSTATUS
VirtIoSndEvent_JackInfoChangeCommon(_In_ PPCEVENT_REQUEST EventRequest, _In_ ULONG JackId)
{
    KIRQL oldIrql;
    NTSTATUS status;

    if (EventRequest == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (JackId >= RTL_NUMBER_OF(g_VirtIoSndTopoJackInfoChangeEventList)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (EventRequest->Verb & PCEVENT_VERB_SUPPORT) {
        /* Best-effort: PortCls will validate the event item metadata. */
        return STATUS_SUCCESS;
    }

    if (EventRequest->Verb & PCEVENT_VERB_ADD) {
        KeAcquireSpinLock(&g_VirtIoSndTopoJackEventListLock, &oldIrql);
        status = PcAddToEventList(&g_VirtIoSndTopoJackInfoChangeEventList[JackId], EventRequest);
        KeReleaseSpinLock(&g_VirtIoSndTopoJackEventListLock, oldIrql);
        return status;
    }

    if (EventRequest->Verb & PCEVENT_VERB_REMOVE) {
        KeAcquireSpinLock(&g_VirtIoSndTopoJackEventListLock, &oldIrql);
        status = PcRemoveFromEventList(&g_VirtIoSndTopoJackInfoChangeEventList[JackId], EventRequest);
        KeReleaseSpinLock(&g_VirtIoSndTopoJackEventListLock, oldIrql);
        return status;
    }

    return STATUS_INVALID_PARAMETER;
}

static NTSTATUS
VirtIoSndEvent_JackInfoChangeSpeaker(_In_ PPCEVENT_REQUEST EventRequest)
{
    return VirtIoSndEvent_JackInfoChangeCommon(EventRequest, VIRTIOSND_JACK_ID_SPEAKER);
}

static NTSTATUS
VirtIoSndEvent_JackInfoChangeMic(_In_ PPCEVENT_REQUEST EventRequest)
{
    return VirtIoSndEvent_JackInfoChangeCommon(EventRequest, VIRTIOSND_JACK_ID_MICROPHONE);
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
    {KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescription2Speaker},
    {KSPROPERTY_JACK_CONTAINERID, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackContainerId},
};

static const PCPROPERTY_ITEM g_VirtIoSndTopoJackPropertiesMic[] = {
    {KSPROPERTY_JACK_DESCRIPTION, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescriptionMono},
    {KSPROPERTY_JACK_DESCRIPTION2, KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_BASICSUPPORT, VirtIoSndProperty_JackDescription2Mic},
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

static const PCEVENT_ITEM g_VirtIoSndTopoJackEvents[] = {
    {KSEVENT_JACK_INFO_CHANGE, 0, VirtIoSndEvent_JackInfoChangeSpeaker},
};

static const PCEVENT_SET g_VirtIoSndTopoEventSets[] = {
    {&KSEVENTSETID_Jack, RTL_NUMBER_OF(g_VirtIoSndTopoJackEvents), g_VirtIoSndTopoJackEvents},
};

static const PCEVENT_ITEM g_VirtIoSndTopoJackEventsMic[] = {
    {KSEVENT_JACK_INFO_CHANGE, 0, VirtIoSndEvent_JackInfoChangeMic},
};

static const PCEVENT_SET g_VirtIoSndTopoEventSetsMic[] = {
    {&KSEVENTSETID_Jack, RTL_NUMBER_OF(g_VirtIoSndTopoJackEventsMic), g_VirtIoSndTopoJackEventsMic},
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoAutomation = {
    RTL_NUMBER_OF(g_VirtIoSndTopoPropertySets),
    g_VirtIoSndTopoPropertySets,
    0,
    NULL,
    RTL_NUMBER_OF(g_VirtIoSndTopoEventSets),
    g_VirtIoSndTopoEventSets,
};

static const PCAUTOMATION_TABLE g_VirtIoSndTopoAutomationMic = {
    RTL_NUMBER_OF(g_VirtIoSndTopoPropertySetsMic),
    g_VirtIoSndTopoPropertySetsMic,
    0,
    NULL,
    RTL_NUMBER_OF(g_VirtIoSndTopoEventSetsMic),
    g_VirtIoSndTopoEventSetsMic,
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
    miniport->Signature = VIRTIOSND_TOPOLOGY_SIGNATURE;
    miniport->RefCount = 1;

    *OutUnknown = (PUNKNOWN)&miniport->Interface;
    return STATUS_SUCCESS;
}
