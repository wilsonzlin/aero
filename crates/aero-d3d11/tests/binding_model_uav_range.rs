use aero_d3d11::binding_model::{
    BINDING_BASE_CBUFFER, BINDING_BASE_INTERNAL, BINDING_BASE_SAMPLER, BINDING_BASE_TEXTURE,
    BINDING_BASE_UAV, BIND_GROUP_INTERNAL_EMULATION, D3D11_MAX_CONSTANT_BUFFER_SLOTS,
    MAX_CBUFFER_SLOTS, MAX_SAMPLER_SLOTS, MAX_TEXTURE_SLOTS, MAX_UAV_SLOTS,
};

const _: () = {
    // Keep this test explicit: these numbers are part of the shared binding-model contract.
    assert!(BINDING_BASE_CBUFFER == 0);
    assert!(BINDING_BASE_TEXTURE == 32);
    assert!(BINDING_BASE_SAMPLER == 160);
    assert!(BINDING_BASE_UAV == 176);

    assert!(MAX_CBUFFER_SLOTS == 32);
    assert!(D3D11_MAX_CONSTANT_BUFFER_SLOTS == 14);
    const { assert!(D3D11_MAX_CONSTANT_BUFFER_SLOTS <= MAX_CBUFFER_SLOTS) };
    assert!(MAX_TEXTURE_SLOTS == 128);
    assert!(MAX_SAMPLER_SLOTS == 16);
    assert!(MAX_UAV_SLOTS == 8);
    assert!(BINDING_BASE_INTERNAL == 256);
    assert!(BIND_GROUP_INTERNAL_EMULATION == 3);

    // Ensure the max binding of each range is strictly below the start of the next range.
    let max_cb_binding = BINDING_BASE_CBUFFER + MAX_CBUFFER_SLOTS - 1;
    assert!(max_cb_binding < BINDING_BASE_TEXTURE);

    let max_tex_binding = BINDING_BASE_TEXTURE + MAX_TEXTURE_SLOTS - 1;
    assert!(max_tex_binding < BINDING_BASE_SAMPLER);

    let max_sampler_binding = BINDING_BASE_SAMPLER + MAX_SAMPLER_SLOTS - 1;
    assert!(max_sampler_binding < BINDING_BASE_UAV);

    // Internal bindings must not overlap the D3D11 register-space ranges.
    let max_uav_binding = BINDING_BASE_UAV + MAX_UAV_SLOTS - 1;
    assert!(max_uav_binding < BINDING_BASE_INTERNAL);

    // Inclusive max slot is MAX_UAV_SLOTS - 1.
    let max_uav_slot = MAX_UAV_SLOTS - 1;
    let max_uav_binding = BINDING_BASE_UAV + max_uav_slot;

    // This should land within the UAV binding range and not overlap samplers.
    let max_sampler_binding = BINDING_BASE_SAMPLER + MAX_SAMPLER_SLOTS - 1;
    assert!(max_sampler_binding < BINDING_BASE_UAV);
    assert!(max_uav_binding >= BINDING_BASE_UAV);
    assert!(max_uav_binding == BINDING_BASE_UAV + MAX_UAV_SLOTS - 1);
};
