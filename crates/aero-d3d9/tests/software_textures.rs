use aero_d3d9::software::{Texture1D, Texture3D, Vec4};
use aero_d3d9::state::{AddressMode, FilterMode, SamplerState};

fn sampler_point(
    address_u: AddressMode,
    address_v: AddressMode,
    address_w: AddressMode,
) -> SamplerState {
    SamplerState {
        min_filter: FilterMode::Point,
        mag_filter: FilterMode::Point,
        address_u,
        address_v,
        address_w,
    }
}

fn sampler_linear(
    address_u: AddressMode,
    address_v: AddressMode,
    address_w: AddressMode,
) -> SamplerState {
    SamplerState {
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        address_u,
        address_v,
        address_w,
    }
}

#[test]
fn texture1d_addressing_clamp_vs_wrap() {
    let tex = Texture1D {
        width: 4,
        texels: (0..4).map(|i| Vec4::new(i as f32, 0.0, 0.0, 1.0)).collect(),
    };

    let clamp = sampler_point(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);
    let wrap = sampler_point(AddressMode::Wrap, AddressMode::Clamp, AddressMode::Clamp);

    assert_eq!(tex.sample(clamp, -0.25).x, 0.0);
    assert_eq!(tex.sample(clamp, 1.25).x, 3.0);

    // -0.25 wraps to 0.75 -> fx=2.25 -> point snaps to texel 2.
    assert_eq!(tex.sample(wrap, -0.25).x, 2.0);
    // 1.25 wraps to 0.25 -> fx=0.75 -> point snaps to texel 1.
    assert_eq!(tex.sample(wrap, 1.25).x, 1.0);
}

#[test]
fn texture1d_point_vs_linear_filtering() {
    let tex = Texture1D {
        width: 4,
        texels: (0..4).map(|i| Vec4::new(i as f32, 0.0, 0.0, 1.0)).collect(),
    };

    let point = sampler_point(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);
    let linear = sampler_linear(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);

    // u=0.5 -> fx=1.5. Point snaps to texel 2, linear interpolates between 1 and 2.
    assert_eq!(tex.sample(point, 0.5).x, 2.0);
    assert_eq!(tex.sample(linear, 0.5).x, 1.5);
}

#[test]
fn texture3d_addressing_clamp_vs_wrap_u_axis() {
    let mut texels = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let v = (x + y * 2 + z * 4) as f32;
                texels.push(Vec4::new(v, 0.0, 0.0, 1.0));
            }
        }
    }
    let tex = Texture3D {
        width: 2,
        height: 2,
        depth: 2,
        texels,
    };

    let clamp = sampler_point(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);
    let wrap = sampler_point(AddressMode::Wrap, AddressMode::Clamp, AddressMode::Clamp);

    assert_eq!(tex.sample(clamp, (-0.25, 0.0, 0.0)).x, 0.0);
    assert_eq!(tex.sample(wrap, (-0.25, 0.0, 0.0)).x, 1.0);
}

#[test]
fn texture3d_point_vs_linear_filtering() {
    let mut texels = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                let v = (x + y * 2 + z * 4) as f32;
                texels.push(Vec4::new(v, 0.0, 0.0, 1.0));
            }
        }
    }
    let tex = Texture3D {
        width: 2,
        height: 2,
        depth: 2,
        texels,
    };

    let point = sampler_point(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);
    let linear = sampler_linear(AddressMode::Clamp, AddressMode::Clamp, AddressMode::Clamp);

    // Center of a 2x2x2 volume:
    // - Point snaps to (1,1,1) -> value 7.
    // - Trilinear averages all 8 texels -> 3.5.
    assert_eq!(tex.sample(point, (0.5, 0.5, 0.5)).x, 7.0);
    assert_eq!(tex.sample(linear, (0.5, 0.5, 0.5)).x, 3.5);
}
