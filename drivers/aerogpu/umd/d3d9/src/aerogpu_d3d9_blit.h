#pragma once

#include "../include/aerogpu_d3d9_umd.h"

namespace aerogpu {

struct Device;
struct Resource;

// Implements D3D9 compositor-critical blit paths (StretchRect/Blt, ColorFill,
// UpdateSurface/UpdateTexture) using the AeroGPU command stream.
//
// All functions expect the caller to hold `Device::mutex`.
HRESULT blit_locked(Device* dev,
                    Resource* dst,
                    const RECT* dst_rect,
                    Resource* src,
                    const RECT* src_rect,
                    uint32_t filter);

HRESULT color_fill_locked(Device* dev, Resource* dst, const RECT* dst_rect, uint32_t color_argb);

HRESULT update_surface_locked(Device* dev,
                              Resource* src,
                              const RECT* src_rect,
                              Resource* dst,
                              const RECT* dst_rect);

HRESULT update_texture_locked(Device* dev, Resource* src, Resource* dst);

// Frees built-in blit resources owned by the device.
void destroy_blit_objects_locked(Device* dev);

} // namespace aerogpu

