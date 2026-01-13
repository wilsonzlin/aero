#![cfg(not(target_arch = "wasm32"))]

use emulator::io::net::trace::{FrameDirection, NetTraceConfig, NetTracer};

fn count_blocks_of_type(mut bytes: &[u8], want_type: u32) -> usize {
    let mut count = 0usize;
    while !bytes.is_empty() {
        assert!(bytes.len() >= 12, "pcapng truncated");
        let block_type = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let total_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert!(total_len >= 12, "pcapng invalid block length");
        assert!(
            total_len.is_multiple_of(4),
            "pcapng block length not 32-bit aligned"
        );
        assert!(bytes.len() >= total_len, "pcapng truncated block");
        assert_eq!(
            &bytes[total_len - 4..total_len],
            &(total_len as u32).to_le_bytes(),
            "pcapng footer length mismatch",
        );

        if block_type == want_type {
            count += 1;
        }
        bytes = &bytes[total_len..];
    }
    count
}

#[test]
fn drops_records_when_max_bytes_exceeded_and_clear_resets_counters() {
    let tracer = NetTracer::new(NetTraceConfig {
        // Two 14-byte Ethernet frames should overflow.
        max_bytes: 20,
        ..NetTraceConfig::default()
    });
    tracer.enable();

    let frame = [0u8; 14];
    tracer.record_ethernet(FrameDirection::GuestTx, &frame);
    tracer.record_ethernet(FrameDirection::GuestTx, &frame);

    let stats = tracer.stats();
    assert!(stats.enabled);
    assert_eq!(stats.records, 1);
    assert_eq!(stats.bytes, 14);
    assert_eq!(stats.dropped_records, 1);
    assert_eq!(stats.dropped_bytes, 14);

    tracer.clear();
    let stats = tracer.stats();
    assert_eq!(stats.records, 0);
    assert_eq!(stats.bytes, 0);
    assert_eq!(stats.dropped_records, 0);
    assert_eq!(stats.dropped_bytes, 0);
}

#[test]
fn export_pcapng_has_section_header_block() {
    let tracer = NetTracer::new(NetTraceConfig::default());
    tracer.enable();
    tracer.record_ethernet_at(1, FrameDirection::GuestTx, &[0u8; 14]);

    let bytes = tracer.export_pcapng();
    assert!(!bytes.is_empty(), "pcapng should not be empty");
    assert!(bytes.len() >= 4);
    let block_type = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    assert_eq!(block_type, 0x0A0D0D0A);
}

#[test]
fn take_pcapng_drains_buffered_records_but_keeps_drop_counters() {
    let tracer = NetTracer::new(NetTraceConfig {
        // Only one Ethernet frame fits.
        max_bytes: 20,
        ..NetTraceConfig::default()
    });
    tracer.enable();

    let frame = [0u8; 14];
    tracer.record_ethernet_at(1, FrameDirection::GuestTx, &frame);
    tracer.record_ethernet_at(2, FrameDirection::GuestTx, &frame);

    let before = tracer.stats();
    assert_eq!(before.records, 1);
    assert_eq!(before.bytes, 14);
    assert_eq!(before.dropped_records, 1);
    assert_eq!(before.dropped_bytes, 14);

    let taken = tracer.take_pcapng();
    assert!(count_blocks_of_type(&taken, 0x0000_0006) >= 1);

    // `take_pcapng()` drains records and resets the live bytes counter, but keeps the drop counters.
    let after_take = tracer.stats();
    assert_eq!(after_take.records, 0);
    assert_eq!(after_take.bytes, 0);
    assert_eq!(after_take.dropped_records, before.dropped_records);
    assert_eq!(after_take.dropped_bytes, before.dropped_bytes);

    let exported = tracer.export_pcapng();
    assert_eq!(count_blocks_of_type(&exported, 0x0000_0006), 0);
}
