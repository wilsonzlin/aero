#pragma once

#include "../include/aerogpu_d3d9_umd.h"

namespace aerogpu {

struct Adapter;

HRESULT get_caps(Adapter* adapter, const AEROGPU_D3D9DDIARG_GETCAPS* pGetCaps);
HRESULT query_adapter_info(Adapter* adapter, const AEROGPU_D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo);

} // namespace aerogpu

