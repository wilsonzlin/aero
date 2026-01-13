mod common;

use std::sync::Arc;

use aero_d3d11::runtime::aerogpu_cmd_executor::AerogpuD3d11Executor;
use aero_d3d11::runtime::expansion_scratch::{ExpansionScratchAllocator, ExpansionScratchDescriptor};

#[test]
fn expansion_scratch_offsets_are_disjoint_across_frames_and_wrap() {
    pollster::block_on(async {
        let exec = match AerogpuD3d11Executor::new_for_tests().await {
            Ok(exec) => exec,
            Err(e) => {
                common::skip_or_panic(module_path!(), &format!("wgpu unavailable ({e:#})"));
                return;
            }
        };

        let device = exec.device();

        let mut scratch = ExpansionScratchAllocator::new(ExpansionScratchDescriptor {
            label: Some("expansion_scratch test"),
            frames_in_flight: 3,
            // Pick a small segment size, but large enough that 256-byte alignment still allows a few
            // allocations per frame on typical adapters.
            per_frame_size: 1024,
            ..ExpansionScratchDescriptor::default()
        });

        let a0 = scratch.alloc_vertex_output(device, 16).unwrap();
        let b0 = scratch.alloc_index_output(device, 4).unwrap();
        let c0 = scratch.alloc_indirect_draw(device).unwrap();

        let per_frame = scratch.per_frame_capacity().expect("allocator initialized");

        // All allocations should reside in the first segment.
        for alloc in [&a0, &b0, &c0] {
            assert!(
                alloc.offset < per_frame,
                "allocation {:?} must be inside segment0 (per_frame_capacity={per_frame})",
                alloc
            );
        }
        assert!(
            a0.offset + a0.size <= b0.offset || b0.offset + b0.size <= a0.offset,
            "allocations must not overlap within a frame"
        );

        // Frame 1 allocations should be in the next segment.
        scratch.begin_frame();
        let a1 = scratch.alloc_vertex_output(device, 16).unwrap();
        assert!(Arc::ptr_eq(&a0.buffer, &a1.buffer), "backing buffer must be reused");
        assert!(
            a1.offset >= per_frame && a1.offset < per_frame * 2,
            "frame1 allocation offset must be inside segment1"
        );

        // Frame 2 allocations should be in the next segment.
        scratch.begin_frame();
        let a2 = scratch.alloc_vertex_output(device, 16).unwrap();
        assert!(Arc::ptr_eq(&a0.buffer, &a2.buffer), "backing buffer must be reused");
        assert!(
            a2.offset >= per_frame * 2 && a2.offset < per_frame * 3,
            "frame2 allocation offset must be inside segment2"
        );

        // Wrap back to segment0 and ensure offsets reset.
        scratch.begin_frame();
        let a0b = scratch.alloc_vertex_output(device, 16).unwrap();
        assert!(Arc::ptr_eq(&a0.buffer, &a0b.buffer), "backing buffer must be reused");
        assert!(
            a0b.offset < per_frame,
            "wrapped allocation must be inside segment0"
        );
        assert_eq!(
            a0b.offset, 0,
            "wrapped segment must have been reset (expected offset 0)"
        );
    });
}

