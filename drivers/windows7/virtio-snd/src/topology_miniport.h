#pragma once

#include <ntddk.h>
#include <portcls.h>
#include <ks.h>
#include <ksmedia.h>

//
// PortCls topology miniport for virtio-snd (Windows 7).
//
// This miniport is intentionally minimal: it provides the topology filter that
// Windows 7 expects for endpoint enumeration and basic KS topology discovery.
//

enum VIRTIO_SND_TOPOLOGY_PIN : ULONG {
    //
    // Bridge pin that is physically connected to the WaveRT filter's bridge pin
    // via PcRegisterPhysicalConnection (adapter driver).
    //
    VIRTIO_SND_TOPOLOGY_PIN_WAVE_BRIDGE = 0,
    //
    // Physical render destination ("speaker") pin.
    //
    VIRTIO_SND_TOPOLOGY_PIN_SPEAKER = 1,
    VIRTIO_SND_TOPOLOGY_PIN_COUNT
};

extern "C" NTSTATUS CreateMiniportTopology(
    _Out_ PUNKNOWN* Unknown,
    _In_ REFCLSID RefClassId,
    _In_opt_ PUNKNOWN OuterUnknown,
    _In_ POOL_TYPE PoolType);

