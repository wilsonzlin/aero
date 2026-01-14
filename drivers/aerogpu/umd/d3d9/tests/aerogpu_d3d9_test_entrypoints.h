// Test-only declarations for host-side AeroGPU D3D9 entrypoints.
//
// The D3D9 UMD keeps most DDI implementations in an anonymous namespace and
// exposes them only via the D3D9DDI_* function tables populated by
// OpenAdapter/CreateDevice. Portable host-side unit tests compile the driver
// directly and call a small subset of those DDIs via thin wrappers defined in
// `src/aerogpu_d3d9_driver.cpp` (under "Host-side test entrypoints").
//
// Keep these declarations in one place so tests don't need to duplicate local
// prototypes (which is easy to get wrong with stdcall/ABI differences).
#pragma once

#include <cstdint>

#include "aerogpu_d3d9_umd.h"
#include "aerogpu_wddm_alloc_list.h"

namespace aerogpu {

// -----------------------------------------------------------------------------
// Host-side DDI wrappers
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL device_set_fvf(D3DDDI_HDEVICE hDevice, uint32_t fvf);

HRESULT AEROGPU_D3D9_CALL device_set_stream_source(
    D3DDDI_HDEVICE hDevice,
    uint32_t stream,
    D3DDDI_HRESOURCE hVb,
    uint32_t offset_bytes,
    uint32_t stride_bytes);

HRESULT AEROGPU_D3D9_CALL device_set_indices(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hIb,
    D3DDDIFORMAT fmt,
    uint32_t offset_bytes);

HRESULT AEROGPU_D3D9_CALL device_set_stream_source_freq(
    D3DDDI_HDEVICE hDevice,
    uint32_t stream,
    uint32_t value);

HRESULT AEROGPU_D3D9_CALL device_set_viewport(D3DDDI_HDEVICE hDevice, const D3DDDIVIEWPORTINFO* pViewport);

HRESULT AEROGPU_D3D9_CALL device_set_transform(
    D3DDDI_HDEVICE hDevice,
    D3DTRANSFORMSTATETYPE state,
    const D3DMATRIX* pMatrix);

HRESULT AEROGPU_D3D9_CALL device_create_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    const void* pDecl,
    uint32_t decl_size,
    D3D9DDI_HVERTEXDECL* phDecl);

HRESULT AEROGPU_D3D9_CALL device_set_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HVERTEXDECL hDecl);

HRESULT AEROGPU_D3D9_CALL device_destroy_vertex_decl(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HVERTEXDECL hDecl);

HRESULT AEROGPU_D3D9_CALL device_create_shader(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    const void* pBytecode,
    uint32_t bytecode_size,
    D3D9DDI_HSHADER* phShader);

HRESULT AEROGPU_D3D9_CALL device_set_shader(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    D3D9DDI_HSHADER hShader);

HRESULT AEROGPU_D3D9_CALL device_destroy_shader(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSHADER hShader);

HRESULT AEROGPU_D3D9_CALL device_set_texture_stage_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value);

HRESULT AEROGPU_D3D9_CALL device_set_material(D3DDDI_HDEVICE hDevice, const D3DMATERIAL9* pMaterial);

HRESULT AEROGPU_D3D9_CALL device_set_light(D3DDDI_HDEVICE hDevice, uint32_t index, const D3DLIGHT9* pLight);

HRESULT AEROGPU_D3D9_CALL device_light_enable(D3DDDI_HDEVICE hDevice, uint32_t index, BOOL enabled);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    int32_t base_vertex,
    uint32_t min_index,
    uint32_t num_vertices,
    uint32_t start_index,
    uint32_t primitive_count);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t primitive_count,
    const void* pVertexData,
    uint32_t stride_bytes);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive_up(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t min_vertex_index,
    uint32_t num_vertices,
    uint32_t primitive_count,
    const void* pIndexData,
    D3DDDIFORMAT index_data_format,
    const void* pVertexData,
    uint32_t stride_bytes);

HRESULT AEROGPU_D3D9_CALL device_draw_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWPRIMITIVE2* pDraw);

HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive2(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_DRAWINDEXEDPRIMITIVE2* pDraw);

HRESULT AEROGPU_D3D9_CALL device_process_vertices(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_PROCESSVERTICES* pProcessVertices);

// -----------------------------------------------------------------------------
// Test-only helpers
// -----------------------------------------------------------------------------

HRESULT AEROGPU_D3D9_CALL device_test_set_cursor_hw_active(D3DDDI_HDEVICE hDevice, BOOL active);

HRESULT AEROGPU_D3D9_CALL device_test_set_unmaterialized_user_shaders(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSHADER user_vs,
    D3D9DDI_HSHADER user_ps);

HRESULT AEROGPU_D3D9_CALL device_test_enable_wddm_context(D3DDDI_HDEVICE hDevice);

HRESULT AEROGPU_D3D9_CALL device_test_rebind_alloc_list_tracker(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_ALLOCATIONLIST* pAllocationList,
    uint32_t allocation_list_capacity,
    uint32_t max_allocation_list_slot_id);

HRESULT AEROGPU_D3D9_CALL device_test_reset_alloc_list_tracker(D3DDDI_HDEVICE hDevice);

AllocRef AEROGPU_D3D9_CALL device_test_track_buffer_read(
    D3DDDI_HDEVICE hDevice,
    WddmAllocationHandle hAllocation,
    uint32_t alloc_id,
    uint64_t share_token);

AllocRef AEROGPU_D3D9_CALL device_test_track_texture_read(
    D3DDDI_HDEVICE hDevice,
    WddmAllocationHandle hAllocation,
    uint32_t alloc_id,
    uint64_t share_token);

AllocRef AEROGPU_D3D9_CALL device_test_track_render_target_write(
    D3DDDI_HDEVICE hDevice,
    WddmAllocationHandle hAllocation,
    uint32_t alloc_id,
    uint64_t share_token);

HRESULT AEROGPU_D3D9_CALL device_test_force_umd_private_features(D3DDDI_HDEVICE hDevice, uint64_t device_features);

HRESULT AEROGPU_D3D9_CALL adapter_test_set_completed_fence(D3DDDI_HADAPTER hAdapter, uint64_t completed_fence);

HRESULT AEROGPU_D3D9_CALL device_test_set_resource_backing(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hResource,
    uint32_t backing_alloc_id,
    uint32_t backing_offset_bytes,
    WddmAllocationHandle wddm_hAllocation);

HRESULT AEROGPU_D3D9_CALL device_test_set_resource_share_token(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hResource,
    uint64_t share_token);

HRESULT AEROGPU_D3D9_CALL device_test_set_resource_shared_private_driver_data(
    D3DDDI_HDEVICE hDevice,
    D3DDDI_HRESOURCE hResource,
    const void* data,
    uint32_t data_size);

HRESULT AEROGPU_D3D9_CALL device_test_alias_fixedfunc_stage0_ps_variant(
    D3DDDI_HDEVICE hDevice,
    uint32_t src_index,
    uint32_t dst_index);

HRESULT AEROGPU_D3D9_CALL device_test_force_device_lost(D3DDDI_HDEVICE hDevice, HRESULT hr);

} // namespace aerogpu
