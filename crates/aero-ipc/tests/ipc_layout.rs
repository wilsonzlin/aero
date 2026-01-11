use aero_ipc::ipc::{
    create_ipc_buffer, find_queue_by_kind, parse_ipc_buffer, IpcLayoutError, IpcQueueSpec,
};
use aero_ipc::layout::{ipc_header, queue_desc, queue_kind, ring_ctrl};

fn write_u32_le(buf: &mut [u8], offset: usize, v: u32) {
    buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn queue_kind_values_are_stable() {
    assert_eq!(queue_kind::CMD, 0);
    assert_eq!(queue_kind::EVT, 1);
    assert_eq!(queue_kind::NET_TX, 2);
    assert_eq!(queue_kind::NET_RX, 3);
}

#[test]
fn parses_ipc_layout_and_finds_queues_by_kind() {
    let specs = &[
        IpcQueueSpec {
            kind: 0,
            capacity_bytes: 64,
        },
        IpcQueueSpec {
            kind: 1,
            capacity_bytes: 128,
        },
        IpcQueueSpec {
            kind: 0,
            capacity_bytes: 256,
        },
    ];

    let bytes = create_ipc_buffer(specs);
    let layout = parse_ipc_buffer(&bytes).expect("parse");
    assert_eq!(layout.total_bytes, bytes.len());
    assert_eq!(layout.queues.len(), specs.len());

    // Offsets are deterministic for the fixed layout algorithm.
    assert_eq!(layout.queues[0].kind, 0);
    assert_eq!(layout.queues[0].offset_bytes, 64);
    assert_eq!(layout.queues[0].capacity_bytes, 64);

    assert_eq!(layout.queues[1].kind, 1);
    assert_eq!(layout.queues[1].offset_bytes, 144);
    assert_eq!(layout.queues[1].capacity_bytes, 128);

    assert_eq!(layout.queues[2].kind, 0);
    assert_eq!(layout.queues[2].offset_bytes, 288);
    assert_eq!(layout.queues[2].capacity_bytes, 256);

    let q0 = find_queue_by_kind(&layout, 0, 0).expect("kind 0 nth 0");
    assert_eq!(q0.offset_bytes, 64);
    let q0_1 = find_queue_by_kind(&layout, 0, 1).expect("kind 0 nth 1");
    assert_eq!(q0_1.offset_bytes, 288);
    assert!(find_queue_by_kind(&layout, 0, 2).is_none());

    let q1 = find_queue_by_kind(&layout, 1, 0).expect("kind 1 nth 0");
    assert_eq!(q1.offset_bytes, 144);
}

#[test]
fn rejects_corrupt_header_fields() {
    let specs = &[IpcQueueSpec {
        kind: 0,
        capacity_bytes: 64,
    }];
    let bytes = create_ipc_buffer(specs);

    // Bad magic.
    let mut bad_magic = bytes.clone();
    write_u32_le(&mut bad_magic, ipc_header::MAGIC * 4, 0);
    assert!(matches!(
        parse_ipc_buffer(&bad_magic),
        Err(IpcLayoutError::BadMagic { .. })
    ));

    // Unsupported version.
    let mut bad_version = bytes.clone();
    write_u32_le(&mut bad_version, ipc_header::VERSION * 4, 999);
    assert!(matches!(
        parse_ipc_buffer(&bad_version),
        Err(IpcLayoutError::UnsupportedVersion { .. })
    ));
}

#[test]
fn rejects_reserved_and_capacity_mismatches() {
    let specs = &[
        IpcQueueSpec {
            kind: 0,
            capacity_bytes: 64,
        },
        IpcQueueSpec {
            kind: 1,
            capacity_bytes: 128,
        },
    ];
    let bytes = create_ipc_buffer(specs);

    // Reserved field must be 0.
    let mut bad_reserved = bytes.clone();
    let desc0_base = ipc_header::BYTES + 0 * queue_desc::BYTES;
    write_u32_le(&mut bad_reserved, desc0_base + queue_desc::RESERVED * 4, 1);
    assert!(matches!(
        parse_ipc_buffer(&bad_reserved),
        Err(IpcLayoutError::QueueReservedNotZero { index: 0, .. })
    ));

    // Ring header capacity must match descriptor capacity.
    let mut bad_ring_cap = bytes.clone();
    let q1_offset_base = ipc_header::BYTES + 1 * queue_desc::BYTES;
    let q1_offset_bytes = u32::from_le_bytes(
        bad_ring_cap[q1_offset_base + queue_desc::OFFSET_BYTES * 4
            ..q1_offset_base + queue_desc::OFFSET_BYTES * 4 + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    write_u32_le(
        &mut bad_ring_cap,
        q1_offset_bytes + ring_ctrl::CAPACITY * 4,
        64,
    );
    assert!(matches!(
        parse_ipc_buffer(&bad_ring_cap),
        Err(IpcLayoutError::RingHeaderCapacityMismatch { index: 1, .. })
    ));
}
