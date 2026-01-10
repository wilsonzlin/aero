use aero_emulator::devices::aerogpu::{
    CmdCreateSurface, CmdHeader, CmdPresent, Opcode, RingBuffer, RingPushError, SurfaceFormat,
};

fn decode_opcode(bytes: &[u8]) -> u32 {
    CmdHeader::decode(bytes).expect("valid header").opcode
}

fn decode_present_surface_id(bytes: &[u8]) -> u32 {
    assert_eq!(decode_opcode(bytes), Opcode::PRESENT);
    CmdPresent::decode(&bytes[CmdHeader::SIZE_BYTES..])
        .expect("present payload")
        .surface_id
}

#[test]
fn ring_wraps_with_padding_nop() {
    let (ring, prod, cons) = RingBuffer::new(64).split();
    let _ = ring; // keep alive

    let cmd1 = CmdPresent { surface_id: 1 }.encode();
    let cmd2 = CmdPresent { surface_id: 2 }.encode();
    let cmd3 = CmdPresent { surface_id: 3 }.encode();

    prod.try_push(&cmd1).unwrap();
    prod.try_push(&cmd2).unwrap();
    prod.try_push(&cmd3).unwrap();

    assert_eq!(
        decode_present_surface_id(&cons.try_pop().unwrap().unwrap()),
        1
    );
    assert_eq!(
        decode_present_surface_id(&cons.try_pop().unwrap().unwrap()),
        2
    );

    // At this point tail is near the end; the next command will straddle the ring end and force
    // a padding NOP.
    let cmd4 = CmdCreateSurface {
        width: 1,
        height: 1,
        format: SurfaceFormat::Rgba8888 as u32,
    }
    .encode();
    prod.try_push(&cmd4).unwrap();

    assert_eq!(
        decode_present_surface_id(&cons.try_pop().unwrap().unwrap()),
        3
    );
    let popped = cons.try_pop().unwrap().unwrap();
    assert_eq!(decode_opcode(&popped), Opcode::CREATE_SURFACE);
}

#[test]
fn ring_reports_full() {
    let (_ring, prod, cons) = RingBuffer::new(32).split();

    // PRESENT is a 16-byte command. Two of them fill a 32-byte ring.
    let cmd = CmdPresent { surface_id: 1 }.encode();
    prod.try_push(&cmd).unwrap();
    prod.try_push(&cmd).unwrap();

    // Third push should fail.
    assert_eq!(prod.try_push(&cmd), Err(RingPushError::Full));

    // Pop one and ensure we can push again.
    let _ = cons.try_pop().unwrap().unwrap();
    prod.try_push(&cmd).unwrap();
}
