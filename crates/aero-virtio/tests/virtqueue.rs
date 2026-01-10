use aero_virtio::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestRam};
use aero_virtio::queue::{
    VirtQueue, VirtQueueConfig, VirtQueueError, VIRTQ_AVAIL_F_NO_INTERRUPT, VIRTQ_DESC_F_INDIRECT,
    VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};

fn write_desc(mem: &mut GuestRam, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).unwrap();
    write_u32_le(mem, base + 8, len).unwrap();
    write_u16_le(mem, base + 12, flags).unwrap();
    write_u16_le(mem, base + 14, next).unwrap();
}

#[test]
fn descriptor_chaining_is_parsed() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;

    write_desc(&mut mem, desc, 0, 0x4000, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut mem, desc, 1, 0x5000, 8, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap(); // flags
    write_u16_le(&mut mem, avail + 2, 1).unwrap(); // idx
    write_u16_le(&mut mem, avail + 4, 0).unwrap(); // ring[0] = head 0

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        false,
    )
    .unwrap();

    let chain = q.pop_descriptor_chain(&mem).unwrap().unwrap();
    assert_eq!(chain.head_index(), 0);
    assert_eq!(chain.descriptors().len(), 2);
    assert_eq!(chain.descriptors()[0].addr, 0x4000);
    assert_eq!(chain.descriptors()[1].addr, 0x5000);
    assert!(chain.descriptors()[1].is_write_only());
}

#[test]
fn indirect_descriptors_are_expanded() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    let indirect = 0x8000;

    write_desc(&mut mem, desc, 0, indirect, 32, VIRTQ_DESC_F_INDIRECT, 0);
    // indirect table has 2 descriptors.
    write_desc(&mut mem, indirect, 0, 0x4000, 4, VIRTQ_DESC_F_NEXT, 1);
    write_desc(&mut mem, indirect, 1, 0x5000, 4, VIRTQ_DESC_F_WRITE, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        false,
    )
    .unwrap();

    let chain = q.pop_descriptor_chain(&mem).unwrap().unwrap();
    assert_eq!(chain.head_index(), 0);
    assert_eq!(chain.descriptors().len(), 2);
    assert_eq!(chain.descriptors()[0].addr, 0x4000);
    assert_eq!(chain.descriptors()[1].addr, 0x5000);
}

#[test]
fn nested_indirect_descriptors_are_rejected() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;
    let indirect = 0x8000;

    write_desc(&mut mem, desc, 0, indirect, 16, VIRTQ_DESC_F_INDIRECT, 0);
    // Nested indirect inside the indirect table should be rejected.
    write_desc(&mut mem, indirect, 0, 0x9000, 16, VIRTQ_DESC_F_INDIRECT, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        false,
    )
    .unwrap();

    let err = q.pop_descriptor_chain(&mem).unwrap_err();
    assert_eq!(err, VirtQueueError::NestedIndirectDescriptor);
}

#[test]
fn ring_index_wraparound_uses_modulo_queue_size() {
    let mut mem = GuestRam::new(0x20000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;

    // Four trivial descriptors.
    for i in 0..4 {
        write_desc(&mut mem, desc, i, 0x4000 + u64::from(i) * 0x10, 1, 0, 0);
    }

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        false,
    )
    .unwrap();

    // Post 4 buffers (ring indices 0..3).
    write_u16_le(&mut mem, avail + 2, 4).unwrap();
    for i in 0..4 {
        write_u16_le(&mut mem, avail + 4 + u64::from(i) * 2, i).unwrap();
    }
    for _ in 0..4 {
        q.pop_descriptor_chain(&mem).unwrap().unwrap();
    }

    // Reuse descriptor 0, which should be read from ring index 0 after wrap.
    write_u16_le(&mut mem, avail + 4, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 5).unwrap();
    let chain = q.pop_descriptor_chain(&mem).unwrap().unwrap();
    assert_eq!(chain.head_index(), 0);
}

#[test]
fn no_interrupt_flag_suppresses_interrupts() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;

    write_u16_le(&mut mem, avail, VIRTQ_AVAIL_F_NO_INTERRUPT).unwrap();
    write_u16_le(&mut mem, avail + 2, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        false,
    )
    .unwrap();

    assert!(!q.add_used(&mut mem, 0, 0).unwrap());
    write_u16_le(&mut mem, avail, 0).unwrap();
    assert!(q.add_used(&mut mem, 0, 0).unwrap());
}

#[test]
fn event_idx_controls_when_interrupts_are_raised() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 0).unwrap();
    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();
    // used_event lives after the avail ring.
    write_u16_le(&mut mem, avail + 4 + 4 * 2, 1).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        true,
    )
    .unwrap();

    assert!(!q.add_used(&mut mem, 0, 0).unwrap());
    assert!(q.add_used(&mut mem, 0, 0).unwrap());
}

#[test]
fn event_idx_used_event_is_updated_for_driver_notifications() {
    let mut mem = GuestRam::new(0x10000);
    let desc = 0x1000;
    let avail = 0x2000;
    let used = 0x3000;

    write_desc(&mut mem, desc, 0, 0x4000, 4, 0, 0);

    write_u16_le(&mut mem, avail, 0).unwrap();
    write_u16_le(&mut mem, avail + 2, 1).unwrap();
    write_u16_le(&mut mem, avail + 4, 0).unwrap();

    write_u16_le(&mut mem, used, 0).unwrap();
    write_u16_le(&mut mem, used + 2, 0).unwrap();

    let mut q = VirtQueue::new(
        VirtQueueConfig {
            size: 4,
            desc_addr: desc,
            avail_addr: avail,
            used_addr: used,
        },
        true,
    )
    .unwrap();

    q.pop_descriptor_chain(&mem).unwrap().unwrap();
    q.update_used_event(&mut mem).unwrap();

    let used_event_addr = avail + 4 + 4 * 2;
    let used_event = read_u16_le(&mem, used_event_addr).unwrap();
    assert_eq!(used_event, 1);
}

#[test]
fn descriptor_parsing_never_panics_on_garbage_guest_memory() {
    struct XorShift64(u64);

    impl XorShift64 {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    let mut rng = XorShift64(0x1234_5678_9abc_def0);
    for _ in 0..1_000 {
        let mut mem = GuestRam::new(0x20000);
        for chunk in mem.as_mut_slice().chunks_exact_mut(8) {
            chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
        }

        let mut q = VirtQueue::new(
            VirtQueueConfig {
                size: 8,
                desc_addr: 0x1000,
                avail_addr: 0x2000,
                used_addr: 0x3000,
            },
            true,
        )
        .unwrap();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = q.pop_descriptor_chain(&mem);
        }));

        assert!(result.is_ok());
    }
}
