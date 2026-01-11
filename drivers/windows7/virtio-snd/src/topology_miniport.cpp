/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <initguid.h>

#include "topology_miniport.h"

#include "trace.h"
#include "virtiosnd.h"

namespace {

class TopologyMiniport : public IMiniportTopology {
public:
    explicit TopologyMiniport(_In_opt_ PUNKNOWN OuterUnknown)
        : m_RefCount(1), m_OuterUnknown(OuterUnknown), m_NonDelegatingUnknown(this), m_AdapterUnknown(NULL)
    {
    }

    virtual ~TopologyMiniport()
    {
        if (m_AdapterUnknown != NULL) {
            m_AdapterUnknown->Release();
            m_AdapterUnknown = NULL;
        }
    }

    void* operator new(size_t Size, POOL_TYPE PoolType)
    {
        return ExAllocatePoolWithTag(PoolType, Size, VIRTIOSND_POOL_TAG);
    }

    void operator delete(void* Ptr)
    {
        if (Ptr != NULL) {
            ExFreePoolWithTag(Ptr, VIRTIOSND_POOL_TAG);
        }
    }

    void operator delete(void* Ptr, POOL_TYPE PoolType)
    {
        UNREFERENCED_PARAMETER(PoolType);
        operator delete(Ptr);
    }

    PUNKNOWN GetUnknownForCreate() { return reinterpret_cast<PUNKNOWN>(&m_NonDelegatingUnknown); }

    //
    // IUnknown (delegating). When aggregated, forward to the outer unknown.
    //
    STDMETHODIMP QueryInterface(_In_ REFIID InterfaceId, _Outptr_ void** Object)
    {
        if (m_OuterUnknown != NULL) {
            return m_OuterUnknown->QueryInterface(InterfaceId, Object);
        }
        return NonDelegatingQueryInterface(InterfaceId, Object);
    }

    STDMETHODIMP_(ULONG) AddRef()
    {
        if (m_OuterUnknown != NULL) {
            return m_OuterUnknown->AddRef();
        }
        return NonDelegatingAddRef();
    }

    STDMETHODIMP_(ULONG) Release()
    {
        if (m_OuterUnknown != NULL) {
            return m_OuterUnknown->Release();
        }
        return NonDelegatingRelease();
    }

    //
    // IMiniport
    //
    STDMETHODIMP_(NTSTATUS)
    Init(_In_opt_ PUNKNOWN UnknownAdapter,
         _In_opt_ PRESOURCELIST ResourceList,
         _In_ PPORT Port,
         _Outptr_result_maybenull_ PSERVICEGROUP* ServiceGroup)
    {
        UNREFERENCED_PARAMETER(ResourceList);
        UNREFERENCED_PARAMETER(Port);

        if (ServiceGroup == NULL) {
            return STATUS_INVALID_PARAMETER;
        }

        *ServiceGroup = NULL;

        if (UnknownAdapter != NULL) {
            UnknownAdapter->AddRef();
            m_AdapterUnknown = UnknownAdapter;
        }

        return STATUS_SUCCESS;
    }

    STDMETHODIMP_(NTSTATUS) GetDescription(_Out_ PPCFILTER_DESCRIPTOR* OutFilterDescriptor);

    STDMETHODIMP_(NTSTATUS)
    DataRangeIntersection(_In_ ULONG PinId,
                          _In_ PKSDATARANGE DataRange,
                          _In_ PKSDATARANGE MatchingDataRange,
                          _In_ ULONG OutputBufferLength,
                          _Out_writes_bytes_to_opt_(OutputBufferLength, *ResultantFormatLength) PVOID ResultantFormat,
                          _Out_ PULONG ResultantFormatLength)
    {
        UNREFERENCED_PARAMETER(PinId);
        UNREFERENCED_PARAMETER(DataRange);
        UNREFERENCED_PARAMETER(MatchingDataRange);
        UNREFERENCED_PARAMETER(OutputBufferLength);
        UNREFERENCED_PARAMETER(ResultantFormat);
        UNREFERENCED_PARAMETER(ResultantFormatLength);

        // Topology pins do not stream data formats.
        return STATUS_NOT_SUPPORTED;
    }

private:
    class NonDelegatingUnknown : public IUnknown {
    public:
        explicit NonDelegatingUnknown(_In_ TopologyMiniport* Parent) : m_Parent(Parent) {}

        STDMETHODIMP QueryInterface(_In_ REFIID InterfaceId, _Outptr_ void** Object)
        {
            return m_Parent->NonDelegatingQueryInterface(InterfaceId, Object);
        }

        STDMETHODIMP_(ULONG) AddRef() { return m_Parent->NonDelegatingAddRef(); }

        STDMETHODIMP_(ULONG) Release() { return m_Parent->NonDelegatingRelease(); }

    private:
        TopologyMiniport* m_Parent;
    };

    friend class NonDelegatingUnknown;

    HRESULT NonDelegatingQueryInterface(_In_ REFIID InterfaceId, _Outptr_ void** Object)
    {
        if (Object == NULL) {
            return E_POINTER;
        }

        *Object = NULL;

        if (IsEqualGUID(InterfaceId, IID_IUnknown)) {
            *Object = reinterpret_cast<void*>(&m_NonDelegatingUnknown);
            NonDelegatingAddRef();
            return S_OK;
        }

        if (IsEqualGUID(InterfaceId, IID_IMiniport) ||
            IsEqualGUID(InterfaceId, IID_IMiniportTopology)) {
            *Object = static_cast<IMiniportTopology*>(this);
            AddRef();
            return S_OK;
        }

        return E_NOINTERFACE;
    }

    ULONG NonDelegatingAddRef() { return static_cast<ULONG>(InterlockedIncrement(&m_RefCount)); }

    ULONG NonDelegatingRelease()
    {
        LONG ref = InterlockedDecrement(&m_RefCount);
        if (ref == 0) {
            delete this;
            return 0;
        }
        return static_cast<ULONG>(ref);
    }

    volatile LONG m_RefCount;
    PUNKNOWN m_OuterUnknown;
    NonDelegatingUnknown m_NonDelegatingUnknown;
    PUNKNOWN m_AdapterUnknown;

    // Non-copyable (C++03 friendly).
    TopologyMiniport(const TopologyMiniport&);
    TopologyMiniport& operator=(const TopologyMiniport&);
};

//
// Topology filter descriptor (minimal render endpoint graph).
//

static const KSPIN_DESCRIPTOR g_TopologyPinDescriptors[] = {
    // VIRTIO_SND_TOPOLOGY_PIN_WAVE_BRIDGE
    {
        0,
        NULL,
        0,
        NULL,
        0,
        NULL,
        KSPIN_DATAFLOW_IN,
        KSPIN_COMMUNICATION_BRIDGE,
        &KSNODETYPE_WAVE_OUT,
        &KSPINNAME_WAVE_OUT
    },

    // VIRTIO_SND_TOPOLOGY_PIN_SPEAKER
    {
        0,
        NULL,
        0,
        NULL,
        0,
        NULL,
        KSPIN_DATAFLOW_OUT,
        KSPIN_COMMUNICATION_NONE,
        &KSNODETYPE_SPEAKER,
        &KSPINNAME_SPEAKER
    },
};

static const PCPIN_DESCRIPTOR g_TopologyPins[] = {
    {
        1,
        1,
        0,
        NULL,
        g_TopologyPinDescriptors[VIRTIO_SND_TOPOLOGY_PIN_WAVE_BRIDGE],
    },
    {
        1,
        1,
        0,
        NULL,
        g_TopologyPinDescriptors[VIRTIO_SND_TOPOLOGY_PIN_SPEAKER],
    },
};

static const PCNODE_DESCRIPTOR g_TopologyNodes[] = {
    // Node 0: speaker endpoint.
    {0, NULL, &KSNODETYPE_SPEAKER, NULL},
};

static const PCCONNECTION_DESCRIPTOR g_TopologyConnections[] = {
    {KSFILTER_NODE, VIRTIO_SND_TOPOLOGY_PIN_WAVE_BRIDGE, 0, 0},
    {0, 0, KSFILTER_NODE, VIRTIO_SND_TOPOLOGY_PIN_SPEAKER},
};

static const GUID* g_TopologyCategories[] = {
    &KSCATEGORY_AUDIO,
    &KSCATEGORY_TOPOLOGY,
};

static const PCFILTER_DESCRIPTOR g_TopologyFilterDescriptor = {
    1, // Version
    0, // Flags
    NULL, // AutomationTable
    sizeof(PCPIN_DESCRIPTOR),
    RTL_NUMBER_OF(g_TopologyPins),
    g_TopologyPins,
    sizeof(PCNODE_DESCRIPTOR),
    RTL_NUMBER_OF(g_TopologyNodes),
    g_TopologyNodes,
    sizeof(PCCONNECTION_DESCRIPTOR),
    RTL_NUMBER_OF(g_TopologyConnections),
    g_TopologyConnections,
    RTL_NUMBER_OF(g_TopologyCategories),
    g_TopologyCategories,
};

STDMETHODIMP_(NTSTATUS) TopologyMiniport::GetDescription(_Out_ PPCFILTER_DESCRIPTOR* OutFilterDescriptor)
{
    if (OutFilterDescriptor == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *OutFilterDescriptor = const_cast<PPCFILTER_DESCRIPTOR>(&g_TopologyFilterDescriptor);
    return STATUS_SUCCESS;
}

} // namespace

_Use_decl_annotations_
extern "C" NTSTATUS NTAPI CreateMiniportTopology(PUNKNOWN* Unknown, REFCLSID RefClassId, PUNKNOWN OuterUnknown, POOL_TYPE PoolType)
{
    UNREFERENCED_PARAMETER(RefClassId);

    if (Unknown == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Unknown = NULL;

    TopologyMiniport* miniport = new (PoolType) TopologyMiniport(OuterUnknown);
    if (miniport == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    // PortCls follows COM-style rules: when aggregated, return the non-delegating
    // IUnknown to the outer object.
    if (OuterUnknown != NULL) {
        *Unknown = miniport->GetUnknownForCreate();
    } else {
        *Unknown = static_cast<IMiniportTopology*>(miniport);
    }

    return STATUS_SUCCESS;
}
