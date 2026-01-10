#![cfg(target_arch = "wasm32")]

mod util;

use aero_d3d9::resources::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test(async)]
async fn managed_texture_budget_enforced_via_eviction() {
    // Each 128x128 BGRA8 texture ~= 64KiB. Create enough to exceed the budget by a wide margin.
    let budget = 256 * 1024;

    let mut rm = util::init_manager_with_options(ResourceManagerOptions {
        texture_budget_bytes: Some(budget),
        upload_staging_capacity_bytes: 4 * 1024 * 1024,
    })
    .await;

    rm.begin_frame();

    let ids: Vec<u32> = (0..32).map(|i| 10_000 + i).collect();
    for id in &ids {
        rm.create_texture(
            *id,
            TextureDesc {
                kind: TextureKind::Texture2D {
                    width: 128,
                    height: 128,
                    levels: 1,
                },
                format: D3DFormat::A8R8G8B8,
                pool: D3DPool::Managed,
                usage: TextureUsageKind::Sampled,
            },
        )
        .unwrap();
    }

    // Newly created textures are considered "recent" for a couple of frames; advance frames so
    // they're eligible for eviction and let `begin_frame()` enforce the budget.
    rm.begin_frame();
    rm.begin_frame();

    let mut total_resident_bytes = 0usize;
    let mut evicted = 0usize;
    let mut resident = 0usize;

    for id in &ids {
        match rm.texture(*id).unwrap().gpu_bytes() {
            Some(bytes) => {
                total_resident_bytes += bytes;
                resident += 1;
            }
            None => evicted += 1,
        }
    }

    assert!(evicted > 0, "expected eviction to happen");
    assert!(
        total_resident_bytes <= budget,
        "resident bytes {} exceeded budget {} (resident {}, evicted {})",
        total_resident_bytes,
        budget,
        resident,
        evicted
    );

    // Ensure accessing an evicted texture recreates GPU backing without panicking.
    let evicted_id = ids
        .iter()
        .copied()
        .find(|id| rm.texture(*id).unwrap().gpu_bytes().is_none())
        .expect("evicted texture id");

    let _view = rm.texture_view(evicted_id).unwrap();

    let mut encoder = rm
        .device()
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    rm.encode_uploads(&mut encoder);
    rm.submit(encoder);
}

