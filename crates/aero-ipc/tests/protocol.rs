use aero_ipc::protocol::*;

#[test]
fn command_roundtrip() {
    let cases = vec![
        Command::Nop { seq: 123 },
        Command::Shutdown,
        Command::MmioRead {
            id: 1,
            addr: 0xFEE0_0000,
            size: 4,
        },
        Command::MmioWrite {
            id: 2,
            addr: 0xFED0_0000,
            data: vec![1, 2, 3, 4, 5],
        },
        Command::PortRead {
            id: 3,
            port: 0x0060,
            size: 1,
        },
        Command::PortWrite {
            id: 4,
            port: 0x0064,
            size: 1,
            value: 0xAA,
        },
    ];

    for cmd in cases {
        let bytes = encode_command(&cmd);
        let decoded = decode_command(&bytes).expect("decode");
        assert_eq!(decoded, cmd);
    }
}

#[test]
fn event_roundtrip() {
    let cases = vec![
        Event::Ack { seq: 42 },
        Event::MmioReadResp {
            id: 9,
            data: vec![0xAA, 0xBB],
        },
        Event::PortReadResp { id: 10, value: 0x1234_5678 },
        Event::MmioWriteResp { id: 11 },
        Event::PortWriteResp { id: 12 },
        Event::FrameReady { frame_id: 999 },
        Event::IrqRaise { irq: 5 },
        Event::IrqLower { irq: 5 },
        Event::A20Set { enabled: true },
        Event::ResetRequest,
        Event::Log {
            level: LogLevel::Info,
            message: "hello".to_string(),
        },
        Event::SerialOutput {
            port: 0x3F8,
            data: b"Hi".to_vec(),
        },
        Event::Panic {
            message: "oh no".to_string(),
        },
        Event::TripleFault,
    ];

    for evt in cases {
        let bytes = encode_event(&evt);
        let decoded = decode_event(&bytes).expect("decode");
        assert_eq!(decoded, evt);
    }
}

#[test]
fn decode_rejects_unknown_tag() {
    let err = decode_command(&[0xFF, 0xFF]).unwrap_err();
    assert_eq!(err, DecodeError::UnknownTag);
}
