use aero_devices_gpu::executor::{
    AeroGpuExecutor, AeroGpuExecutorConfig, AeroGpuFenceCompletionMode,
};
use aero_devices_gpu::regs::{irq_bits, ring_control, AeroGpuRegs};
use aero_devices_gpu::ring::{
    AeroGpuRingHeader, AeroGpuSubmitDesc, AEROGPU_FENCE_PAGE_MAGIC, AEROGPU_RING_HEADER_SIZE_BYTES,
    AEROGPU_RING_MAGIC, FENCE_PAGE_COMPLETED_FENCE_OFFSET, FENCE_PAGE_MAGIC_OFFSET,
    RING_HEAD_OFFSET, RING_TAIL_OFFSET,
};
use aero_protocol::aerogpu::aerogpu_cmd::AEROGPU_PRESENT_FLAG_VSYNC;
use aero_protocol::aerogpu::cmd_writer::AerogpuCmdWriter;
use memory::{Bus, MemoryBus};

fn write_ring_header(
    mem: &mut dyn MemoryBus,
    gpa: u64,
    entry_count: u32,
    head: u32,
    tail: u32,
    abi_version: u32,
) -> u32 {
    let stride = AeroGpuSubmitDesc::SIZE_BYTES;
    let size_bytes = u32::try_from(
        AEROGPU_RING_HEADER_SIZE_BYTES + u64::from(entry_count) * u64::from(stride),
    )
    .expect("ring size fits u32");

    mem.write_u32(gpa, AEROGPU_RING_MAGIC);
    mem.write_u32(gpa + 4, abi_version);
    mem.write_u32(gpa + 8, size_bytes);
    mem.write_u32(gpa + 12, entry_count);
    mem.write_u32(gpa + 16, stride);
    mem.write_u32(gpa + 20, 0);
    mem.write_u32(gpa + RING_HEAD_OFFSET, head);
    mem.write_u32(gpa + RING_TAIL_OFFSET, tail);

    size_bytes
}

fn write_submit_desc(
    mem: &mut dyn MemoryBus,
    gpa: u64,
    cmd_gpa: u64,
    cmd_size_bytes: u32,
    signal_fence: u64,
    flags: u32,
) {
    mem.write_u32(gpa, AeroGpuSubmitDesc::SIZE_BYTES);
    mem.write_u32(gpa + 4, flags);
    mem.write_u32(gpa + 8, 0); // context_id
    mem.write_u32(gpa + 12, 0); // engine_id
    mem.write_u64(gpa + 16, cmd_gpa);
    mem.write_u32(gpa + 24, cmd_size_bytes);
    mem.write_u32(gpa + 28, 0); // cmd_reserved0
    mem.write_u64(gpa + 32, 0); // alloc_table_gpa
    mem.write_u32(gpa + 40, 0); // alloc_table_size_bytes
    mem.write_u32(gpa + 44, 0); // alloc_table_reserved0
    mem.write_u64(gpa + 48, signal_fence);
    mem.write_u64(gpa + 56, 0); // reserved0
}

#[test]
fn vsync_present_fence_does_not_complete_until_vblank_tick() {
    let mut mem = Bus::new(0x10_000);
    let ring_gpa = 0x1000u64;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;
    let signal_fence = 1u64;

    // Build a command stream containing a PRESENT with the VSYNC flag.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    let mut regs = AeroGpuRegs::default();
    regs.scanout0.enable = true;
    regs.ring_gpa = ring_gpa;
    regs.ring_control = ring_control::ENABLE;
    regs.fence_gpa = fence_gpa;
    regs.irq_enable = irq_bits::FENCE;

    let entry_count = 8u32;
    regs.ring_size_bytes = write_ring_header(
        &mut mem,
        ring_gpa,
        entry_count,
        /*head=*/ 0,
        /*tail=*/ 1,
        regs.abi_version,
    );

    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 0 * stride;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    exec.process_doorbell(&mut regs, &mut mem);

    // Doorbell must not complete vsynced presents: the fence should remain pending.
    assert_eq!(regs.completed_fence, 0);
    assert_eq!(regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET), 0);

    // Ring head should advance to match tail.
    let ring = AeroGpuRingHeader::read_from(&mut mem, ring_gpa);
    assert_eq!(ring.head, 1);
    assert_eq!(ring.tail, 1);

    // On vblank, the vsync fence becomes eligible and completes.
    exec.process_vblank_tick(&mut regs, &mut mem);

    assert_eq!(regs.completed_fence, signal_fence);
    assert_ne!(regs.irq_status & irq_bits::FENCE, 0);
    assert_eq!(
        mem.read_u32(fence_gpa + FENCE_PAGE_MAGIC_OFFSET),
        AEROGPU_FENCE_PAGE_MAGIC
    );
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        signal_fence
    );
}

#[test]
fn pending_vsync_fence_is_flushed_when_scanout_is_disabled() {
    let mut mem = Bus::new(0x10_000);
    let ring_gpa = 0x1000u64;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;
    let signal_fence = 1u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    let mut regs = AeroGpuRegs::default();
    regs.scanout0.enable = true;
    regs.ring_gpa = ring_gpa;
    regs.ring_control = ring_control::ENABLE;
    regs.fence_gpa = fence_gpa;
    regs.irq_enable = irq_bits::FENCE;

    let entry_count = 8u32;
    regs.ring_size_bytes = write_ring_header(
        &mut mem,
        ring_gpa,
        entry_count,
        /*head=*/ 0,
        /*tail=*/ 1,
        regs.abi_version,
    );

    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);
    let desc_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 0 * stride;
    write_submit_desc(
        &mut mem,
        desc_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        signal_fence,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 0);

    // If scanout/vblank pacing is disabled after a vsync present is queued, do not leave the fence
    // blocked forever. The device should flush/publish the completion.
    regs.scanout0.enable = false;
    exec.flush_pending_fences(&mut regs, &mut mem);

    assert_eq!(regs.completed_fence, signal_fence);
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        signal_fence
    );
}

#[test]
fn vsync_fence_blocks_immediate_fences_behind_it_until_vblank() {
    let mut mem = Bus::new(0x10_000);
    let ring_gpa = 0x1000u64;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;

    // Vsync present stream for the first submission.
    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    let mut regs = AeroGpuRegs::default();
    regs.scanout0.enable = true;
    regs.ring_gpa = ring_gpa;
    regs.ring_control = ring_control::ENABLE;
    regs.fence_gpa = fence_gpa;
    regs.irq_enable = irq_bits::FENCE;

    // Two submissions: fence 1 (vsync) then fence 2 (immediate, empty submission).
    regs.ring_size_bytes = write_ring_header(
        &mut mem,
        ring_gpa,
        /*entry_count=*/ 8,
        /*head=*/ 0,
        /*tail=*/ 2,
        regs.abi_version,
    );
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 0 * stride;
    write_submit_desc(
        &mut mem,
        desc0_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        1,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );

    let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 1 * stride;
    // Empty submission (no cmd stream) should be treated as immediate.
    write_submit_desc(&mut mem, desc1_gpa, 0, 0, 2, 0);

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(
        regs.completed_fence, 0,
        "immediate fence behind vsync fence must not complete on doorbell"
    );

    exec.process_vblank_tick(&mut regs, &mut mem);

    // Completing the vsync fence should also allow the immediate fence behind it to complete on
    // the same vblank tick.
    assert_eq!(regs.completed_fence, 2);
    assert_eq!(
        mem.read_u64(fence_gpa + FENCE_PAGE_COMPLETED_FENCE_OFFSET),
        2
    );
}

#[test]
fn completes_at_most_one_vsync_fence_per_vblank_tick() {
    let mut mem = Bus::new(0x10_000);
    let ring_gpa = 0x1000u64;
    let cmd_gpa = 0x2000u64;
    let fence_gpa = 0x3000u64;

    let mut writer = AerogpuCmdWriter::new();
    writer.present(0, AEROGPU_PRESENT_FLAG_VSYNC);
    let cmd_stream = writer.finish();
    mem.write_physical(cmd_gpa, &cmd_stream);

    let mut regs = AeroGpuRegs::default();
    regs.scanout0.enable = true;
    regs.ring_gpa = ring_gpa;
    regs.ring_control = ring_control::ENABLE;
    regs.fence_gpa = fence_gpa;
    regs.irq_enable = irq_bits::FENCE;

    regs.ring_size_bytes = write_ring_header(
        &mut mem,
        ring_gpa,
        /*entry_count=*/ 8,
        /*head=*/ 0,
        /*tail=*/ 2,
        regs.abi_version,
    );
    let stride = u64::from(AeroGpuSubmitDesc::SIZE_BYTES);

    let desc0_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 0 * stride;
    write_submit_desc(
        &mut mem,
        desc0_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        1,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );

    let desc1_gpa = ring_gpa + AEROGPU_RING_HEADER_SIZE_BYTES + 1 * stride;
    write_submit_desc(
        &mut mem,
        desc1_gpa,
        cmd_gpa,
        cmd_stream.len() as u32,
        2,
        AeroGpuSubmitDesc::FLAG_PRESENT,
    );

    let mut exec = AeroGpuExecutor::new(AeroGpuExecutorConfig {
        verbose: false,
        keep_last_submissions: 0,
        fence_completion: AeroGpuFenceCompletionMode::Immediate,
    });

    exec.process_doorbell(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 0);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 1);

    exec.process_vblank_tick(&mut regs, &mut mem);
    assert_eq!(regs.completed_fence, 2);
}
